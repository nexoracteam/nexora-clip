//! Minimal Gemini REST client for the features that need to *see*: frames and video.
//! (Text-only calls go through `llm.rs` and any provider.) Uses the user's own Google key.

use crate::models::Settings;
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

const BASE: &str = "https://generativelanguage.googleapis.com";

/// The key, when the vision features may run (switched on in Settings and a Google key exists).
pub fn key(settings: &Settings) -> Option<String> {
    let k = settings.google_api_key.trim();
    (settings.gemini_vision && !k.is_empty()).then(|| k.to_string())
}

/// The model to use: the configured Gemini model, else a fast default.
pub fn model(settings: &Settings) -> String {
    match settings.llm.strip_prefix("google-gla:").or_else(|| settings.llm.strip_prefix("google:")) {
        Some(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => "gemini-3-flash-preview".into(),
    }
}

pub fn text(s: &str) -> Value {
    json!({ "text": s })
}

pub fn jpeg(bytes: &[u8]) -> Value {
    json!({ "inline_data": { "mime_type": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(bytes) } })
}

pub fn video_file(uri: &str) -> Value {
    json!({ "file_data": { "mime_type": "video/mp4", "file_uri": uri } })
}

fn api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"].as_str().unwrap_or(body);
    let short: String = msg.chars().take(200).collect();
    match status.as_u16() {
        400 if short.to_lowercase().contains("api key") => anyhow!("Google rejected the API key. Check it in Settings."),
        401 | 403 => anyhow!("Google rejected the request ({status}): {short}"),
        429 => anyhow!("Gemini quota used up (429): {short}"),
        _ => anyhow!("Gemini error {status}: {short}"),
    }
}

/// generateContent through the chosen model, then through the other free Gemini models if it is out of quota or unavailable.
pub async fn generate(state: &State, key: &str, model: &str, system: &str, parts: Vec<Value>, schema: Value, timeout_secs: u64) -> Result<Value> {
    let mut list: Vec<String> = vec![model.to_string()];
    list.extend(crate::llm::FREE_GEMINI.iter().map(|m| m.to_string()));
    let mut seen = std::collections::HashSet::new();
    list.retain(|m| seen.insert(m.clone()));
    let mut last = anyhow!("Gemini did not answer.");
    for m in list {
        let id = format!("google-gla:{m}");
        if crate::quota::cooling(&id) {
            continue;
        }
        match generate_one(state, key, &m, system, parts.clone(), schema.clone(), timeout_secs).await {
            Ok(v) => {
                crate::quota::note(state, &id, true);
                return Ok(v);
            }
            Err(e) => {
                crate::quota::note(state, &id, false);
                let text = e.to_string();
                if crate::quota::is_quota(&text) {
                    crate::quota::rest(&id, crate::quota::retry_seconds(&text).unwrap_or(1800));
                }
                last = e;
            }
        }
    }
    Err(last)
}

/// One generateContent call that must answer with JSON matching `schema`.
async fn generate_one(state: &State, key: &str, model: &str, system: &str, parts: Vec<Value>, schema: Value, timeout_secs: u64) -> Result<Value> {
    let body = json!({
        "systemInstruction": { "parts": [ text(system) ] },
        "contents": [ { "role": "user", "parts": parts } ],
        "generationConfig": { "responseMimeType": "application/json", "responseSchema": schema, "temperature": 0.3 }
    });
    let url = format!("{BASE}/v1beta/models/{model}:generateContent");
    let mut last = anyhow!("Gemini did not answer.");
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(3 * attempt)).await;
        }
        let resp = match state.http.post(&url).header("x-goog-api-key", key).timeout(Duration::from_secs(timeout_secs)).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                last = anyhow!("Could not reach Gemini: {e}");
                continue;
            }
        };
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            last = api_error(status, &raw);
            if matches!(status.as_u16(), 500 | 502 | 503 | 504) {
                continue;
            }
            return Err(last);
        }
        let v: Value = serde_json::from_str(&raw).map_err(|_| anyhow!("Gemini returned unreadable data."))?;
        let Some(answer) = v["candidates"][0]["content"]["parts"][0]["text"].as_str() else {
            let why = v["candidates"][0]["finishReason"].as_str().or(v["promptFeedback"]["blockReason"].as_str()).unwrap_or("no answer");
            last = anyhow!("Gemini gave no answer ({why}).");
            continue;
        };
        match crate::llm::extract_json(answer) {
            Ok(j) => return Ok(j),
            Err(_) => last = anyhow!("Gemini's answer was not valid JSON."),
        }
    }
    Err(last)
}

/// Upload a (small, re-encoded) video through the Files API and wait until Gemini can read it.
pub async fn upload_video(state: &State, key: &str, bytes: Vec<u8>, name: &str) -> Result<(String, String)> {
    let start = state.http.post(format!("{BASE}/upload/v1beta/files"))
        .header("x-goog-api-key", key)
        .header("X-Goog-Upload-Protocol", "resumable")
        .header("X-Goog-Upload-Command", "start")
        .header("X-Goog-Upload-Header-Content-Length", bytes.len().to_string())
        .header("X-Goog-Upload-Header-Content-Type", "video/mp4")
        .timeout(Duration::from_secs(60))
        .json(&json!({ "file": { "display_name": name } }))
        .send().await.map_err(|e| anyhow!("Could not reach Google to upload the video: {e}"))?;
    if !start.status().is_success() {
        let s = start.status();
        bail!("{}", api_error(s, &start.text().await.unwrap_or_default()));
    }
    let upload_url = start.headers().get("x-goog-upload-url").and_then(|v| v.to_str().ok()).map(str::to_string)
        .ok_or_else(|| anyhow!("Google did not give an upload address."))?;
    let fin = state.http.post(&upload_url)
        .header("X-Goog-Upload-Offset", "0")
        .header("X-Goog-Upload-Command", "upload, finalize")
        .timeout(Duration::from_secs(600))
        .body(bytes)
        .send().await.map_err(|e| anyhow!("Uploading the video failed: {e}"))?;
    let status = fin.status();
    let raw = fin.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{}", api_error(status, &raw));
    }
    let v: Value = serde_json::from_str(&raw).map_err(|_| anyhow!("Google returned unreadable upload data."))?;
    let file_name = v["file"]["name"].as_str().ok_or_else(|| anyhow!("Google did not return the uploaded file."))?.to_string();
    let uri = v["file"]["uri"].as_str().unwrap_or("").to_string();
    let mut state_now = v["file"]["state"].as_str().unwrap_or("PROCESSING").to_string();
    for _ in 0..90 {
        if state_now == "ACTIVE" {
            return Ok((file_name, uri));
        }
        if state_now == "FAILED" {
            bail!("Google could not process the video.");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let r = state.http.get(format!("{BASE}/v1beta/{file_name}")).header("x-goog-api-key", key).timeout(Duration::from_secs(30)).send().await;
        if let Ok(r) = r {
            if let Ok(j) = r.json::<Value>().await {
                state_now = j["state"].as_str().unwrap_or("PROCESSING").to_string();
            }
        }
    }
    bail!("Google took too long to process the video.")
}

pub async fn delete_file(state: &State, key: &str, file_name: &str) {
    let _ = state.http.delete(format!("{BASE}/v1beta/{file_name}")).header("x-goog-api-key", key).timeout(Duration::from_secs(20)).send().await;
}
