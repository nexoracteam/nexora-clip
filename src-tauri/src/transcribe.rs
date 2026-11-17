//! Word-level transcription: local whisper.cpp, AssemblyAI, or an OpenAI-compatible Whisper API.

use crate::ffmpeg::{self, command};
use crate::models::{Line, Settings, Transcript, Word};
use crate::state::State;
use crate::tools;
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};

pub fn load_cached(state: &State, task_id: &str) -> Option<Transcript> {
    let p = state.data_dir.join("sources").join(format!("{task_id}.transcript.json"));
    let raw = std::fs::read_to_string(p).ok()?;
    let t: Transcript = serde_json::from_str(&raw).ok()?;
    (!t.words.is_empty() || t.language == "no-speech").then_some(t)
}

pub(crate) fn save(state: &State, task_id: &str, t: &Transcript) -> Result<()> {
    let p = state.data_dir.join("sources").join(format!("{task_id}.transcript.json"));
    std::fs::write(p, serde_json::to_string(t)?)?;
    Ok(())
}

pub async fn transcribe(
    state: &State,
    settings: &Settings,
    source: &Path,
    task_id: &str,
    duration: f64,
    cancel: &AtomicBool,
    mut on: impl FnMut(f64, &str),
) -> Result<Transcript> {
    if let Some(t) = load_cached(state, task_id) {
        return Ok(t);
    }
    let work = state.data_dir.join("work");
    let provider = settings.transcription_provider.as_str();
    on(0.02, "Extracting audio");
    let mut transcript = match provider {
        "whisper_local" => {
            let wav = work.join(format!("{task_id}.wav"));
            ffmpeg::run(state, vec!["-i".into(), source.to_string_lossy().into(), "-vn".into(), "-ac".into(), "1".into(),
                "-ar".into(), "16000".into(), "-c:a".into(), "pcm_s16le".into(), wav.to_string_lossy().into()], cancel, |_| {}).await?;
            let r = whisper_local(state, settings, &wav, cancel, &mut on).await;
            let _ = std::fs::remove_file(&wav);
            r?
        }
        "assemblyai" | "groq" | "openai" => {
            let mp3 = work.join(format!("{task_id}.mp3"));
            ffmpeg::run(state, vec!["-i".into(), source.to_string_lossy().into(), "-vn".into(), "-ac".into(), "1".into(),
                "-ar".into(), "16000".into(), "-c:a".into(), "libmp3lame".into(), "-b:a".into(), "32k".into(),
                mp3.to_string_lossy().into()], cancel, |_| {}).await?;
            let r = if provider == "assemblyai" {
                assemblyai(state, settings, &mp3, cancel, &mut on).await
            } else {
                openai_compatible(state, settings, provider, &mp3, task_id, duration, cancel, &mut on).await
            };
            let _ = std::fs::remove_file(&mp3);
            r?
        }
        other => bail!("Unknown transcription provider: {other}"),
    };
    transcript.words.retain(|w| !w.text.trim().is_empty() && w.end >= w.start);
    transcript.provider = provider.to_string();
    if transcript.words.is_empty() {
        transcript.language = "no-speech".into();
    }
    save(state, task_id, &transcript)?;
    Ok(transcript)
}

async fn whisper_local(
    state: &State,
    settings: &Settings,
    wav: &Path,
    cancel: &AtomicBool,
    on: &mut impl FnMut(f64, &str),
) -> Result<Transcript> {
    let exe = tools::whisper_exe(state)
        .ok_or_else(|| anyhow!("whisper.cpp is not installed. Install it in Settings → Tools, or pick another transcription provider."))?;
    let model = tools::whisper_model_path(state, &settings.whisper_model);
    if !model.is_file() {
        bail!("The Whisper model \"{}\" is not downloaded. Install it in Settings → Tools.", settings.whisper_model);
    }
    let out_base = wav.with_extension("");
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 12);
    let lang = if settings.transcription_language.trim().is_empty() { "auto".to_string() } else {
        settings.transcription_language.trim().split(['_', '-']).next().unwrap_or("auto").to_lowercase()
    };
    let mut child = command(&exe)
        .current_dir(exe.parent().unwrap())
        .arg("-m").arg(&model)
        .arg("-f").arg(wav)
        .args(["-oj", "-ml", "1", "-sow", "-pp", "-np", "-l", &lang, "-t", &threads.to_string()])
        .arg("-of").arg(&out_base)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(300));
    let mut tail: Vec<String> = Vec::new();
    on(0.05, "Transcribing on this PC");
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if let Some(i) = l.find("progress =") {
                        if let Ok(p) = l[i + 10..].trim().trim_end_matches('%').trim().parse::<f64>() {
                            on(0.05 + 0.95 * p / 100.0, "Transcribing on this PC");
                        }
                    } else {
                        tail.push(l);
                        if tail.len() > 8 { tail.remove(0); }
                    }
                }
                _ => break,
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; bail!("cancelled"); }
            }
        }
    }
    let status = child.wait().await?;
    let json_path = PathBuf::from(format!("{}.json", out_base.to_string_lossy()));
    if !status.success() || !json_path.is_file() {
        bail!("whisper.cpp failed: {}", tail.join(" | "));
    }
    let raw = std::fs::read(&json_path)?;
    let _ = std::fs::remove_file(&json_path);
    let v: Value = serde_json::from_str(&String::from_utf8_lossy(&raw))?;
    let mut words: Vec<Word> = Vec::new();
    for seg in v["transcription"].as_array().cloned().unwrap_or_default() {
        let text = seg["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() || text.starts_with('[') || text.starts_with('(') || text.starts_with('♪') {
            continue;
        }
        let start = seg["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let end = seg["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0;
        // Lone punctuation belongs to the word before it.
        if !text.chars().any(|c| c.is_alphanumeric()) {
            if let Some(last) = words.last_mut() {
                last.text.push_str(&text);
            }
            continue;
        }
        words.push(Word { text, start, end: end.max(start + 0.02), speaker: None });
    }
    Ok(Transcript { words, language: v["result"]["language"].as_str().unwrap_or("").to_string(), provider: String::new() })
}

