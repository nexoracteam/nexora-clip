//! Moments from pictures and sound, for videos with little or no speech
//! (game trailers, gameplay, montages): scene cuts + loudness pick the clips,
//! the video's own title is used to write the hook titles.

use crate::ffmpeg::{command, tool};
use crate::llm;
use crate::models::*;
use crate::state::State;
use anyhow::{bail, Result};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

/// Is there too little speech for the transcript to choose clips?
pub fn is_low_speech(words: usize, duration: f64) -> bool {
    let per_minute = words as f64 / (duration / 60.0).max(0.2);
    words < 20 || per_minute < 8.0
}

/// Scene-change score of every quarter second (0 = same picture, 1 = new picture).
async fn scene_scores(state: &State, source: &Path, cancel: &AtomicBool) -> Result<Vec<(f64, f64)>> {
    let out = command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(source)
        .args(["-an", "-vf", "fps=4,scale=320:-2,select='gte(scene,0)',metadata=print:key=lavfi.scene_score:file=-", "-f", "null", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut scores = Vec::new();
    let mut t = 0.0;
    for line in text.lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + 9..].trim().parse().unwrap_or(t);
        } else if let Some(v) = line.strip_prefix("lavfi.scene_score=") {
            scores.push((t, v.trim().parse::<f64>().unwrap_or(0.0)));
        }
    }
    Ok(scores)
}

/// Hard cuts: strong changes, merged when closer than a second (a flash or fade is one change).
fn cuts_from(scores: &[(f64, f64)]) -> Vec<f64> {
    let mut merged: Vec<f64> = Vec::new();
    for (t, v) in scores {
        if *v >= 0.25 && merged.last().map(|l| t - l >= 1.0).unwrap_or(true) {
            merged.push(*t);
        }
    }
    merged
}

/// RMS level per half second, in dB.
async fn loudness(state: &State, source: &Path) -> Vec<(f64, f64)> {
    let Ok(out) = command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(source)
        .args(["-vn", "-af", "aresample=16000,asetnsamples=n=8000:p=0,astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.RMS_level:file=-", "-f", "null", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
    else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut levels = Vec::new();
    let mut t = 0.0;
    for line in text.lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + 9..].trim().parse().unwrap_or(t);
        } else if let Some(v) = line.strip_prefix("lavfi.astats.Overall.RMS_level=") {
            levels.push((t, v.trim().parse::<f64>().unwrap_or(-90.0).max(-90.0)));
        }
    }
    levels
}

struct Cand {
    range: [f64; 2],
    score: f64,
    cut_rate: f64,
    loud: f64,
}

fn candidates(duration: f64, scores: &[(f64, f64)], cuts: &[f64], levels: &[(f64, f64)], min: f64, max: f64, ideal: f64) -> Vec<Cand> {
    let mut bounds: Vec<f64> = vec![0.0];
    bounds.extend(cuts.iter().copied().filter(|c| *c > 0.5 && *c < duration - 0.5));
    // A steady grid, so videos without hard cuts (gameplay) still get candidate windows.
    let mut g = 5.0;
    while g < duration - 0.5 {
        bounds.push(g);
        g += 5.0;
    }
    bounds.push(duration);
    bounds.sort_by(|a, b| a.total_cmp(b));
    bounds.dedup_by(|a, b| (*a - *b).abs() < 0.8);
    let mut out = Vec::new();
    for &s in &bounds {
        if s > duration - min + 0.01 {
            continue;
        }
        let mut ends: Vec<f64> = bounds.iter().copied().filter(|e| *e - s >= min && *e - s <= max).collect();
        if ends.is_empty() {
            ends.push((s + ideal).min(duration));
        }
        for e in ends {
            let len = e - s;
            if len < min - 0.01 {
                continue;
            }
            let n_cuts = cuts.iter().filter(|c| **c > s && **c < e).count() as f64;
            let cut_rate = (n_cuts / len / 0.4).min(1.0);
            let act: Vec<f64> = scores.iter().filter(|(t, _)| *t >= s && *t < e).map(|(_, v)| *v).collect();
            let activity = if act.is_empty() { 0.3 } else { (act.iter().sum::<f64>() / act.len() as f64 / 0.12).min(1.0) };
            let window: Vec<f64> = levels.iter().filter(|(t, _)| *t >= s && *t < e).map(|(_, v)| *v).collect();
            let loud = if window.is_empty() { 0.4 } else { ((window.iter().sum::<f64>() / window.len() as f64 + 60.0) / 60.0).clamp(0.0, 1.0) };
            let peak = window.iter().copied().fold(-90.0, f64::max);
            let peak = ((peak + 40.0) / 40.0).clamp(0.0, 1.0);
            let on_cut = cuts.iter().any(|c| (c - s).abs() < 0.6) as i32 as f64;
            let score = 0.25 * cut_rate + 0.2 * activity + 0.36 * loud + 0.13 * peak + 0.06 * on_cut - 0.12 * ((len - ideal).abs() / ideal);
            out.push(Cand { range: [s, e], score, cut_rate: cut_rate.max(activity * 0.6), loud });
        }
    }
    out
}

