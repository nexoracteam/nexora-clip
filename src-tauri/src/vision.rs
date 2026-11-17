//! Features that let Gemini look at the picture:
//!  1. `analyze`: pick clips from a small copy of the video when there is little speech.
//!  2. `ground_hook`: write a clip's hook from three frames, not from a thin transcript.
//!  3. `subtitle_keep`: find subtitles burned into the source so they can be cropped off.
//!
//! All of them need the user's Google key and the "Let Gemini look" setting, and every one
//! falls back to the normal path when anything goes wrong.

use crate::ffmpeg::{self, command, tool, Probe};
use crate::gemini;
use crate::models::{Analysis, Segment, Settings, TaskOptions, Virality};
use crate::state::State;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Longest stretch watched in one call (300 tokens per second keeps it inside the model's window).
const STRETCH: f64 = 2400.0;
const MAX_STRETCHES: usize = 8;

// ---------- frames ----------

/// One JPEG per time, `width` pixels wide.
pub async fn frames(state: &State, source: &Path, times: &[f64], width: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for t in times {
        let r = command(tool(state, "ffmpeg"))
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{t:.2}"), "-i"])
            .arg(source)
            .args(["-frames:v", "1", "-vf", &format!("scale='min({width},iw)':-2"), "-q:v", "4", "-f", "image2pipe", "-vcodec", "mjpeg", "-"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
        if let Ok(o) = r {
            if o.status.success() && o.stdout.len() > 500 {
                out.push(o.stdout);
            }
        }
    }
    out
}

/// Source times spread over a clip's ranges at the given fractions of its length.
pub fn times_in(ranges: &[[f64; 2]], fractions: &[f64]) -> Vec<f64> {
    let total: f64 = ranges.iter().map(|r| r[1] - r[0]).sum();
    let mut out = Vec::new();
    for f in fractions {
        let mut at = total * f;
        for r in ranges {
            let len = r[1] - r[0];
            if at <= len {
                out.push(r[0] + at);
                break;
            }
            at -= len;
        }
    }
    out
}

// ---------- 1. watch the video ----------

/// Near-equal stretches no longer than STRETCH (no tiny tail), as (start, length).
pub fn stretches(duration: f64) -> Vec<(f64, f64)> {
    let n = ((duration / STRETCH).ceil() as usize).clamp(1, MAX_STRETCHES);
    let len = if n == MAX_STRETCHES && duration > STRETCH * n as f64 { STRETCH } else { duration / n as f64 };
    (0..n).map(|i| (i as f64 * len, len.min(duration - i as f64 * len))).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub what: String,
    pub reason: String,
    pub score: i64,
}

/// A time as the model may write it: 12.5, "12.5", "1:05" or "00:01:05".
pub fn seconds(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let t = v.as_str()?.trim().trim_end_matches('s');
    if let Ok(n) = t.parse::<f64>() {
        return Some(n);
    }
    let parts: Vec<f64> = t.split(':').map(|x| x.trim().parse::<f64>()).collect::<std::result::Result<_, _>>().ok()?;
    match parts.as_slice() {
        [m, s] => Some(m * 60.0 + s),
        [h, m, s] => Some(h * 3600.0 + m * 60.0 + s),
        _ => None,
    }
}

/// Clips from one stretch's JSON answer, moved to source time and held inside the allowed length.
pub fn parse_picks(v: &Value, offset: f64, stretch_len: f64, max_len: f64) -> Vec<Pick> {
    let mut out = Vec::new();
    for c in v["clips"].as_array().cloned().unwrap_or_default() {
        let (Some(s), Some(e)) = (seconds(&c["start_seconds"]), seconds(&c["end_seconds"])) else { continue };
        let s = s.clamp(0.0, stretch_len);
        let e = e.clamp(0.0, stretch_len).min(s + max_len);
        if e - s < 5.0 {
            continue;
        }
        out.push(Pick {
            start: offset + s,
            end: offset + e,
            title: c["hook_title"].as_str().unwrap_or("").trim().to_string(),
            what: c["what_happens"].as_str().unwrap_or("").trim().to_string(),
            reason: c["reason"].as_str().unwrap_or("").trim().to_string(),
            score: c["score"].as_i64().unwrap_or(60).clamp(0, 100),
        });
    }
    out
}

/// Best first, no two overlapping by more than a third, then back in time order.
pub fn merge_picks(mut all: Vec<Pick>, count: usize) -> Vec<Pick> {
    all.sort_by(|a, b| b.score.cmp(&a.score).then(a.start.total_cmp(&b.start)));
    let mut kept: Vec<Pick> = Vec::new();
    for p in all {
        if kept.len() >= count {
            break;
        }
        let clash = kept.iter().any(|k| {
            let overlap = (p.end.min(k.end) - p.start.max(k.start)).max(0.0);
            overlap > (p.end - p.start).min(k.end - k.start) / 3.0
        });
        if !clash {
            kept.push(p);
        }
    }
    kept.sort_by(|a, b| a.start.total_cmp(&b.start));
    kept
}

fn clips_schema() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {
            "summary": { "type": "STRING" },
            "key_topics": { "type": "ARRAY", "items": { "type": "STRING" } },
            "clips": { "type": "ARRAY", "items": { "type": "OBJECT", "properties": {
                "start_seconds": { "type": "NUMBER" }, "end_seconds": { "type": "NUMBER" },
                "hook_title": { "type": "STRING" }, "what_happens": { "type": "STRING" },
                "score": { "type": "INTEGER" }, "reason": { "type": "STRING" }
            }, "required": ["start_seconds", "end_seconds", "hook_title", "what_happens", "score", "reason"] } }
        },
        "required": ["summary", "clips"]
    })
}

