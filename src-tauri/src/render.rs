//! Renders one clip document into a vertical MP4 with a single ffmpeg pass:
//! range cut + concat, face-centred crop, B-roll overlays, burned captions and hook title.

use crate::captions::{self, FontInfo};
use crate::face;
use crate::ffmpeg::{self, round_even, Probe};
use crate::models::{ClipDoc, Settings, Transcript};
use crate::state::State;
use anyhow::{bail, Result};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub struct RenderCtx<'a> {
    pub state: &'a State,
    pub settings: &'a Settings,
    pub encoder: &'a str,
    pub source: &'a Path,
    pub probe: &'a Probe,
    pub transcript: &'a Transcript,
    pub fonts: &'a [FontInfo],
}

pub fn output_size(settings: &Settings) -> (i64, i64) {
    let h = if settings.output_height <= 1280 { 1280 } else { 1920 };
    (h * 9 / 16, h)
}

pub async fn render_clip(
    ctx: &RenderCtx<'_>,
    clip_id: &str,
    doc: &mut ClipDoc,
    out: &Path,
    cancel: &AtomicBool,
    mut on: impl FnMut(f64),
) -> Result<f64> {
    // Subtitles burned into the source are cut off first; everything after sees the shorter picture.
    let cropped_probe;
    let (p, keep_h) = match doc.sub_crop.filter(|k| *k > 0.5 && *k < 0.99) {
        Some(k) if ctx.probe.height > 2 => {
            let h = round_even(ctx.probe.height as f64 * k).clamp(2, ctx.probe.height);
            cropped_probe = Probe { height: h, ..ctx.probe.clone() };
            (&cropped_probe, Some(h))
        }
        _ => (ctx.probe, None),
    };
    doc.ranges.retain(|r| r[1] - r[0] > 0.05);
    for r in doc.ranges.iter_mut() {
        r[0] = r[0].max(0.0);
        if p.duration > 0.0 {
            r[1] = r[1].min(p.duration);
        }
    }
    let total = doc.duration();
    if doc.ranges.is_empty() || total < 0.3 {
        bail!("This clip has no length.");
    }
    if !p.has_video || p.width <= 0 || p.height <= 0 {
        bail!("The source has no video track.");
    }
    let (out_w, out_h) = output_size(ctx.settings);
    let n = doc.ranges.len();

    let aspect = p.width as f64 / p.height as f64;
    // Gaming split: gameplay on top, the streamer's face below, captions between. Needs a landscape source with a facecam.
    let mut gaming = doc.layout == "gaming" && aspect > 9.0 / 16.0 + 0.01;
    if gaming {
        if doc.facecam.is_none() {
            doc.facecam = Some(face::find_facecam(ctx.state, ctx.source, ctx.probe, &doc.ranges, cancel).await);
        }
        gaming = doc.facecam.as_ref().map(facecam_usable).unwrap_or(false);
    }
    // A "facecam" that fills the frame is a talking head, not an overlay: follow the speaker instead of showing the face twice.
    let talking_head = doc.layout == "gaming" && !gaming && doc.facecam.as_ref().map(|f| f.found).unwrap_or(false);
    // Cinematic: the whole picture in a band, big captions below. Used for trailers and anything without a facecam.
    let speaking = doc.captions && !captions::clip_words(ctx.transcript, doc, false).words.is_empty();
    let mut cinema = (doc.layout == "cinema" || (doc.layout == "gaming" && !gaming && !talking_head)) && aspect > 0.6;
    if cinema {
        if doc.bars.is_none() {
            doc.bars = Some(detect_bars(ctx.state, ctx.source, p, &doc.ranges).await.unwrap_or([0, 0, 0, 0]));
        }
    }
    if let (Some(h), Some(b)) = (keep_h, doc.bars) {
        // Bars were measured on the whole frame: keep them inside the cropped picture.
        if b[1] > 0 && b[3] + b[1] > h {
            let nh = (h - b[3]) & !1;
            doc.bars = Some(if nh > 2 { [b[0], nh, b[2], b[3]] } else { [0, 0, 0, 0] });
        }
    }
    if gaming {
        cinema = false;
    }
    if !gaming && !cinema && doc.crop.is_none() {
        doc.crop = Some(face::crop_plan(ctx.state, ctx.source, ctx.probe, &doc.ranges, cancel).await);
    }
    let plan = doc.crop.clone().unwrap_or_default();

    let mut args: Vec<String> = Vec::new();
    for r in &doc.ranges {
        args.extend(["-ss".into(), format!("{:.3}", r[0]), "-t".into(), format!("{:.3}", r[1] - r[0]), "-i".into(), ctx.source.to_string_lossy().into()]);
    }
    let broll: Vec<_> = doc.broll.iter().filter(|b| Path::new(&b.path).is_file() && b.start < total - 0.5).cloned().collect();
    for b in &broll {
        args.extend(["-stream_loop".into(), "-1".into(), "-t".into(), format!("{:.3}", b.duration), "-i".into(), b.path.clone()]);
    }

    // Outro: appended after the clip when switched on (per clip, else the global setting).
    let mut outro: Option<(usize, f64)> = None;
    if doc.outro.unwrap_or(ctx.settings.outro_enabled) {
        match crate::outro::ensure(ctx.state, ctx.settings, cancel).await {
            Ok((path, secs)) => {
                args.extend(["-i".into(), path.to_string_lossy().into()]);
                outro = Some((n + broll.len(), secs));
            }
            Err(e) => bail!("{e}"),
        }
    }
    let fps = if p.fps > 1.0 && p.fps <= 60.5 { p.fps } else { 30.0 };
    let fps = if ctx.settings.fps_30 { fps.min(30.0) } else { fps };
    let mut fc: Vec<String> = Vec::new();
    // --- video: cut + join
    if n == 1 {
        fc.push("[0:v]setpts=PTS-STARTPTS[vc0]".into());
    } else {
        let mut labels = String::new();
        // Junction style: a quick dip through black or white keeps the timeline (and captions) intact.
        let dip = match doc.transition.as_deref() { Some("dip") => Some("black"), Some("flash") => Some("white"), _ => None };
        for i in 0..n {
            let len = doc.ranges[i][1] - doc.ranges[i][0];
            let fade = match dip {
                Some(color) if len > 0.8 => {
                    let mut f = String::new();
                    if i > 0 { f.push_str(&format!(",fade=t=in:st=0:d=0.14:color={color}")); }
                    if i + 1 < n { f.push_str(&format!(",fade=t=out:st={:.3}:d=0.14:color={color}", len - 0.14)); }
                    f
                }
                _ => String::new(),
            };
            fc.push(format!("[{i}:v]setpts=PTS-STARTPTS,setsar=1{fade}[v{i}]"));
            labels.push_str(&format!("[v{i}]"));
        }
        fc.push(format!("{labels}concat=n={n}:v=1:a=0[vc0]"));
    }
    fc.push(match keep_h { Some(h) => format!("[vc0]crop=iw:{h}:0:0[vc]"), None => "[vc0]null[vc]".to_string() });
    // --- crop to 9:16 around the speaker, then scale
    let target = 9.0 / 16.0;
    if gaming {
        let mut fcam = doc.facecam.clone().unwrap_or_default();
        if let Some(h) = keep_h {
            // The face was found in the whole frame; the picture is now shorter.
            let k = h as f64 / ctx.probe.height as f64;
            fcam.cy /= k;
            fcam.h /= k;
        }
        fc.push(gaming_graph(p, fps, out_w, out_h, &fcam));
    } else if cinema {
        fc.push(cinema_layout(p, doc.bars, out_w, out_h, speaking).graph(fps, out_w, out_h));
    } else {
    let mut chain = format!("fps={fps:.3}");
    if aspect > target + 0.01 {
        let cw = round_even(p.height as f64 * target).clamp(2, p.width);
        let x = face::crop_x_expr(&plan, p.width, cw);
        chain.push_str(&format!(",crop={cw}:{}:'{x}':0", p.height));
    } else if aspect < target - 0.01 {
        let ch = round_even(p.width as f64 / target).clamp(2, p.height);
        chain.push_str(&format!(",crop={}:{ch}:0:{}", p.width, (p.height - ch) / 2));
    }
    if ctx.settings.ken_burns && plan.segments.len() <= 1 {
        // Slow ~5% punch-in on static shots.
        let d = total.max(0.1);
        chain.push_str(&format!(
            ",scale=w='2*trunc({out_w}*(1+0.05*min(t/{d:.3},1))/2)':h='2*trunc({out_h}*(1+0.05*min(t/{d:.3},1))/2)':eval=frame:flags=bilinear,crop={out_w}:{out_h}"
        ));
    } else {
        chain.push_str(&format!(",scale={out_w}:{out_h}:flags=bicubic"));
    }
    chain.push_str(",setsar=1");
    fc.push(format!("[vc]{chain}[vs]"));
    }
    let mut cur = "vs".to_string();
    // --- B-roll cutaways
    for (j, b) in broll.iter().enumerate() {
        let k = n + j;
        let end = (b.start + b.duration).min(total);
        fc.push(format!(
            "[{k}:v]scale={out_w}:{out_h}:force_original_aspect_ratio=increase,crop={out_w}:{out_h},setsar=1,fps={fps:.3},setpts=PTS-STARTPTS+{:.3}/TB[b{j}]",
            b.start
        ));
        fc.push(format!("[{cur}][b{j}]overlay=enable='between(t,{:.3},{:.3})':eof_action=pass[o{j}]", b.start, end));
        cur = format!("o{j}");
    }
    // --- captions + hook title
    let cw = captions::clip_words(ctx.transcript, doc, false);
    let words: &[crate::models::Word] = if doc.captions { &cw.words } else { &[] };
    let hook = if doc.show_hook { doc.hook_title.as_deref() } else { None };
    let ass_path = ctx.state.data_dir.join("work").join(format!("{clip_id}.ass"));
    if !words.is_empty() || hook.is_some() {
        let ass = captions::build_ass(&captions::AssInput {
            words,
            breaks: &cw.breaks,
            hook_title: hook,
            template: captions::template(if (gaming || cinema) && doc.template == "default" { "gaming" } else { &doc.template }),
            font_family: doc.font_family.as_deref(),
            font_size: doc.font_size,
            font_color: doc.font_color.as_deref(),
            width: out_w,
            height: out_h,
            duration: total,
            fonts: ctx.fonts,
            emoji: false, // libass in ffmpeg cannot draw colour emoji; they come out as boxes
            pos_y: if gaming { Some(GAME_SEAM) } else if cinema { Some(cinema_layout(p, doc.bars, out_w, out_h, speaking).caption_y) } else { None },
            title_top: if gaming { Some(0.035) } else if cinema { Some(0.06) } else { None },
            title_always: gaming || cinema,
        });
        std::fs::write(&ass_path, ass)?;
        fc.push(format!("[{cur}]subtitles={}:fontsdir=fonts[vout]", ffmpeg::rel(ctx.state, &ass_path)));
        cur = "vout".into();
    }
    if let Some((k, _)) = outro {
        fc.push(format!("[{cur}]fps={fps:.3},setsar=1,format=yuv420p[mainv]"));
        fc.push(format!("[{k}:v]scale={out_w}:{out_h}:flags=bicubic,fps={fps:.3},setsar=1,format=yuv420p[outv]"));
        fc.push("[mainv][outv]concat=n=2:v=1:a=0[vfinal]".into());
        cur = "vfinal".into();
    }
    // --- audio
    let mut audio_label = None;
    if p.has_audio {
        let mut tail = String::new();
        if ctx.settings.loudnorm {
            tail.push_str(",loudnorm=I=-14:TP=-1.5:LRA=11");
        }
        tail.push_str(",aresample=48000");
        if n == 1 {
            fc.push(format!("[0:a:0]asetpts=PTS-STARTPTS{tail}[aout]"));
        } else {
            let mut labels = String::new();
            for (i, r) in doc.ranges.iter().enumerate() {
                let d = r[1] - r[0];
                // Tiny fades hide the click at each cut.
                fc.push(format!("[{i}:a:0]asetpts=PTS-STARTPTS,afade=t=in:d=0.015,afade=t=out:st={:.3}:d=0.015[a{i}]", (d - 0.015).max(0.0)));
                labels.push_str(&format!("[a{i}]"));
            }
            fc.push(format!("{labels}concat=n={n}:v=0:a=1{tail}[aout]"));
        }
        audio_label = Some("[aout]");
        if let Some((k, _)) = outro {
            fc.push(format!("[aout]aresample=48000,aformat=channel_layouts=stereo[am]"));
            fc.push(format!("[{k}:a:0]aresample=48000,aformat=channel_layouts=stereo[ao]"));
            fc.push("[am][ao]concat=n=2:v=0:a=1[afinal]".into());
            audio_label = Some("[afinal]");
        }
    }
    args.extend(["-filter_complex".into(), fc.join(";"), "-map".into(), format!("[{cur}]")]);
    if let Some(a) = audio_label {
        args.extend(["-map".into(), a.into()]);
        args.extend(ffmpeg::audio_encode_args("192k"));
    }
    args.extend(ffmpeg::video_encode_args(ctx.encoder, ctx.settings, None));
    let full = total + outro.map(|(_, s)| s).unwrap_or(0.0);
    args.extend(["-t".into(), format!("{full:.3}"), out.to_string_lossy().into()]);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = ffmpeg::run(ctx.state, args, cancel, |t| on((t / full).clamp(0.0, 1.0))).await;
    let _ = std::fs::remove_file(&ass_path);
    result?;
    Ok(full)
}

