//! Face-centred 9:16 framing. YuNet (the same ONNX model the original ships)
//! runs in pure Rust through tract; frames come from ffmpeg as raw BGR.

use crate::ffmpeg::{command, round_even, tool, Probe};
use crate::models::CropPlan;
use crate::state::State;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::io::AsyncReadExt;
use tract_onnx::prelude::*;

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

const INPUT: usize = 640;
const SCORE_THRESHOLD: f32 = 0.6;
const NMS_THRESHOLD: f32 = 0.3;

static MODEL: OnceLock<Option<Arc<Model>>> = OnceLock::new();

fn model(path: &Path) -> Option<Arc<Model>> {
    MODEL
        .get_or_init(|| {
            let load = || -> TractResult<Model> {
                tract_onnx::onnx()
                    .model_for_path(path)?
                    .with_input_fact(0, InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, INPUT, INPUT)))?
                    .into_optimized()?
                    .into_runnable()
            };
            match load() {
                Ok(m) => Some(Arc::new(m)),
                Err(e) => {
                    eprintln!("face model failed to load: {e:?}");
                    None
                }
            }
        })
        .clone()
}

/// (x, y, w, h, score) in frame pixels.
type FaceBox = (f32, f32, f32, f32, f32);

fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    inter / (a.2 * a.3 + b.2 * b.3 - inter).max(1e-6)
}

fn detect(model: &Model, bgr: &[u8], w: usize, h: usize) -> Result<Vec<FaceBox>> {
    let mut input = tract_ndarray::Array4::<f32>::zeros((1, 3, INPUT, INPUT));
    for y in 0..h.min(INPUT) {
        let row = &bgr[y * w * 3..(y + 1) * w * 3];
        for x in 0..w.min(INPUT) {
            for c in 0..3 {
                input[[0, c, y, x]] = row[x * 3 + c] as f32;
            }
        }
    }
    let tensor: Tensor = input.into();
    let outputs = model.run(tvec!(tensor.into())).map_err(|e| anyhow!("{e:?}"))?;
    // Outputs are cls/obj/bbox/kps for strides 8, 16, 32. Identify them by shape:
    // N anchors tells the stride, the channel count tells the kind.
    let mut scores: std::collections::HashMap<usize, Vec<f32>> = Default::default();
    let mut boxes: std::collections::HashMap<usize, Vec<f32>> = Default::default();
    for out in outputs.iter() {
        let view = out.to_array_view::<f32>().map_err(|e| anyhow!("{e:?}"))?;
        let shape = view.shape().to_vec();
        if shape.len() < 2 {
            continue;
        }
        let (n, c) = (shape[shape.len() - 2], shape[shape.len() - 1]);
        let data: Vec<f32> = view.iter().copied().collect();
        match c {
            1 => {
                let entry = scores.entry(n).or_insert_with(|| vec![1.0; n]);
                for (e, v) in entry.iter_mut().zip(data.iter()) {
                    *e *= v.clamp(0.0, 1.0);
                }
            }
            4 => {
                boxes.insert(n, data);
            }
            _ => {}
        }
    }
    let mut found: Vec<FaceBox> = Vec::new();
    for (n, score) in &scores {
        let Some(bbox) = boxes.get(n) else { continue };
        let cols = (*n as f64).sqrt().round() as usize;
        if cols == 0 || cols * cols != *n {
            continue;
        }
        let stride = (INPUT / cols) as f32;
        for idx in 0..*n {
            let s = score[idx].sqrt();
            if s < SCORE_THRESHOLD {
                continue;
            }
            let (row, col) = ((idx / cols) as f32, (idx % cols) as f32);
            let b = &bbox[idx * 4..idx * 4 + 4];
            let cx = (col + b[0]) * stride;
            let cy = (row + b[1]) * stride;
            let bw = b[2].exp() * stride;
            let bh = b[3].exp() * stride;
            found.push((cx - bw / 2.0, cy - bh / 2.0, bw, bh, s));
        }
    }
    found.sort_by(|a, b| b.4.total_cmp(&a.4));
    let mut kept: Vec<FaceBox> = Vec::new();
    for f in found {
        if kept.iter().all(|k| iou(k, &f) < NMS_THRESHOLD) {
            kept.push(f);
        }
    }
    Ok(kept)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

/// Turn per-sample face centres into a few steady shots with hard cuts
/// (a pan that follows every head movement looks seasick).
fn plan_from_samples(samples: Vec<Option<f64>>, fps: f64) -> CropPlan {
    if samples.iter().all(|s| s.is_none()) {
        return CropPlan { segments: vec![(0.0, 0.5)], faces_found: false };
    }
    let first = samples.iter().flatten().next().copied().unwrap_or(0.5);
    let mut filled: Vec<f64> = Vec::with_capacity(samples.len());
    let mut last = first;
    for s in &samples {
        if let Some(v) = s {
            last = *v;
        }
        filled.push(last);
    }
    // 3-tap median removes single-frame misdetections.
    let smooth: Vec<f64> = (0..filled.len())
        .map(|i| {
            let mut w = [filled[i.saturating_sub(1)], filled[i], filled[(i + 1).min(filled.len() - 1)]];
            median(&mut w)
        })
        .collect();
    let mut cuts: Vec<usize> = vec![0];
    let mut anchor = smooth[0];
    for i in 1..smooth.len() {
        let moved = (smooth[i] - anchor).abs() > 0.10;
        let settled = i + 1 >= smooth.len() || (smooth[i + 1] - smooth[i]).abs() < 0.06;
        let long_enough = (i - cuts[cuts.len() - 1]) as f64 / fps >= 1.2;
        if moved && settled && long_enough {
            cuts.push(i);
            anchor = smooth[i];
        }
    }
    let mut segments = Vec::new();
    for (k, &start) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).copied().unwrap_or(smooth.len());
        let mut slice = smooth[start..end].to_vec();
        segments.push((start as f64 / fps, median(&mut slice)));
    }
    CropPlan { segments, faces_found: true }
}