pub async fn analyze(
    state: &State,
    settings: &Settings,
    key: &str,
    source: &Path,
    probe: &Probe,
    video_title: &str,
    opts: &TaskOptions,
    cancel: &AtomicBool,
    mut on: impl FnMut(f64, &str),
) -> Result<Analysis> {
    let duration = probe.duration;
    if duration < 8.0 {
        bail!("This video is too short to cut clips from.");
    }
    let b = crate::ai::bounds(&opts.clip_length);
    let (min_len, max_len) = (b.min.min(duration).max(6.0), b.max.min(duration));
    let target = ((duration / 60.0).ceil() as usize).clamp(3, settings.max_clips.max(3) as usize);
    let parts = stretches(duration);
    let model = gemini::model(settings);
    let work = state.data_dir.join("work");
    std::fs::create_dir_all(&work)?;
    let mut picks: Vec<Pick> = Vec::new();
    let mut summary = String::new();
    let mut topics: Vec<String> = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    for (i, (start, len)) in parts.iter().enumerate() {
        let base = i as f64 / parts.len() as f64;
        let step = 1.0 / parts.len() as f64;
        on(base + step * 0.05, "Making a small copy for Gemini");
        let proxy = work.join(format!("{}-vision-{i}.mp4", uuid::Uuid::new_v4()));
        let mut args: Vec<String> = vec!["-ss".into(), format!("{start:.2}"), "-t".into(), format!("{len:.2}"), "-i".into(), source.to_string_lossy().into(),
            "-vf".into(), "scale=-2:360,fps=1".into(), "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), "32".into()];
        if probe.has_audio {
            args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "48k".into(), "-ac".into(), "1".into()]);
        } else {
            args.push("-an".into());
        }
        args.extend(["-movflags".into(), "+faststart".into(), proxy.to_string_lossy().into()]);
        let made = ffmpeg::run(state, args, cancel, |t| on(base + step * (0.05 + 0.25 * (t / len).clamp(0.0, 1.0)), "Making a small copy for Gemini")).await;
        if let Err(e) = made {
            let _ = std::fs::remove_file(&proxy);
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("cancelled");
            }
            failure = Some(e);
            continue;
        }
        let bytes = std::fs::read(&proxy);
        let _ = std::fs::remove_file(&proxy);
        let bytes = match bytes { Ok(b) => b, Err(e) => { failure = Some(e.into()); continue; } };
        on(base + step * 0.35, "Uploading it to Gemini");
        let (file_name, uri) = match gemini::upload_video(state, key, bytes, "nexora-clip").await {
            Ok(x) => x,
            Err(e) => { failure = Some(e); continue; }
        };
        on(base + step * 0.65, "Gemini is watching the video");
        let n_here = ((target as f64 * len / duration).ceil() as usize + 1).max(2);
        let system = format!(
            "You are an expert short-form video editor. You watch footage with little or no speech (gameplay, trailers, montages, action, music) \
and choose the moments that make the best vertical clips for TikTok, Reels and Shorts.\n\
Rules:\n- Choose up to {n_here} clips, best first. Each clip is between {min} and {max} seconds long.\n\
- start_seconds and end_seconds are seconds from the start of THIS video. Clips must not overlap.\n\
- Start where the action or the beat begins, never mid-motion or mid-word. End just after the payoff, never before it.\n\
- Prefer a clear peak: a dramatic reveal, a big action beat, a funny failure, a striking visual or a strong drop in the music.\n\
- hook_title: 3 to 8 words, plain text, no emojis, hashtags or quotes. A specific, curious teaser built from what the viewer SEES (a place, vehicle, character, action), never a flat description like 'Aerial views of...' or a vague one like 'Action and chaos'. Invent no names, dates or facts you cannot see or hear.\n\
- what_happens: one sentence describing the clip. reason: one sentence on why it works. score: 0 to 100.\n\
- summary: one sentence about the whole video. key_topics: up to 6 short tags.",
            min = min_len as i64, max = max_len as i64
        );
        let mut user = format!("Video title: {video_title}\nThis video is {len:.0} seconds long.");
        if !opts.instructions.trim().is_empty() {
            user.push_str(&format!("\nCreator instructions: {}", opts.instructions.trim()));
        }
        let answer = gemini::generate(state, key, &model, &system, vec![gemini::video_file(&uri), gemini::text(&user)], clips_schema(), 300).await;
        gemini::delete_file(state, key, &file_name).await;
        match answer {
            Ok(v) => {
                let got = parse_picks(&v, *start, *len, max_len);
                if got.is_empty() {
                    let raw = v.to_string();
                    crate::applog::log(&format!("gemini vision: no usable clips in the answer: {}", raw.chars().take(500).collect::<String>()));
                }
                picks.extend(got);
                if summary.is_empty() { summary = v["summary"].as_str().unwrap_or("").trim().to_string(); }
                for t in v["key_topics"].as_array().cloned().unwrap_or_default() {
                    if let Some(t) = t.as_str() { if topics.len() < 8 && !topics.iter().any(|x| x == t) { topics.push(t.to_string()); } }
                }
            }
            Err(e) => failure = Some(e),
        }
        on(base + step, "Gemini finished watching");
    }
    let chosen = merge_picks(picks, target);
    if chosen.is_empty() {
        bail!("{}", failure.map(|e| e.to_string()).unwrap_or_else(|| "Gemini found no clips in this video.".into()));
    }
    let segments = chosen.into_iter().map(|p| {
        let part = (p.score as f64 / 4.0).round().clamp(0.0, 25.0) as i64;
        Segment {
            ranges: vec![[(p.start + 0.05).max(0.0), (p.end - 0.05).max(p.start + 1.0)]],
            text: p.what,
            reasoning: p.reason,
            relevance: (p.score as f64 / 100.0).clamp(0.3, 0.98),
            virality: Virality { hook_score: part, engagement_score: part, value_score: part, shareability_score: part, total_score: p.score, hook_type: "none".into(), virality_reasoning: "Scored by Gemini after watching the video.".into() },
            hook_title: crate::ai::sanitize_hook_title(Some(&p.title)).or_else(|| Some("Watch this moment".into())),
            role: None,
            part_number: None,
            grounded: true,
        }
    }).collect();
    Ok(Analysis { segments, summary, key_topics: topics })
}