// Layout fractions of the output height (1080x1920 reference: game 265-1035, face 1040-1720).
const GAME_TOP: f64 = 265.0 / 1920.0;
const GAME_H: f64 = 770.0 / 1920.0;
const FACE_TOP: f64 = 1040.0 / 1920.0;
const FACE_H: f64 = 680.0 / 1920.0;
/// Caption centre: the seam between gameplay and face.
const GAME_SEAM: f64 = 1037.0 / 1920.0;

fn even(v: f64) -> i64 {
    round_even(v).max(2)
}

/// Crop `aspect` (w/h) out of a frame of sw x sh around a centre, clamped inside the frame. Returns (w, h, x, y).
fn crop_for(sw: i64, sh: i64, aspect: f64, want_h: f64, cx: f64, cy_top_frac: f64, cy: f64) -> (i64, i64, i64, i64) {
    let mut ch = want_h.min(sh as f64);
    let mut cw = ch * aspect;
    if cw > sw as f64 {
        cw = sw as f64;
        ch = cw / aspect;
    }
    let (cw, ch) = (even(cw), even(ch));
    let x = (cx - cw as f64 / 2.0).clamp(0.0, (sw - cw).max(0) as f64);
    let y = (cy - ch as f64 * cy_top_frac).clamp(0.0, (sh - ch).max(0) as f64);
    (cw, ch, (x as i64) & !1, (y as i64) & !1)
}

