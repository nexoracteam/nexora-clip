//! LLM providers behind one JSON-returning call (replaces Pydantic AI).
//! Model names keep the original `provider:model` format.

use crate::models::Settings;
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub fn split_model(name: &str) -> (String, String) {
    match name.split_once(':') {
        Some((p, m)) => (p.trim().to_lowercase(), m.trim().to_string()),
        None => ("openai".into(), name.trim().to_string()),
    }
}

/// A human-readable problem with the configured model, or None when it is usable.
pub fn config_error(settings: &Settings, model_name: &str) -> Option<String> {
    let (provider, model) = split_model(model_name);
    if model.is_empty() {
        return Some("Choose an AI model in Settings (format provider:model).".into());
    }
    let missing = |key: &str, label: &str| key.trim().is_empty().then(|| format!("Add your {label} API key in Settings."));
    match provider.as_str() {
        "google" | "google-gla" => missing(&settings.google_api_key, "Google AI"),
        "openai" => missing(&settings.openai_api_key, "OpenAI"),
        "anthropic" => missing(&settings.anthropic_api_key, "Anthropic"),
        "openrouter" => missing(&settings.openrouter_api_key, "OpenRouter"),
        "groq" => missing(&settings.groq_api_key, "Groq"),
        "ollama" => None,
        "custom" => settings.custom_base_url.trim().is_empty().then(|| "Set the custom OpenAI-compatible base URL in Settings.".to_string()),
        other => Some(format!("Unsupported AI provider \"{other}\". Use google-gla, openai, anthropic, openrouter, groq, ollama or custom.")),
    }
}

pub(crate) fn extract_json(text: &str) -> Result<Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    let start = trimmed.find(['{', '[']).ok_or_else(|| anyhow!("The model did not return JSON"))?;
    let end = trimmed.rfind(['}', ']']).ok_or_else(|| anyhow!("The model did not return JSON"))?;
    if end <= start {
        bail!("The model did not return JSON");
    }
    serde_json::from_str(&trimmed[start..=end]).map_err(|e| anyhow!("The model returned invalid JSON: {e}"))
}

struct Failure {
    transient: bool,
    message: String,
}

async fn call_once(state: &State, settings: &Settings, model_name: &str, system: &str, user: &str, timeout: Duration) -> std::result::Result<String, Failure> {
    let (provider, model) = split_model(model_name);
    let fail = |transient: bool, message: String| Failure { transient, message };
    let net = |e: reqwest::Error| Failure { transient: true, message: format!("Network error talking to {provider}: {e}") };
    let (req, kind) = match provider.as_str() {
        "google" | "google-gla" => (
            state.http.post(format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"))
                .header("x-goog-api-key", settings.google_api_key.trim())
                .json(&json!({
                    "systemInstruction": {"parts": [{"text": system}]},
                    "contents": [{"role": "user", "parts": [{"text": user}]}],
                    "generationConfig": {"responseMimeType": "application/json", "temperature": 0.3}
                })),
            "google",
        ),
        "anthropic" => (
            state.http.post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", settings.anthropic_api_key.trim())
                .header("anthropic-version", "2023-06-01")
                .json(&json!({
                    "model": model, "max_tokens": 16000, "system": system,
                    "messages": [{"role": "user", "content": user}]
                })),
            "anthropic",
        ),
        _ => {
            let (base, key) = match provider.as_str() {
                "openai" => ("https://api.openai.com/v1".to_string(), settings.openai_api_key.clone()),
                "openrouter" => ("https://openrouter.ai/api/v1".to_string(), settings.openrouter_api_key.clone()),
                "groq" => ("https://api.groq.com/openai/v1".to_string(), settings.groq_api_key.clone()),
                "ollama" => {
                    let b = settings.ollama_base_url.trim().trim_end_matches('/');
                    (if b.is_empty() { "http://localhost:11434/v1".to_string() } else { b.to_string() }, settings.ollama_api_key.clone())
                }
                _ => (settings.custom_base_url.trim().trim_end_matches('/').to_string(), settings.custom_api_key.clone()),
            };
            let mut r = state.http.post(format!("{base}/chat/completions")).json(&json!({
                "model": model,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
                "response_format": {"type": "json_object"}
            }));
            if !key.trim().is_empty() {
                r = r.bearer_auth(key.trim());
            }
            (r, "openai")
        }
    };
    let resp = req.timeout(timeout).send().await.map_err(net)?;
    let status = resp.status();
    let body = resp.text().await.map_err(net)?;
    if !status.is_success() {
        let transient = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504 | 529);
        let detail = serde_json::from_str::<Value>(&body).ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from).or_else(|| v["error"].as_str().map(String::from)))
            .unwrap_or_else(|| body.chars().take(300).collect());
        return Err(fail(transient, format!("{provider} returned {status}: {detail}")));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| fail(true, format!("Unreadable response from {provider}: {e}")))?;
    let text = match kind {
        "google" => v["candidates"][0]["content"]["parts"].as_array().map(|parts| {
            parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")
        }).unwrap_or_default(),
        "anthropic" => v["content"].as_array().map(|parts| {
            parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")
        }).unwrap_or_default(),
        _ => v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string(),
    };
    if text.trim().is_empty() {
        return Err(fail(true, format!("{provider} returned an empty answer")));
    }
    Ok(text)
}