// ---------- 2. hook from frames ----------

/// Words said inside a clip's ranges, per second of clip: low means the hook cannot come from speech.
pub fn speech_density(words: &[crate::models::Word], ranges: &[[f64; 2]]) -> f64 {
    let total: f64 = ranges.iter().map(|r| r[1] - r[0]).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let n = words.iter().filter(|w| ranges.iter().any(|r| w.start >= r[0] && w.start < r[1])).count();
    n as f64 / total
}

pub async fn ground_hook(state: &State, settings: &Settings, key: &str, source: &Path, ranges: &[[f64; 2]], said: &str, current: &str, video_title: &str) -> Option<String> {
    let times = times_in(ranges, &[0.12, 0.5, 0.88]);
    let imgs = frames(state, source, &times, 1024).await;
    if imgs.len() < 2 {
        return None;
    }
    let system = "You write the on-screen hook of a vertical short: 3 to 8 words, plain text, no emojis, hashtags or quotes. \
It must name or tease what the viewer actually SEES in these frames from the clip (action, place, vehicle, character, event). \
Use words from the clip's speech only if they are given. Invent no names, dates or facts that are not visible or said. \
Make it specific and curious, not generic ('Insane moment' is bad). Answer in the language of the speech if there is some, else English.";
    let mut user = format!("Video title: {video_title}\nCurrent hook (from the transcript, may be vague): {current}\n");
    user.push_str(&if said.trim().is_empty() { "Nobody speaks in this clip.".to_string() } else { format!("What is said in the clip: {}", said.chars().take(600).collect::<String>()) });
    let mut parts = vec![gemini::text(&user)];
    for (i, img) in imgs.iter().enumerate() {
        parts.push(gemini::text(&format!("Frame {} of {}:", i + 1, imgs.len())));
        parts.push(gemini::jpeg(img));
    }
    let schema = json!({ "type": "OBJECT", "properties": { "hook_title": { "type": "STRING" } }, "required": ["hook_title"] });
    let v = gemini::generate(state, key, &gemini::model(settings), system, parts, schema, 90).await.ok()?;
    crate::ai::sanitize_hook_title(v["hook_title"].as_str())
}