fn pick(mut cands: Vec<Cand>, count: usize) -> Vec<Cand> {
    cands.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Cand> = Vec::new();
    for c in cands {
        if kept.len() >= count {
            break;
        }
        if kept.iter().all(|k| c.range[1] <= k.range[0] - 1.0 || c.range[0] >= k.range[1] + 1.0) {
            kept.push(c);
        }
    }
    kept.sort_by(|a, b| a.range[0].total_cmp(&b.range[0]));
    kept
}

fn clean_title(video_title: &str) -> String {
    let t = video_title.split(['|', '–', '—']).next().unwrap_or(video_title).trim();
    let t: String = t.chars().take(42).collect();
    t.trim().to_string()
}

async fn titles(state: &State, settings: &Settings, video_title: &str, instructions: &str, segs: &[Cand], duration: f64) -> Vec<String> {
    let fallback = |i: usize| {
        let base = clean_title(video_title);
        if base.is_empty() { format!("Moment {}", i + 1) } else if segs.len() == 1 { base } else { format!("{base} — part {}", i + 1) }
    };
    let list: Vec<String> = segs.iter().enumerate().map(|(i, c)| format!(
        "clip {}: {:.0}s-{:.0}s of {:.0}s, {}, {}", i + 1, c.range[0], c.range[1], duration,
        if c.cut_rate > 0.55 { "very fast cutting" } else if c.cut_rate > 0.25 { "fast cutting" } else { "slow, steady shots" },
        if c.loud > 0.7 { "loud" } else if c.loud > 0.45 { "medium loudness" } else { "quiet" })).collect();
    let system = "You write short, punchy on-screen hook titles (3-8 words, plain text, no emojis, no hashtags, no quotes) for vertical clips cut from one video. You only know the video's title and each clip's position and intensity: never invent plot facts, names, dates or numbers that are not in the video title. Make each title different, curiosity-driven and in the video title's language. Return valid JSON only: {\"titles\":[\"...\"]} with one title per clip, in order.";
    let mut user = format!("Video title: {video_title}\n\n{}", list.join("\n"));
    if !instructions.trim().is_empty() {
        user.push_str(&format!("\n\nCreator preferences: {}", instructions.trim()));
    }
    let Ok(v) = llm::json_call(state, settings, &settings.llm, system, &user, 90).await else {
        return (0..segs.len()).map(fallback).collect();
    };
    let got: Vec<String> = v["titles"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str().and_then(|s| crate::ai::sanitize_hook_title(Some(s)))).collect();
    (0..segs.len()).map(|i| got.get(i).cloned().unwrap_or_else(|| fallback(i))).collect()
}

