//! Use the subtitles that already ship with a downloaded video (YouTube, etc.)
//! instead of transcribing. yt-dlp keeps `.info.json` next to the video; it lists
//! subtitle tracks with signed URLs. We take the `json3` track, which carries
//! per-word offsets for automatic captions, and turn it into the same word list
//! the transcribers produce. Anything unusual returns None and the normal
//! transcription runs instead.

use crate::models::{Settings, Transcript, Word};
use crate::state::State;
use serde_json::Value;

/// A subtitle track: (language key, url, human-made?).
fn pick_track(info: &Value, preferred: &str) -> Option<(String, String, bool)> {
    let video_lang = info["language"].as_str().unwrap_or("").to_lowercase();
    let lang = if !preferred.is_empty() { preferred.to_string() } else { video_lang.split(['-', '_']).next().unwrap_or("").to_string() };
    let langs: Vec<String> = if lang.is_empty() { vec!["en".into()] } else { vec![lang.clone()] };
    let find = |map: &Value, want: &dyn Fn(&str) -> bool| -> Option<(String, String)> {
        for (key, list) in map.as_object()? {
            if !want(&key.to_lowercase()) {
                continue;
            }
            for item in list.as_array()? {
                let url = item["url"].as_str().unwrap_or("");
                // Machine translations of another language are not the real speech.
                if item["ext"].as_str() == Some("json3") && !url.is_empty() && !url.contains("tlang=") {
                    return Some((key.clone(), url.to_string()));
                }
            }
        }
        None
    };
    let matches_lang = |k: &str| langs.iter().any(|l| k == l || k.starts_with(&format!("{l}-")) || k.starts_with(&format!("{l}_")));
    if let Some((k, u)) = find(&info["subtitles"], &matches_lang) {
        return Some((k, u, true));
    }
    // Automatic captions: the "-orig" track is the speech as spoken.
    if let Some((k, u)) = find(&info["automatic_captions"], &|k| k.ends_with("-orig") && (lang.is_empty() || matches_lang(k.trim_end_matches("-orig")) || k.starts_with(&lang))) {
        return Some((k, u, false));
    }
    if let Some((k, u)) = find(&info["automatic_captions"], &matches_lang) {
        return Some((k, u, false));
    }
    None
}

fn is_noise(token: &str) -> bool {
    let t = token.trim();
    t.is_empty() || (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')') && t.len() > 2) || t.chars().all(|c| "♪♫-–—_ ".contains(c))
}

/// Turn a YouTube `json3` document into words with start/end seconds.
pub fn parse_json3(doc: &Value) -> Vec<Word> {
    struct Raw {
        text: String,
        start: f64,
        end: Option<f64>,
    }
    let mut raw: Vec<Raw> = Vec::new();
    for ev in doc["events"].as_array().cloned().unwrap_or_default() {
        let Some(segs) = ev["segs"].as_array() else { continue };
        let t0 = ev["tStartMs"].as_f64().unwrap_or(0.0);
        let dur = ev["dDurationMs"].as_f64().unwrap_or(0.0);
        let word_level = segs.len() > 1 && segs.iter().any(|s| s.get("tOffsetMs").is_some());
        if word_level {
            for s in segs {
                let off = s["tOffsetMs"].as_f64().unwrap_or(0.0);
                for tok in s["utf8"].as_str().unwrap_or("").split_whitespace() {
                    raw.push(Raw { text: tok.to_string(), start: (t0 + off) / 1000.0, end: None });
                }
            }
        } else {
            // One line of text with a start and a length: spread the words by their letters.
            let line: String = segs.iter().map(|s| s["utf8"].as_str().unwrap_or("")).collect::<Vec<_>>().join("");
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.is_empty() {
                continue;
            }
            let total: f64 = toks.iter().map(|t| t.chars().count() as f64 + 1.0).sum();
            let span = if dur > 0.0 { dur / 1000.0 } else { toks.len() as f64 * 0.35 };
            let mut at = t0 / 1000.0;
            for tok in toks {
                let len = span * (tok.chars().count() as f64 + 1.0) / total;
                raw.push(Raw { text: tok.to_string(), start: at, end: Some(at + len) });
                at += len;
            }
        }
    }
    raw.retain(|r| !is_noise(&r.text));
    raw.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    // Overlapping events can repeat a word.
    raw.dedup_by(|b, a| a.text == b.text && (b.start - a.start).abs() < 0.05);
    let mut out: Vec<Word> = Vec::with_capacity(raw.len());
    for i in 0..raw.len() {
        let start = raw[i].start;
        let next = raw.get(i + 1).map(|n| n.start).unwrap_or(start + 0.6);
        let mut end = raw[i].end.unwrap_or(next).min(start + 1.5).max(start + 0.08);
        if next > start && end > next {
            end = next.max(start + 0.05);
        }
        out.push(Word { text: raw[i].text.clone(), start, end, speaker: None });
    }
    out
}