/// Ask the model for one JSON object. Retries transient failures and bad JSON.
pub async fn json_call(state: &State, settings: &Settings, model_name: &str, system: &str, user: &str, timeout_secs: u64) -> Result<Value> {
    if let Some(v) = try_groq(state, settings, model_name, system, user, timeout_secs, false).await? {
        return Ok(v);
    }
    if let Some(problem) = config_error(settings, model_name) {
        let groq_note = if settings.groq_api_key.trim().is_empty() { String::new() } else { format!(" (Groq: {})", with_groq(|g| g.message.clone())) };
        bail!("{problem}{groq_note}");
    }
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
        }
        match call_once(state, settings, model_name, system, user, Duration::from_secs(timeout_secs)).await {
            Ok(text) => match extract_json(&text) {
                Ok(v) => return Ok(v),
                Err(e) => last = e.to_string(),
            },
            Err(f) => {
                last = f.message;
                if !f.transient {
                    break;
                }
            }
        }
    }
    bail!("AI request failed: {last}")
}

// ---------- Groq first, configured model as fallback ----------

pub const GROQ_MODELS: [&str; 4] = ["openai/gpt-oss-20b", "openai/gpt-oss-120b", "llama-3.3-70b-versatile", "llama-3.1-8b-instant"];

#[derive(Default)]
struct GroqState {
    until: Option<std::time::Instant>,
    blocked: bool,
    state: String,
    message: String,
    remaining_requests: Option<i64>,
    remaining_tokens: Option<i64>,
    last_key: String,
}

static GROQ: std::sync::Mutex<Option<GroqState>> = std::sync::Mutex::new(None);

fn with_groq<R>(f: impl FnOnce(&mut GroqState) -> R) -> R {
    let mut g = GROQ.lock().unwrap();
    f(g.get_or_insert_with(GroqState::default))
}

fn groq_model(settings: &Settings) -> String {
    if GROQ_MODELS.contains(&settings.groq_model.as_str()) { settings.groq_model.clone() } else { GROQ_MODELS[0].to_string() }
}

pub fn groq_status(settings: &Settings) -> Value {
    let key = settings.groq_api_key.trim().to_string();
    with_groq(|g| {
        if g.last_key != key {
            g.last_key = key.clone();
            g.until = None;
            g.blocked = false;
            g.state.clear();
            g.message.clear();
        }
        if g.until.map(|t| t <= std::time::Instant::now()).unwrap_or(false) {
            g.until = None;
            g.state.clear();
        }
        let (state, message) = if key.is_empty() {
            ("not_configured", "Add a Groq API key to use Groq first.".to_string())
        } else if !settings.groq_enabled {
            ("disabled", "Groq is disabled.".to_string())
        } else if !g.state.is_empty() {
            (g.state.as_str(), g.message.clone())
        } else {
            ("ready", "Groq will be tried first. Your other AI model stays as the fallback.".to_string())
        };
        serde_json::json!({
            "enabled": settings.groq_enabled, "configured": !key.is_empty(), "model": groq_model(settings), "models": GROQ_MODELS,
            "state": state, "message": message,
            "retry_in": g.until.map(|t| t.saturating_duration_since(std::time::Instant::now()).as_secs()).unwrap_or(0),
            "remaining": {"requests": g.remaining_requests, "tokens": g.remaining_tokens},
        })
    })
}