fn gaming_graph(p: &Probe, fps: f64, out_w: i64, out_h: i64, face: &crate::models::FaceCam) -> String {
    let (sw, sh) = (p.width, p.height);
    let (gy, gh) = (even(out_h as f64 * GAME_TOP), even(out_h as f64 * GAME_H));
    let (fy, fh) = (even(out_h as f64 * FACE_TOP), even(out_h as f64 * FACE_H));
    // Gameplay: the full frame width, trimmed on the sides to fit the band.
    let (gcw, gch, gcx, gcy) = crop_for(sw, sh, out_w as f64 / gh as f64, sh as f64, sw as f64 / 2.0, 0.5, sh as f64 / 2.0);
    // Face: head and shoulders around the detected face, face sitting a little above the middle.
    let face_px = (face.h * sh as f64).max(24.0);
    let (fcw, fch, fcx, fcy) = crop_for(sw, sh, out_w as f64 / fh as f64, face_px * 2.3, face.cx * sw as f64, 0.46, face.cy * sh as f64);
    let (bw, bh) = (even(out_w as f64 / 6.0), even(out_h as f64 / 6.0));
    let line = |y: i64| format!("drawbox=x=0:y={y}:w=iw:h=4:color=black@1:t=fill");
    format!(
        "[vc]fps={fps:.3},split=3[gA][gB][gC];\
[gA]scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh},boxblur=7:2,scale={out_w}:{out_h}:flags=bilinear,eq=brightness=-0.16:saturation=0.9[bgv];\
[gB]crop={gcw}:{gch}:{gcx}:{gcy},scale={out_w}:{gh}:flags=lanczos,eq=contrast=1.05:saturation=1.15,unsharp=5:5:0.7:5:5:0[gv];\
[gC]crop={fcw}:{fch}:{fcx}:{fcy},scale={out_w}:{fh}:flags=lanczos,hqdn3d=1.5:1.5:4:4,unsharp=5:5:0.8:5:5:0,eq=saturation=1.08[fv];\
[bgv][gv]overlay=0:{gy}[t1];[t1][fv]overlay=0:{fy},{},{},{},{},setsar=1[vs]",
        line(gy - 2), line(gy + gh - 2), line(fy - 2), line(fy + fh - 2)
    )
}