async fn assemblyai(
    state: &State,
    settings: &Settings,
    audio: &Path,
    cancel: &AtomicBool,
    on: &mut impl FnMut(f64, &str),
) -> Result<Transcript> {
    let key = settings.assemblyai_api_key.trim();
    if key.is_empty() {
        bail!("Add your AssemblyAI API key in Settings, or switch transcription to local Whisper.");
    }
    on(0.1, "Uploading audio");
    let bytes = tokio::fs::read(audio).await?;
    let up: Value = state.http.post("https://api.assemblyai.com/v2/upload")
        .header("authorization", key)
        .header("content-type", "application/octet-stream")
        .body(bytes).send().await?.error_for_status()
        .map_err(|e| anyhow!("AssemblyAI upload failed: {e}"))?
        .json().await?;
    let upload_url = up["upload_url"].as_str().ok_or_else(|| anyhow!("AssemblyAI returned no upload URL"))?;
    let mut body = serde_json::json!({"audio_url": upload_url, "speaker_labels": true, "punctuate": true, "format_text": true});
    let lang = settings.transcription_language.trim();
    if lang.is_empty() { body["language_detection"] = Value::Bool(true); } else { body["language_code"] = Value::String(lang.into()); }
    let created: Value = state.http.post("https://api.assemblyai.com/v2/transcript")
        .header("authorization", key).json(&body).send().await?.error_for_status()
        .map_err(|e| anyhow!("AssemblyAI rejected the job: {e}"))?
        .json().await?;
    let id = created["id"].as_str().ok_or_else(|| anyhow!("AssemblyAI returned no transcript id"))?.to_string();
    let mut waited = 0.0f64;
    loop {
        if cancel.load(Ordering::Relaxed) { bail!("cancelled"); }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        waited += 3.0;
        on((0.2 + waited / 300.0).min(0.95), "Transcribing");
        let v: Value = state.http.get(format!("https://api.assemblyai.com/v2/transcript/{id}"))
            .header("authorization", key).send().await?.error_for_status()?.json().await?;
        match v["status"].as_str() {
            Some("completed") => {
                let words = v["words"].as_array().cloned().unwrap_or_default().iter().map(|w| Word {
                    text: w["text"].as_str().unwrap_or("").to_string(),
                    start: w["start"].as_f64().unwrap_or(0.0) / 1000.0,
                    end: w["end"].as_f64().unwrap_or(0.0) / 1000.0,
                    speaker: w["speaker"].as_str().map(String::from),
                }).collect();
                return Ok(Transcript { words, language: v["language_code"].as_str().unwrap_or("").into(), provider: String::new() });
            }
            Some("error") => {
                let msg = v["error"].as_str().unwrap_or("unknown error");
                // Music-only audio is not a failure: the pipeline picks clips from action and sound instead.
                if msg.to_lowercase().contains("no spoken audio") {
                    return Ok(Transcript { words: vec![], language: String::new(), provider: String::new() });
                }
                bail!("AssemblyAI failed: {msg}")
            }
            _ => {}
        }
        if waited > 3600.0 { bail!("AssemblyAI timed out"); }
    }
}