// ---------- 3. subtitles burned into the source ----------

/// Height fraction to keep so burned-in subtitles are cut off, from the per-frame answers
/// (top edge of the subtitle text as a fraction of the height, or None).
pub fn band_from(tops: &[Option<f64>]) -> Option<f64> {
    if tops.len() < 4 {
        return None;
    }
    let mut seen: Vec<f64> = tops.iter().flatten().copied().filter(|t| (0.5..0.98).contains(t)).collect();
    // Present in at least 40% of the frames, and low in the picture (not a title or a lower third).
    if seen.len() * 10 < tops.len() * 4 {
        return None;
    }
    seen.sort_by(|a, b| a.total_cmp(b));
    let median = seen[seen.len() / 2];
    if median < 0.7 {
        return None;
    }
    Some((seen[0] - 0.015).clamp(0.6, 0.95))
}

pub async fn subtitle_keep(state: &State, settings: &Settings, key: &str, source: &Path, probe: &Probe) -> Option<f64> {
    if probe.width <= 0 || probe.height <= 0 || (probe.width as f64 / probe.height as f64) < 1.2 || probe.duration < 20.0 {
        return None;
    }
    let times: Vec<f64> = (0..12).map(|i| probe.duration * (0.05 + 0.9 * i as f64 / 11.0)).collect();
    let imgs = frames(state, source, &times, 1024).await;
    if imgs.len() < 6 {
        return None;
    }
    let system = "For each numbered image, report where subtitles that are burned into the video start. \
Subtitles are dialogue text (a transcription or translation of what people say), usually one or two lines near the bottom. \
Return subtitle_top: the top edge of that text as a fraction of the image height (0 = top, 1 = bottom), or null if the image has no such subtitles. \
Do NOT count: logos or watermarks, news tickers, name captions, game interface (health, ammo, map, kill feed, chat), scene text such as signs, or text overlays at the top.";
    let mut parts = vec![];
    for (i, img) in imgs.iter().enumerate() {
        parts.push(gemini::text(&format!("Image {}:", i + 1)));
        parts.push(gemini::jpeg(img));
    }
    let schema = json!({ "type": "OBJECT", "properties": { "frames": { "type": "ARRAY", "items": { "type": "OBJECT", "properties": {
        "index": { "type": "INTEGER" }, "subtitle_top": { "type": "NUMBER", "nullable": true } }, "required": ["index"] } } }, "required": ["frames"] });
    let v = gemini::generate(state, key, &gemini::model(settings), system, parts, schema, 90).await.ok()?;
    let mut tops: Vec<Option<f64>> = vec![None; imgs.len()];
    for f in v["frames"].as_array()? {
        if let (Some(i), t) = (f["index"].as_i64(), f["subtitle_top"].as_f64()) {
            if i >= 1 && (i as usize) <= tops.len() {
                tops[i as usize - 1] = t;
            }
        }
    }
    band_from(&tops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_videos_are_cut_into_near_equal_stretches() {
        assert_eq!(stretches(600.0), vec![(0.0, 600.0)]);
        let s = stretches(2500.0);
        assert_eq!(s.len(), 2);
        assert!((s[0].1 - 1250.0).abs() < 1e-6 && (s[1].0 - 1250.0).abs() < 1e-6);
        assert_eq!(stretches(100_000.0).len(), MAX_STRETCHES);
    }

    #[test]
    fn answers_are_moved_to_source_time_and_clamped() {
        let v = json!({"clips": [
            {"start_seconds": 10.0, "end_seconds": 100.0, "hook_title": "A", "what_happens": "x", "score": 150, "reason": "r"},
            {"start_seconds": 20.0, "end_seconds": 22.0, "hook_title": "too short", "what_happens": "", "score": 50, "reason": ""},
            {"hook_title": "no times"}]});
        let p = parse_picks(&v, 1000.0, 90.0, 60.0);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].start, p[0].end, p[0].score), (1010.0, 1070.0, 100));
    }

    #[test]
    fn times_can_be_numbers_strings_or_clock_times() {
        assert_eq!(seconds(&json!(12.5)), Some(12.5));
        assert_eq!(seconds(&json!("12.5")), Some(12.5));
        assert_eq!(seconds(&json!("1:05")), Some(65.0));
        assert_eq!(seconds(&json!("00:01:05")), Some(65.0));
        assert_eq!(seconds(&json!("soon")), None);
        let v = json!({"clips": [{"start_seconds": "0:10", "end_seconds": "0:40", "hook_title": "A", "what_happens": "", "score": 70, "reason": ""}]});
        assert_eq!(parse_picks(&v, 0.0, 600.0, 60.0).len(), 1);
    }

    #[test]
    fn best_clips_win_and_overlaps_are_dropped() {
        let pick = |s: f64, e: f64, score: i64| Pick { start: s, end: e, title: String::new(), what: String::new(), reason: String::new(), score };
        let got = merge_picks(vec![pick(0.0, 30.0, 70), pick(10.0, 40.0, 90), pick(100.0, 130.0, 60), pick(200.0, 230.0, 80)], 3);
        assert_eq!(got.iter().map(|p| p.start).collect::<Vec<_>>(), vec![10.0, 100.0, 200.0]);
    }

    #[test]
    fn subtitle_band_needs_to_be_common_and_low() {
        let some = |v: f64| Some(v);
        assert_eq!(band_from(&[some(0.84), some(0.85), some(0.83), some(0.86), None, None]).map(|x| (x * 1000.0).round()), Some(815.0));
        assert!(band_from(&[some(0.84), None, None, None, None, None]).is_none());
        assert!(band_from(&[some(0.6), some(0.62), some(0.6), some(0.61)]).is_none());
        assert!(band_from(&[None; 4]).is_none());
    }

    #[test]
    fn silent_clips_are_found_by_word_density() {
        let w = |t: f64| crate::models::Word { text: "x".into(), start: t, end: t + 0.2, speaker: None };
        let words: Vec<_> = (0..4).map(|i| w(i as f64)).collect();
        assert!(speech_density(&words, &[[0.0, 30.0]]) < 0.2);
        assert!(speech_density(&words, &[[0.0, 4.0]]) >= 1.0);
        assert_eq!(times_in(&[[0.0, 10.0], [50.0, 60.0]], &[0.0, 0.5, 0.9]), vec![0.0, 10.0, 58.0]);
    }
}