struct Cinema {
    bars: Option<[i64; 4]>,
    /// Crop of the (bar-less) frame: w, h, x, y
    crop: (i64, i64, i64, i64),
    band_h: i64,
    band_y: i64,
    caption_y: f64,
}

fn cinema_layout(p: &Probe, bars: Option<[i64; 4]>, out_w: i64, out_h: i64, speaking: bool) -> Cinema {
    let bars = bars.filter(|b| b[0] > 0 && b[1] > 0);
    let (sw, sh) = bars.map(|b| (b[0], b[1])).unwrap_or((p.width, p.height));
    let aspect = sw as f64 / sh as f64;
    // The band shows the full width; very tall pictures are trimmed to half the frame.
    // A bit taller than the natural picture, so widescreen trailers do not float in empty space (the sides are trimmed).
    // Without captions the picture can take more room and sit nearer the middle.
    let (grow, cap, centre) = if speaking { (1.3, 0.4, 0.44) } else { (1.5, 0.46, 0.5) };
    let band_h = even((out_w as f64 / aspect * grow).min(out_h as f64 * cap));
    let crop = crop_for(sw, sh, out_w as f64 / band_h as f64, sh as f64, sw as f64 / 2.0, 0.5, sh as f64 / 2.0);
    let band_y = even((out_h as f64 * centre - band_h as f64 / 2.0).max(out_h as f64 * 0.16));
    let caption_y = (band_y + band_h) as f64 / out_h as f64 + 0.085;
    Cinema { bars, crop, band_h, band_y, caption_y }
}