pub async fn analyze(
    state: &State,
    settings: &Settings,
    source: &Path,
    duration: f64,
    video_title: &str,
    opts: &TaskOptions,
    cancel: &AtomicBool,
    mut on: impl FnMut(f64, &str),
) -> Result<Analysis> {
    let b = crate::ai::bounds(&opts.clip_length);
    if duration < 6.0 {
        bail!("This video is too short to cut clips from.");
    }
    on(0.1, "Finding scene cuts");
    let scores = scene_scores(state, source, cancel).await?;
    let cuts = cuts_from(&scores);
    on(0.5, "Measuring the sound");
    let levels = loudness(state, source).await;
    let (min, max, ideal) = if duration < b.min { (duration.min(6.0), duration, duration) } else { (b.min.min(duration), b.max.min(duration), b.ideal_max.min(duration)) };
    let count = ((duration / 45.0).ceil() as usize).clamp(1, (settings.max_clips.max(1)) as usize);
    let mut chosen = pick(candidates(duration, &scores, &cuts, &levels, min, max, ideal), count);
    if chosen.is_empty() {
        chosen.push(Cand { range: [0.0, duration.min(max)], score: 0.5, cut_rate: 0.3, loud: 0.5 });
    }
    on(0.8, "Writing titles");
    let names = titles(state, settings, video_title, &opts.instructions, &chosen, duration).await;
    on(1.0, "Clips selected");
    let segments = chosen.iter().zip(names).map(|(c, title)| {
        let base = (45.0 + c.score.clamp(0.0, 1.0) * 50.0).round() as i64;
        let part = (base as f64 / 4.0).round().clamp(0.0, 25.0) as i64;
        Segment {
            ranges: vec![[(c.range[0] + 0.05).max(0.0), (c.range[1] - 0.05).max(c.range[0] + 0.5)]],
            text: String::new(),
            reasoning: format!("Picked by scene changes and sound: {:.0}% cut density, {:.0}% loudness.", c.cut_rate * 100.0, c.loud * 100.0),
            relevance: c.score.clamp(0.3, 0.98),
            virality: Virality { hook_score: part, engagement_score: part, value_score: part, shareability_score: part, total_score: base.clamp(0, 100), hook_type: "none".into(), virality_reasoning: "Scored from action and audio energy (no speech in this video).".into(), mood: String::new() },
            hook_title: Some(title),
            role: None,
            part_number: None,
            grounded: false,
        }
    }).collect();
    Ok(Analysis { segments, summary: format!("{} — clips chosen from scene changes and sound.", clean_title(video_title)), key_topics: vec![] })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_speech_detection() {
        assert!(is_low_speech(0, 90.0));
        assert!(is_low_speech(15, 60.0));
        assert!(is_low_speech(60, 3600.0));
        assert!(!is_low_speech(300, 120.0));
    }

    #[test]
    fn loud_busy_windows_win_and_never_overlap() {
        let scores: Vec<(f64, f64)> = (0..400).map(|i| (i as f64 * 0.25, if (40.0..70.0).contains(&(i as f64 * 0.25)) { 0.3 } else { 0.01 })).collect();
        let cuts = cuts_from(&scores);
        let levels: Vec<(f64, f64)> = (0..200).map(|i| (i as f64 * 0.5, if (40.0..70.0).contains(&(i as f64 * 0.5)) { -12.0 } else { -45.0 })).collect();
        let picked = pick(candidates(100.0, &scores, &cuts, &levels, 15.0, 30.0, 25.0), 3);
        assert!(!picked.is_empty());
        assert!(picked.iter().any(|c| c.range[0] >= 35.0 && c.range[1] <= 75.0), "{:?}", picked.iter().map(|c| c.range).collect::<Vec<_>>());
        for w in picked.windows(2) {
            assert!(w[0].range[1] <= w[1].range[0]);
        }
    }

    #[test]
    fn titles_fall_back_to_the_video_name() {
        assert_eq!(clean_title("GTA VI Trailer 2 | Rockstar Games"), "GTA VI Trailer 2");
    }
}
