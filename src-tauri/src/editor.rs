//! Clip editing (trim, split, merge, captions) and exports (platform presets, ZIP).
//! Edits are non-destructive: they change the clip document and re-render from the source.

use crate::ffmpeg;
use crate::models::*;
use crate::state::{now, State};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Apply a JSON patch from the editor UI. Returns true when the picture changed
/// enough that framing has to be recomputed.
pub fn apply_patch(doc: &mut ClipDoc, patch: &Value, source_duration: f64) -> Result<()> {
    if let Some(ranges) = patch.get("ranges") {
        let parsed: Vec<[f64; 2]> = serde_json::from_value(ranges.clone()).map_err(|_| anyhow!("Invalid ranges"))?;
        let mut clean = Vec::new();
        for r in parsed {
            let s = r[0].max(0.0);
            let e = if source_duration > 0.0 { r[1].min(source_duration) } else { r[1] };
            if e - s < 0.3 {
                bail!("Each part of a clip must be at least 0.3 seconds long.");
            }
            clean.push([s, e]);
        }
        if clean.is_empty() {
            bail!("A clip needs at least one range.");
        }
        if clean != doc.ranges {
            doc.ranges = clean;
            doc.crop = None;
            doc.broll.clear();
        }
    }
    if let Some(v) = patch.get("hook_title") {
        doc.hook_title = v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    if let Some(v) = patch.get("template").and_then(|v| v.as_str()) {
        doc.template = v.to_string();
    }
    if let Some(v) = patch.get("font_family") {
        doc.font_family = v.as_str().map(String::from).filter(|s| !s.is_empty());
    }
    if let Some(v) = patch.get("font_size") {
        doc.font_size = v.as_u64().map(|n| n.clamp(12, 72) as u32);
    }
    if let Some(v) = patch.get("font_color") {
        doc.font_color = v.as_str().map(String::from).filter(|s| !s.is_empty());
    }
    if let Some(v) = patch.get("captions").and_then(|v| v.as_bool()) {
        doc.captions = v;
    }
    if let Some(v) = patch.get("transition") {
        doc.transition = v.as_str().map(String::from).filter(|s| s != "none" && !s.is_empty());
    }
    if let Some(v) = patch.get("layout").and_then(|v| v.as_str()) {
        if ["auto", "gaming", "cinema", "center"].contains(&v) && doc.layout != v {
            doc.layout = v.to_string();
            doc.crop = None;
        }
    }
    if let Some(v) = patch.get("outro") {
        doc.outro = match v.as_str() { Some("on") => Some(true), Some("off") => Some(false), _ => None };
    }
    if let Some(v) = patch.get("emoji").and_then(|v| v.as_bool()) {
        doc.emoji = v;
    }
    if let Some(v) = patch.get("show_hook").and_then(|v| v.as_bool()) {
        doc.show_hook = v;
    }
    if let Some(v) = patch.get("word_edits") {
        doc.word_edits = serde_json::from_value(v.clone()).map_err(|_| anyhow!("Invalid caption edits"))?;
    }
    if patch.get("reframe").and_then(|v| v.as_bool()) == Some(true) {
        doc.crop = None;
        doc.facecam = None;
        doc.bars = None;
    }
    if patch.get("remove_broll").and_then(|v| v.as_bool()) == Some(true) {
        doc.broll.clear();
    }
    Ok(())
}

/// Split a clip at `at` seconds on its own timeline into two documents.
pub fn split_doc(doc: &ClipDoc, at: f64) -> Result<(ClipDoc, ClipDoc)> {
    let total = doc.duration();
    if at < 0.5 || at > total - 0.5 {
        bail!("Pick a split point at least half a second from either end.");
    }
    let (mut first, mut second) = (Vec::new(), Vec::new());
    let mut offset = 0.0;
    for r in &doc.ranges {
        let len = r[1] - r[0];
        if at >= offset + len {
            first.push(*r);
        } else if at <= offset {
            second.push(*r);
        } else {
            let cut = r[0] + (at - offset);
            first.push([r[0], cut]);
            second.push([cut, r[1]]);
        }
        offset += len;
    }
    let make = |ranges: Vec<[f64; 2]>, keep_hook: bool| ClipDoc {
        ranges,
        crop: None,
        broll: vec![],
        hook_title: if keep_hook { doc.hook_title.clone() } else { None },
        ..doc.clone()
    };
    Ok((make(first, true), make(second, false)))
}

pub fn merge_docs(docs: &[ClipDoc]) -> Result<ClipDoc> {
    let first = docs.first().ok_or_else(|| anyhow!("Select at least two clips to merge."))?;
    if docs.len() < 2 {
        bail!("Select at least two clips to merge.");
    }
    let mut merged = ClipDoc { crop: None, broll: vec![], ranges: vec![], word_edits: Default::default(), ..first.clone() };
    for d in docs {
        merged.ranges.extend(d.ranges.iter().copied());
        merged.word_edits.extend(d.word_edits.clone());
    }
    merged.text = docs.iter().map(|d| d.text.as_str()).collect::<Vec<_>>().join(" ");
    merged.reasoning = "Merged clip".into();
    merged.role = None;
    merged.part_number = None;
    Ok(merged)
}

pub fn new_clip(task_id: &str, idx: i64, doc: ClipDoc) -> Clip {
    Clip {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        idx,
        doc,
        status: "rendering".into(),
        created_at: now(),
        ..Default::default()
    }
}

/// (width, height, video bitrate, audio bitrate)
fn preset(name: &str) -> Option<(i64, i64, &'static str, &'static str)> {
    match name {
        "tiktok" => Some((1080, 1920, "10M", "192k")),
        "reels" => Some((1080, 1920, "12M", "192k")),
        "shorts" => Some((1080, 1920, "10M", "192k")),
        _ => None,
    }
}

pub async fn export_preset(state: &State, clip: &Clip, preset_name: &str, dest: &Path) -> Result<()> {
    let (w, h, vb, ab) = preset(preset_name).ok_or_else(|| anyhow!("Unknown export preset: {preset_name}"))?;
    if !Path::new(&clip.file_path).is_file() {
        bail!("This clip has not been rendered yet.");
    }
    let settings = state.settings();
    let encoder = ffmpeg::resolve_encoder(state, &settings).await;
    let mut args: Vec<String> = vec![
        "-i".into(), clip.file_path.clone(),
        "-vf".into(), format!("scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"),
    ];
    args.extend(ffmpeg::video_encode_args(&encoder, &settings, Some(vb)));
    args.extend(ffmpeg::audio_encode_args(ab));
    args.push(dest.to_string_lossy().into());
    ffmpeg::run(state, args, &AtomicBool::new(false), |_| {}).await
}

fn safe_name(text: &str) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' }).collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(60).collect::<String>().trim().to_string()
}

