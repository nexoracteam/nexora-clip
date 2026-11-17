//! Optional stock-footage cutaways from Pexels, planned by the LLM.

use crate::llm;
use crate::models::{BrollPlacement, Settings, Word};
use crate::state::State;
use crate::tools::download_file;
use serde_json::Value;

const BROLL_SYSTEM: &str = include_str!("prompts/broll_system.txt");
const EARLIEST_START: f64 = 4.2; // keep the hook title and the speaker's opening clear

fn timed_lines(words: &[Word]) -> String {
    words
        .chunks(8)
        .map(|c| format!("{:.1}: {}", c[0].start, c.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn search(state: &State, key: &str, query: &str) -> Option<(String, String)> {
    let v: Value = state
        .http
        .get("https://api.pexels.com/videos/search")
        .header("Authorization", key)
        .query(&[("query", query), ("orientation", "portrait"), ("per_page", "6"), ("size", "medium")])
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    for video in v["videos"].as_array()? {
        let dur = video["duration"].as_f64().unwrap_or(0.0);
        if dur < 3.0 {
            continue;
        }
        // Closest file to 1080x1920 that is at least 720 wide.
        let best = video["video_files"]
            .as_array()?
            .iter()
            .filter(|f| f["file_type"].as_str() == Some("video/mp4") && f["width"].as_i64().unwrap_or(0) >= 540)
            .min_by_key(|f| (f["height"].as_i64().unwrap_or(0) - 1920).abs());
        if let Some(f) = best {
            let id = format!("{}_{}", video["id"].as_i64().unwrap_or(0), f["id"].as_i64().unwrap_or(0));
            return Some((id, f["link"].as_str()?.to_string()));
        }
    }
    None
}

/// Plan and fetch cutaways for one clip. Best effort: any failure just means fewer cutaways.
pub async fn plan(state: &State, settings: &Settings, words: &[Word], duration: f64) -> Vec<BrollPlacement> {
    let key = settings.pexels_api_key.trim();
    if key.is_empty() || words.is_empty() || duration < 10.0 {
        return vec![];
    }
    let max_cues = ((duration / 12.0).round() as usize).clamp(1, 4);
    let prompt = format!(
        "Clip length: {duration:.1} seconds.\nReturn at most {max_cues} cutaways. None may start before second {EARLIEST_START:.1} or run past second {:.1}.\n\nTranscript, one line per phrase, prefixed with the clip second it starts at:\n{}",
        (duration - 1.0).max(0.0),
        timed_lines(words)
    );
    let Ok(v) = llm::json_call(state, settings, &settings.llm, BROLL_SYSTEM, &prompt, 60).await else { return vec![] };
    let mut placements: Vec<BrollPlacement> = Vec::new();
    for cue in v["cues"].as_array().cloned().unwrap_or_default() {
        if placements.len() >= max_cues {
            break;
        }
        let start = cue["start"].as_f64().unwrap_or(-1.0);
        let dur = cue["duration"].as_f64().unwrap_or(3.0).clamp(2.0, 4.0);
        let query = cue["query"].as_str().unwrap_or("").trim().to_string();
        if query.is_empty() || start < EARLIEST_START || start + dur > duration - 1.0 {
            continue;
        }
        if placements.iter().any(|p| start < p.start + p.duration + 1.0 && p.start < start + dur + 1.0) {
            continue;
        }
        let mut hit = search(state, key, &query).await;
        if hit.is_none() {
            let fallback = cue["fallback_query"].as_str().unwrap_or("").trim();
            if !fallback.is_empty() {
                hit = search(state, key, fallback).await;
            }
        }
        let Some((id, link)) = hit else { continue };
        let path = state.data_dir.join("cache").join("broll").join(format!("{id}.mp4"));
        if !path.is_file() && download_file(&state.http, &link, &path, |_| {}).await.is_err() {
            continue;
        }
        placements.push(BrollPlacement { start, duration: dur, path: path.to_string_lossy().into(), query });
    }
    placements.sort_by(|a, b| a.start.total_cmp(&b.start));
    placements
}