/// Words are usable when they cover a good part of the video.
pub fn usable(words: &[Word], duration: f64) -> bool {
    if words.len() < 20 {
        return false;
    }
    let span = words.last().map(|w| w.end).unwrap_or(0.0) - words.first().map(|w| w.start).unwrap_or(0.0);
    duration <= 0.0 || span >= duration * 0.4
}

/// Try the downloaded video's own subtitles. None means "transcribe instead".
pub async fn from_download(state: &State, settings: &Settings, task_id: &str, duration: f64) -> Option<Transcript> {
    let dir = state.data_dir.join("sources").join(task_id);
    let info_path = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with(".info.json"))?;
    let info: Value = serde_json::from_str(&std::fs::read_to_string(info_path).ok()?).ok()?;
    let preferred = settings.transcription_language.trim().to_lowercase();
    let preferred = preferred.split(['-', '_']).next().unwrap_or("").to_string();
    let (lang, url, manual) = pick_track(&info, &preferred)?;
    let resp = state.http.get(&url).timeout(std::time::Duration::from_secs(30)).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let doc: Value = resp.json().await.ok()?;
    let words = parse_json3(&doc);
    if !usable(&words, duration) {
        return None;
    }
    let t = Transcript { words, language: lang.split(['-', '_']).next().unwrap_or("").to_string(), provider: if manual { "subtitles".into() } else { "auto-captions".into() } };
    crate::transcribe::save(state, task_id, &t).ok()?;
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn word_level_captions_keep_their_offsets() {
        let doc = json!({"events": [
            {"tStartMs": 1000, "dDurationMs": 3000, "segs": [{"utf8": "hello"}, {"utf8": " big", "tOffsetMs": 400}, {"utf8": " world", "tOffsetMs": 900}]},
            {"tStartMs": 4000, "dDurationMs": 100, "segs": [{"utf8": "\n"}]},
            {"tStartMs": 5000, "dDurationMs": 2000, "segs": [{"utf8": "[Music]"}]}
        ]});
        let w = parse_json3(&doc);
        assert_eq!(w.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["hello", "big", "world"]);
        assert!((w[1].start - 1.4).abs() < 1e-6 && (w[2].start - 1.9).abs() < 1e-6);
        assert!(w[0].end <= w[1].start + 1e-6);
    }

    #[test]
    fn plain_lines_are_spread_over_their_time() {
        let doc = json!({"events": [{"tStartMs": 2000, "dDurationMs": 2000, "segs": [{"utf8": "one two three four"}]}]});
        let w = parse_json3(&doc);
        assert_eq!(w.len(), 4);
        assert!((w[0].start - 2.0).abs() < 1e-6);
        assert!(w[3].end <= 4.01 && w.windows(2).all(|p| p[1].start >= p[0].start));
    }

    #[test]
    fn translations_are_skipped_and_original_track_wins() {
        let info = json!({"language": "en",
            "subtitles": {},
            "automatic_captions": {
                "de": [{"ext": "json3", "url": "https://x/t?tlang=de"}],
                "en-orig": [{"ext": "json3", "url": "https://x/orig"}],
                "en": [{"ext": "json3", "url": "https://x/en"}]}});
        let (k, u, manual) = pick_track(&info, "").unwrap();
        assert_eq!((k.as_str(), u.as_str(), manual), ("en-orig", "https://x/orig", false));
        let only_translation = json!({"language": "ur", "automatic_captions": {"en": [{"ext": "json3", "url": "https://x/t?tlang=en"}]}});
        assert!(pick_track(&only_translation, "").is_none());
        let human = json!({"language": "en", "subtitles": {"en": [{"ext": "json3", "url": "https://x/human"}]}, "automatic_captions": {"en-orig": [{"ext": "json3", "url": "https://x/orig"}]}});
        assert!(pick_track(&human, "").unwrap().2);
    }

    #[test]
    fn short_or_partial_subtitles_are_not_trusted() {
        let few: Vec<Word> = (0..5).map(|i| Word { text: "a".into(), start: i as f64, end: i as f64 + 0.3, speaker: None }).collect();
        assert!(!usable(&few, 60.0));
        let early: Vec<Word> = (0..40).map(|i| Word { text: "a".into(), start: i as f64 * 0.5, end: i as f64 * 0.5 + 0.3, speaker: None }).collect();
        assert!(!usable(&early, 600.0));
        assert!(usable(&early, 30.0));
    }
}