async fn groq_once(state: &State, settings: &Settings, system: &str, user: &str, timeout: Duration) -> std::result::Result<Value, String> {
    let model = groq_model(settings);
    let mut body = json!({
        "model": model, "temperature": 0.2, "max_completion_tokens": 8192, "stream": false,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "response_format": {"type": "json_object"}
    });
    if model.starts_with("openai/gpt-oss") {
        body["reasoning_effort"] = json!("low");
        body["include_reasoning"] = json!(false);
    }
    let resp = state.http.post("https://api.groq.com/openai/v1/chat/completions").bearer_auth(settings.groq_api_key.trim()).json(&body).timeout(timeout).send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(_) => {
            with_groq(|g| { g.until = Some(std::time::Instant::now() + Duration::from_secs(30)); g.state = "unavailable".into(); g.message = "Groq did not answer. Using your other AI model.".into(); });
            return Err("Groq did not answer".into());
        }
    };
    let status = resp.status();
    let headers = resp.headers().clone();
    let num = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
    let (rr, rt) = (num("x-ratelimit-remaining-requests"), num("x-ratelimit-remaining-tokens"));
    let retry = headers.get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<f64>().ok());
    let text = resp.text().await.unwrap_or_default();
    with_groq(|g| { if rr.is_some() { g.remaining_requests = rr; } if rt.is_some() { g.remaining_tokens = rt; } });
    if !status.is_success() {
        with_groq(|g| match status.as_u16() {
            429 => { g.until = Some(std::time::Instant::now() + Duration::from_secs_f64(retry.unwrap_or(60.0).clamp(1.0, 86400.0))); g.state = "rate_limited".into(); g.message = "Groq's free quota is used up for now. Using your other AI model until it resets.".into(); }
            401 | 403 => { g.blocked = true; g.state = "key_rejected".into(); g.message = "Groq did not accept this key or model permission. Update the key and test again.".into(); }
            400 | 404 => { g.blocked = true; g.state = "model_unavailable".into(); g.message = "Groq could not use the selected model. Pick another model and test again.".into(); }
            _ => { g.until = Some(std::time::Instant::now() + Duration::from_secs(30)); g.state = "unavailable".into(); g.message = "Groq is temporarily unavailable. Using your other AI model.".into(); }
        });
        return Err(format!("Groq returned {status}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| "Groq returned unreadable data".to_string())?;
    let choice = &v["choices"][0];
    if choice["finish_reason"].as_str().map(|r| r != "stop").unwrap_or(false) {
        return Err("Groq's answer was cut off".into());
    }
    let content = choice["message"]["content"].as_str().unwrap_or("");
    extract_json(content).map_err(|e| e.to_string())
}

/// Try Groq; Ok(None) means "skip Groq, use the fallback model".
async fn try_groq(state: &State, settings: &Settings, model_name: &str, system: &str, user: &str, timeout_secs: u64, force: bool) -> Result<Option<Value>> {
    let key = settings.groq_api_key.trim();
    if (!settings.groq_enabled && !force) || key.is_empty() || (split_model(model_name).0 == "groq" && !force) {
        return Ok(None);
    }
    let _ = groq_status(settings); // refresh key identity / cooldown
    let skip = with_groq(|g| !force && (g.blocked || g.until.is_some()));
    if skip {
        return Ok(None);
    }
    for attempt in 0..2 {
        match groq_once(state, settings, system, user, Duration::from_secs(timeout_secs.min(180))).await {
            Ok(v) => {
                with_groq(|g| { g.state = "ready".into(); g.message = "Groq connected. Your other AI model stays as the fallback.".into(); g.until = None; g.blocked = false; });
                return Ok(Some(v));
            }
            Err(e) => {
                let stop = with_groq(|g| g.blocked || g.until.is_some());
                if stop || attempt == 1 {
                    if force {
                        bail!("{}", with_groq(|g| if g.message.is_empty() { e.clone() } else { g.message.clone() }));
                    }
                    return Ok(None);
                }
            }
        }
    }
    Ok(None)
}

pub async fn test_groq(state: &State, settings: &Settings) -> Result<Value> {
    if settings.groq_api_key.trim().is_empty() {
        bail!("Add a Groq API key first.");
    }
    let v = try_groq(state, settings, "", "You answer with JSON only.", "Return exactly this JSON object: {\"ok\": true}", 30, true).await?.ok_or_else(|| anyhow!("Groq did not answer."))?;
    if v["ok"].as_bool() != Some(true) {
        bail!("Groq returned an unexpected test answer.");
    }
    Ok(groq_status(settings))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_inside_chatty_answers() {
        assert_eq!(extract_json("```json\n{\"a\": 1}\n```").unwrap()["a"], 1);
        assert_eq!(extract_json("Sure! {\"ok\": true} hope it helps").unwrap()["ok"], true);
        assert!(extract_json("no json here").is_err());
    }

    #[test]
    fn model_names_and_missing_keys() {
        assert_eq!(split_model("ollama:llama3.1"), ("ollama".to_string(), "llama3.1".to_string()));
        let s = Settings::default();
        assert!(config_error(&s, "google-gla:gemini-3-flash-preview").is_some());
        assert!(config_error(&s, "ollama:llama3.1").is_none());
        assert!(config_error(&s, "weird:model").unwrap().contains("Unsupported"));
    }

    #[test]
    fn groq_status_without_a_key() {
        let s = Settings::default();
        assert_eq!(groq_status(&s)["state"], "not_configured");
    }
}