#[allow(clippy::too_many_arguments)]
async fn openai_compatible(
    state: &State,
    settings: &Settings,
    provider: &str,
    mp3: &Path,
    task_id: &str,
    duration: f64,
    cancel: &AtomicBool,
    on: &mut impl FnMut(f64, &str),
) -> Result<Transcript> {
    let (base, key, model) = if provider == "groq" {
        ("https://api.groq.com/openai/v1", settings.groq_api_key.trim(), "whisper-large-v3-turbo")
    } else {
        ("https://api.openai.com/v1", settings.openai_api_key.trim(), "whisper-1")
    };
    if key.is_empty() {
        bail!("Add your {provider} API key in Settings to transcribe with it.");
    }
    // Both APIs cap uploads at 25 MB; 32 kbps mono is ~14 MB per hour.
    const CHUNK: f64 = 3000.0;
    let mut chunks: Vec<(PathBuf, f64)> = Vec::new();
    if duration > CHUNK + 60.0 {
        let pattern = state.data_dir.join("work").join(format!("{task_id}_%03d.mp3"));
        ffmpeg::run(state, vec!["-i".into(), mp3.to_string_lossy().into(), "-f".into(), "segment".into(),
            "-segment_time".into(), format!("{CHUNK}"), "-c".into(), "copy".into(), pattern.to_string_lossy().into()], cancel, |_| {}).await?;
        let mut i = 0;
        loop {
            let p = state.data_dir.join("work").join(format!("{task_id}_{i:03}.mp3"));
            if !p.is_file() { break; }
            chunks.push((p, i as f64 * CHUNK));
            i += 1;
        }
    } else {
        chunks.push((mp3.to_path_buf(), 0.0));
    }
    let mut words: Vec<Word> = Vec::new();
    let mut language = String::new();
    let total = chunks.len().max(1) as f64;
    for (i, (path, offset)) in chunks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) { bail!("cancelled"); }
        on(0.1 + 0.85 * i as f64 / total, "Transcribing");
        let bytes = tokio::fs::read(path).await?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name("audio.mp3").mime_str("audio/mpeg")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model)
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word");
        let lang = settings.transcription_language.trim();
        if !lang.is_empty() {
            form = form.text("language", lang.split(['_', '-']).next().unwrap_or(lang).to_lowercase());
        }
        let resp = state.http.post(format!("{base}/audio/transcriptions")).bearer_auth(key).multipart(form)
            .timeout(std::time::Duration::from_secs(1200)).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("{provider} transcription failed ({status}): {}", text.chars().take(300).collect::<String>());
        }
        let v: Value = serde_json::from_str(&text)?;
        if language.is_empty() { language = v["language"].as_str().unwrap_or("").to_string(); }
        for w in v["words"].as_array().cloned().unwrap_or_default() {
            words.push(Word {
                text: w["word"].as_str().unwrap_or("").trim().to_string(),
                start: w["start"].as_f64().unwrap_or(0.0) + offset,
                end: w["end"].as_f64().unwrap_or(0.0) + offset,
                speaker: None,
            });
        }
        if chunks.len() > 1 { let _ = std::fs::remove_file(path); }
    }
    Ok(Transcript { words, language, provider: String::new() })
}

pub fn fmt_ts(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as i64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Parse `MM:SS`, `HH:MM:SS` or plain seconds.
pub fn parse_ts(value: &str) -> Option<f64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    let nums: Option<Vec<f64>> = parts.iter().map(|p| p.trim().parse::<f64>().ok()).collect();
    let nums = nums?;
    match nums.len() {
        1 => Some(nums[0]),
        2 => Some(nums[0] * 60.0 + nums[1]),
        3 => Some(nums[0] * 3600.0 + nums[1] * 60.0 + nums[2]),
        _ => None,
    }
}

/// Group words into sentence-sized timestamped lines for the model.
pub fn build_lines(words: &[Word]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Vec<&Word> = Vec::new();
    let flush = |cur: &mut Vec<&Word>, lines: &mut Vec<Line>| {
        if cur.is_empty() { return; }
        let text = cur.iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" ");
        lines.push(Line { start: cur[0].start, end: cur[cur.len() - 1].end, text });
        cur.clear();
    };
    for (i, w) in words.iter().enumerate() {
        if let Some(prev) = cur.last() {
            let gap = w.start - prev.end;
            let speaker_changed = w.speaker.is_some() && prev.speaker != w.speaker;
            let long = w.end - cur[0].start > 14.0 || cur.len() >= 45;
            if gap > 1.5 || speaker_changed || long {
                flush(&mut cur, &mut lines);
            }
        }
        cur.push(w);
        let ends_sentence = w.text.trim_end().ends_with(['.', '?', '!', '。', '？', '！']);
        let next_gap = words.get(i + 1).map(|n| n.start - w.end).unwrap_or(0.0);
        if ends_sentence && (cur.len() >= 4 || next_gap > 0.6) {
            flush(&mut cur, &mut lines);
        }
    }
    flush(&mut cur, &mut lines);
    lines
}

pub fn format_lines(lines: &[Line]) -> String {
    lines.iter().map(|l| format!("[{} - {}] {}", fmt_ts(l.start), fmt_ts(l.end.ceil()), l.text)).collect::<Vec<_>>().join("\n")
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_and_format() {
        assert_eq!(parse_ts("01:30"), Some(90.0));
        assert_eq!(parse_ts("1:02:03"), Some(3723.0));
        assert_eq!(parse_ts("42"), Some(42.0));
        assert_eq!(parse_ts("x:y"), None);
        assert_eq!(fmt_ts(125.9), "02:05");
    }

    #[test]
    fn words_become_sentence_lines() {
        let w = |t: &str, s: f64| Word { text: t.into(), start: s, end: s + 0.4, speaker: None };
        let words = vec![w("Hello", 0.0), w("there", 0.5), w("friend.", 1.0), w("Next", 3.0), w("one.", 3.5)];
        let lines = build_lines(&words);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hello there friend.");
        assert!(format_lines(&lines).starts_with("[00:00 - 00:02]"));
    }
}