impl Cinema {
    fn graph(&self, fps: f64, out_w: i64, out_h: i64) -> String {
        let (cw, ch, cx, cy) = self.crop;
        let pre = self.bars.map(|b| format!(",crop={}:{}:{}:{}", b[0], b[1], b[2], b[3])).unwrap_or_default();
        let (bw, bh) = (even(out_w as f64 / 6.0), even(out_h as f64 / 6.0));
        let line = |y: i64| format!("drawbox=x=0:y={y}:w=iw:h=4:color=black@1:t=fill");
        format!(
            "[vc]fps={fps:.3}{pre},split=2[cA][cB];\
[cA]scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh},boxblur=7:2,scale={out_w}:{out_h}:flags=bilinear,eq=brightness=-0.18:saturation=1.0[bgv];\
[cB]crop={cw}:{ch}:{cx}:{cy},scale={out_w}:{bh2}:flags=lanczos,eq=contrast=1.06:saturation=1.18,unsharp=5:5:0.8:5:5:0[mv];\
[bgv][mv]overlay=0:{by},{l1},{l2},setsar=1[vs]",
            bh2 = self.band_h, by = self.band_y, l1 = line(self.band_y - 2), l2 = line(self.band_y + self.band_h - 2)
        )
    }
}

/// Letterbox bars: ask ffmpeg's cropdetect at three places and keep the union.
async fn detect_bars(state: &crate::state::State, source: &Path, p: &Probe, ranges: &[[f64; 2]]) -> Option<[i64; 4]> {
    let total: f64 = ranges.iter().map(|r| r[1] - r[0]).sum();
    let mut union: Option<(i64, i64, i64, i64)> = None; // x0, y0, x1, y1
    let re = regex::Regex::new(r"crop=(\d+):(\d+):(\d+):(\d+)").ok()?;
    for frac in [0.2, 0.5, 0.8] {
        let mut at = total * frac;
        let mut t = ranges[0][0];
        for r in ranges {
            let len = r[1] - r[0];
            if at <= len { t = r[0] + at; break; }
            at -= len;
        }
        let out = ffmpeg::command(ffmpeg::tool(state, "ffmpeg"))
            .args(["-hide_banner", "-nostdin", "-ss", &format!("{t:.2}"), "-t", "3", "-i"])
            .arg(source)
            .args(["-an", "-vf", "cropdetect=limit=24:round=2:reset=0", "-f", "null", "-"])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .output()
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&out.stderr);
        let Some(c) = re.captures_iter(&text).last() else { return None };
        let v: Vec<i64> = (1..=4).filter_map(|i| c[i].parse().ok()).collect();
        if v.len() != 4 { return None; }
        let (w, h, x, y) = (v[0], v[1], v[2], v[3]);
        union = Some(match union {
            None => (x, y, x + w, y + h),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h)),
        });
    }
    let (x0, y0, x1, y1) = union?;
    let (w, h) = ((x1 - x0) & !1, (y1 - y0) & !1);
    // Only worth cutting when the bars are real (more than 3% of the picture).
    if w <= 0 || h <= 0 || (w * h) as f64 > 0.97 * (p.width * p.height) as f64 {
        return None;
    }
    Some([w, h, x0 & !1, y0 & !1])
}