/// Sample the clip's ranges and decide where the 9:16 window sits over time.
pub async fn crop_plan(state: &State, source: &Path, probe: &Probe, ranges: &[[f64; 2]], cancel: &AtomicBool) -> CropPlan {
    let centre = CropPlan { segments: vec![(0.0, 0.5)], faces_found: false };
    // Nothing to track when the source is already as narrow as the output.
    if probe.width <= 0 || probe.height <= 0 || (probe.width as f64 / probe.height as f64) <= 9.0 / 16.0 + 0.01 {
        return centre;
    }
    let model_path = state.res_dir.join("models").join("face_detection_yunet_2023mar.onnx");
    let Some(model) = model(&model_path) else { return centre };
    let scale = INPUT as f64 / probe.width.max(probe.height) as f64;
    let tw = round_even(probe.width as f64 * scale).clamp(2, INPUT as i64) as usize;
    let th = round_even(probe.height as f64 * scale).clamp(2, INPUT as i64) as usize;
    let total: f64 = ranges.iter().map(|r| r[1] - r[0]).sum();
    let fps = if total > 90.0 { 1.0 } else { 2.0 };
    let frame_len = tw * th * 3;
    let mut samples: Vec<Option<f64>> = Vec::new();
    for r in ranges {
        if cancel.load(Ordering::Relaxed) {
            return centre;
        }
        let spawned = command(tool(state, "ffmpeg"))
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{:.3}", r[0]), "-t", &format!("{:.3}", r[1] - r[0]), "-i"])
            .arg(source)
            .args(["-an", "-vf", &format!("fps={fps},scale={tw}:{th}:flags=fast_bilinear"), "-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { return centre };
        let mut stdout = child.stdout.take().unwrap();
        let mut frames: Vec<Vec<u8>> = Vec::new();
        loop {
            let mut buf = vec![0u8; frame_len];
            if stdout.read_exact(&mut buf).await.is_err() {
                break;
            }
            frames.push(buf);
        }
        let _ = child.wait().await;
        let m = model.clone();
        let detected = tokio::task::spawn_blocking(move || {
            frames
                .iter()
                .map(|frame| {
                    let faces = detect(&m, frame, tw, th).unwrap_or_default();
                    // The subject is the biggest confident face.
                    faces
                        .iter()
                        .max_by(|a, b| (a.2 * a.3 * a.4).total_cmp(&(b.2 * b.3 * b.4)))
                        .map(|f| ((f.0 + f.2 / 2.0) / tw as f32).clamp(0.0, 1.0) as f64)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        // Keep the sample count aligned with the output timeline even if ffmpeg came up short.
        let expected = ((r[1] - r[0]) * fps).round().max(1.0) as usize;
        let mut detected = detected;
        detected.resize(expected, None);
        samples.extend(detected);
    }
    if samples.is_empty() {
        return centre;
    }
    plan_from_samples(samples, fps)
}

/// ffmpeg expression for the crop window's left edge over time.
pub fn crop_x_expr(plan: &CropPlan, src_w: i64, crop_w: i64) -> String {
    let max_x = (src_w - crop_w).max(0);
    let x_of = |centre: f64| ((centre * src_w as f64 - crop_w as f64 / 2.0).round() as i64).clamp(0, max_x);
    let segs = &plan.segments;
    if segs.len() <= 1 {
        return format!("{}", x_of(segs.first().map(|s| s.1).unwrap_or(0.5)));
    }
    // Nested if(lt(t,T1),X0,if(lt(t,T2),X1,...Xn))
    let mut expr = format!("{}", x_of(segs[segs.len() - 1].1));
    for k in (0..segs.len() - 1).rev() {
        expr = format!("if(lt(t,{:.3}),{},{})", segs[k + 1].0, x_of(segs[k].1), expr);
    }
    expr
}

/// Find a streamer's facecam: sample frames across the clip, look for faces in a 2x2 tiling of
/// a 1280-wide copy (so a small corner webcam is still big enough), and return the steadiest one.
pub async fn find_facecam(state: &State, source: &Path, probe: &Probe, ranges: &[[f64; 2]], cancel: &AtomicBool) -> crate::models::FaceCam {
    let none = crate::models::FaceCam { cx: 0.5, cy: 0.5, h: 0.2, found: false };
    if probe.width <= 0 || probe.height <= 0 {
        return none;
    }
    let model_path = state.res_dir.join("models").join("face_detection_yunet_2023mar.onnx");
    let Some(model) = model(&model_path) else { return none };
    let fw = 1280usize;
    let fh = (round_even(fw as f64 * probe.height as f64 / probe.width as f64).clamp(2, 1440)) as usize;
    let total: f64 = ranges.iter().map(|r| r[1] - r[0]).sum();
    let samples = 7usize;
    let mut times: Vec<f64> = Vec::new();
    for k in 0..samples {
        let mut at = total * (k as f64 + 0.5) / samples as f64;
        for r in ranges {
            let len = r[1] - r[0];
            if at <= len {
                times.push(r[0] + at);
                break;
            }
            at -= len;
        }
    }
    let mut hits: Vec<(f64, f64, f64, f32)> = Vec::new(); // cx, cy, h (fractions), score
    for t in times {
        if cancel.load(Ordering::Relaxed) {
            return none;
        }
        let out = command(tool(state, "ffmpeg"))
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{t:.3}"), "-i"])
            .arg(source)
            .args(["-frames:v", "1", "-vf", &format!("scale={fw}:{fh}:flags=bilinear"), "-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await;
        let Ok(out) = out else { return none };
        if out.stdout.len() < fw * fh * 3 {
            continue;
        }
        let frame = out.stdout;
        let m = model.clone();
        let found = tokio::task::spawn_blocking(move || {
            let (tw, th) = (fw / 2, (fh / 2).min(INPUT));
            let mut all: Vec<(f64, f64, f64, f32)> = Vec::new();
            for ty in 0..2usize {
                for tx in 0..2usize {
                    let mut tile = vec![0u8; tw * th * 3];
                    for y in 0..th {
                        let sy = ty * (fh / 2) + y;
                        let src = (sy * fw + tx * tw) * 3;
                        tile[y * tw * 3..(y + 1) * tw * 3].copy_from_slice(&frame[src..src + tw * 3]);
                    }
                    for f in detect(&m, &tile, tw, th).unwrap_or_default() {
                        let cx = (tx * tw) as f64 + (f.0 + f.2 / 2.0) as f64;
                        let cy = (ty * (fh / 2)) as f64 + (f.1 + f.3 / 2.0) as f64;
                        all.push((cx / fw as f64, cy / fh as f64, f.3 as f64 / fh as f64, f.4));
                    }
                }
            }
            all
        })
        .await
        .unwrap_or_default();
        // The facecam is the biggest confident face in this frame.
        if let Some(best) = found.into_iter().max_by(|a, b| (a.2 * a.3 as f64).total_cmp(&(b.2 * b.3 as f64))) {
            hits.push(best);
        }
    }
    if hits.len() < 2 {
        return none;
    }
    // Keep the detections that agree on a place; a moving subject (not a webcam) is rejected.
    let mut xs: Vec<f64> = hits.iter().map(|h| h.0).collect();
    let mut ys: Vec<f64> = hits.iter().map(|h| h.1).collect();
    let (mx, my) = (median(&mut xs), median(&mut ys));
    let mut agree: Vec<&(f64, f64, f64, f32)> = hits.iter().filter(|h| (h.0 - mx).abs() < 0.08 && (h.1 - my).abs() < 0.12).collect();
    if agree.len() < 2 || agree.len() * 2 < hits.len() {
        return none;
    }
    let mut hs: Vec<f64> = agree.drain(..).map(|h| h.2).collect();
    crate::models::FaceCam { cx: mx, cy: my, h: median(&mut hs), found: true }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_face_gives_one_shot_and_no_face_gives_centre() {
        let plan = plan_from_samples(vec![Some(0.4); 20], 2.0);
        assert_eq!(plan.segments.len(), 1);
        assert!(plan.faces_found);
        let none = plan_from_samples(vec![None; 10], 2.0);
        assert!(!none.faces_found && none.segments[0].1 == 0.5);
    }

    #[test]
    fn a_real_move_makes_a_cut_but_a_blip_does_not() {
        let mut moved = vec![Some(0.3); 12];
        moved.extend(vec![Some(0.7); 12]);
        assert!(plan_from_samples(moved, 2.0).segments.len() >= 2);
        let mut blip = vec![Some(0.3); 24];
        blip[10] = Some(0.9);
        assert_eq!(plan_from_samples(blip, 2.0).segments.len(), 1);
    }

    #[test]
    fn crop_expression_is_clamped() {
        let plan = CropPlan { segments: vec![(0.0, 0.0), (3.0, 1.0)], faces_found: true };
        let e = crop_x_expr(&plan, 1920, 608);
        assert!(e.contains("if(lt(t,3.000),0,1312)"), "{e}");
    }
}