pub fn clip_file_name(clip: &Clip, number: usize) -> String {
    let title = clip.doc.hook_title.as_deref().map(safe_name).filter(|s| !s.is_empty()).unwrap_or_else(|| "clip".into());
    format!("{number:02} - {title}.mp4")
}

/// Write every rendered clip of a task into one ZIP (stored, MP4 does not compress).
pub fn export_zip(clips: &[Clip], dest: &Path) -> Result<usize> {
    use std::io::Write;
    let ready: Vec<&Clip> = clips.iter().filter(|c| c.status == "ready" && Path::new(&c.file_path).is_file()).collect();
    if ready.is_empty() {
        bail!("There are no finished clips to export.");
    }
    let mut zip = zip::ZipWriter::new(std::fs::File::create(dest)?);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    for (i, clip) in ready.iter().enumerate() {
        zip.start_file(clip_file_name(clip, i + 1), options)?;
        let mut file = std::fs::File::open(&clip.file_path)?;
        std::io::copy(&mut file, &mut zip)?;
    }
    zip.flush()?;
    zip.finish()?;
    Ok(ready.len())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> ClipDoc { ClipDoc { ranges: vec![[10.0, 20.0], [30.0, 40.0]], hook_title: Some("t".into()), ..Default::default() } }

    #[test]
    fn split_inside_a_range() {
        let (a, b) = split_doc(&doc(), 5.0).unwrap();
        assert_eq!(a.ranges, vec![[10.0, 15.0]]);
        assert_eq!(b.ranges, vec![[15.0, 20.0], [30.0, 40.0]]);
        assert!(a.hook_title.is_some() && b.hook_title.is_none());
    }

    #[test]
    fn split_at_an_edge_is_refused() {
        assert!(split_doc(&doc(), 0.2).is_err());
        assert!(split_doc(&doc(), 19.9).is_err());
    }

    #[test]
    fn merge_needs_two() {
        assert!(merge_docs(&[doc()]).is_err());
        assert_eq!(merge_docs(&[doc(), doc()]).unwrap().ranges.len(), 4);
    }

    #[test]
    fn patch_validates_ranges_and_resets_framing() {
        let mut d = doc();
        d.crop = Some(Default::default());
        apply_patch(&mut d, &serde_json::json!({"ranges": [[1.0, 5.0]], "layout": "cinema", "template": "neon"}), 100.0).unwrap();
        assert_eq!(d.ranges, vec![[1.0, 5.0]]);
        assert!(d.crop.is_none());
        assert_eq!((d.layout.as_str(), d.template.as_str()), ("cinema", "neon"));
        assert!(apply_patch(&mut d, &serde_json::json!({"ranges": [[1.0, 1.1]]}), 100.0).is_err());
        assert!(apply_patch(&mut d, &serde_json::json!({"layout": "bogus"}), 100.0).is_ok());
        assert_eq!(d.layout, "cinema");
    }
}