/// A webcam overlay is a small box (well under a third of the frame height); anything bigger is the whole picture.
pub fn facecam_usable(f: &crate::models::FaceCam) -> bool {
    f.found && f.h <= 0.28
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_full_frame_face_is_not_a_facecam_overlay() {
        use crate::models::FaceCam;
        assert!(super::facecam_usable(&FaceCam { cx: 0.88, cy: 0.14, h: 0.16, found: true }));
        assert!(!super::facecam_usable(&FaceCam { cx: 0.55, cy: 0.38, h: 0.39, found: true }));
        assert!(!super::facecam_usable(&FaceCam { cx: 0.5, cy: 0.5, h: 0.2, found: false }));
    }

    use super::*;

    fn probe(w: i64, h: i64) -> Probe { Probe { duration: 60.0, width: w, height: h, has_audio: true, has_video: true, fps: 60.0 } }

    #[test]
    fn crops_stay_inside_the_frame_and_even() {
        let (w, h, x, y) = crop_for(1920, 1080, 1080.0 / 680.0, 400.0, 1900.0, 0.46, 1000.0);
        assert!(x >= 0 && y >= 0 && x + w <= 1920 && y + h <= 1080, "{x},{y},{w},{h}");
        assert!(w % 2 == 0 && h % 2 == 0 && x % 2 == 0 && y % 2 == 0);
    }

    #[test]
    fn gaming_graph_has_all_pieces() {
        let g = gaming_graph(&probe(1920, 1080), 60.0, 1080, 1920, &crate::models::FaceCam { cx: 0.88, cy: 0.83, h: 0.12, found: true });
        for needle in ["[bgv]", "[gv]", "[fv]", "overlay=0:", "[vs]", "split=3"] {
            assert!(g.contains(needle), "missing {needle} in {g}");
        }
    }

    #[test]
    fn cinema_band_grows_without_captions_and_trims_bars() {
        let talk = cinema_layout(&probe(1920, 1080), Some([1920, 804, 0, 138]), 1080, 1920, true);
        let mute = cinema_layout(&probe(1920, 1080), Some([1920, 804, 0, 138]), 1080, 1920, false);
        assert!(mute.band_h >= talk.band_h);
        assert!(talk.caption_y > (talk.band_y + talk.band_h) as f64 / 1920.0);
        assert!(talk.graph(60.0, 1080, 1920).contains("crop=1920:804:0:138"));
    }

    #[test]
    fn even_rounding() {
        assert_eq!(even(1079.0), 1078);
        assert_eq!(even(1.0), 2);
    }
}
