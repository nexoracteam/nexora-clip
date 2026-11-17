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

/// Gemini models with a free tier, best first. Each has its own daily allowance, so when one is used up the next still works.
pub const FREE_GEMINI: [&str; 4] = ["gemini-3.1-flash-lite", "gemini-2.5-flash", "gemini-flash-latest", "gemini-3-flash-preview"];
/// OpenRouter models that cost nothing (the ":free" ones).
pub const FREE_OPENROUTER: [&str; 2] = ["openai/gpt-oss-20b:free", "meta-llama/llama-3.3-70b-instruct:free"];

fn pretty(model_name: &str) -> String {
    let (p, m) = split_model(model_name);
    let p = match p.as_str() {
        "google-gla" | "google" => "Gemini",
        "openrouter" => "OpenRouter",
        "ollama" => "Ollama (this PC)",
        "groq" => "Groq",
        "openai" => "OpenAI",
        "anthropic" => "Claude",
        other => other,
    };
    format!("{p} {m}")
}

static OLLAMA: std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>> = std::sync::Mutex::new(None);

/// Models installed in a local Ollama, if it is running (checked at most once a minute).
pub async fn ollama_models(state: &State, settings: &Settings) -> Vec<String> {
    if let Some((t, v)) = OLLAMA.lock().unwrap().as_ref() {
        if t.elapsed() < Duration::from_secs(60) {
            return v.clone();
        }
    }
    let base = settings.ollama_base_url.trim().trim_end_matches('/').trim_end_matches("/v1").to_string();
    let base = if base.is_empty() { "http://localhost:11434".to_string() } else { base };
    let mut names: Vec<String> = Vec::new();
    if let Ok(r) = state.http.get(format!("{base}/api/tags")).timeout(Duration::from_millis(1200)).send().await {
        if let Ok(v) = r.json::<Value>().await {
            for m in v["models"].as_array().cloned().unwrap_or_default() {
                if let Some(n) = m["name"].as_str() {
                    names.push(n.to_string());
                }
            }
        }
    }
    // Instruction-tuned general models first.
    names.sort_by_key(|n| {
        let l = n.to_lowercase();
        ["qwen2.5", "qwen3", "llama3", "gemma", "mistral", "phi"].iter().position(|k| l.contains(k)).unwrap_or(9)
    });
    names.retain(|n| !n.to_lowercase().contains("embed"));
    *OLLAMA.lock().unwrap() = Some((std::time::Instant::now(), names.clone()));
    names
}

/// The models to try, in order: the chosen one, then free Gemini siblings, free OpenRouter, then anything local.
async fn candidates(state: &State, settings: &Settings, model_name: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![model_name.to_string()];
    if !settings.google_api_key.trim().is_empty() {
        v.extend(FREE_GEMINI.iter().map(|m| format!("google-gla:{m}")));
    }
    if !settings.openrouter_api_key.trim().is_empty() {
        v.extend(FREE_OPENROUTER.iter().map(|m| format!("openrouter:{m}")));
    }
    for m in ollama_models(state, settings).await.into_iter().take(2) {
        v.push(format!("ollama:{m}"));
    }
    let mut seen = std::collections::HashSet::new();
    v.retain(|m| seen.insert(m.clone()));
    v
}

/// One model, with retries for short hiccups. A "quota used up" answer is not retried: the model rests instead.
async fn attempt_model(state: &State, settings: &Settings, cand: &str, system: &str, user: &str, timeout_secs: u64) -> std::result::Result<Value, String> {
    let mut last = String::new();
    for attempt in 0..2u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        match call_once(state, settings, cand, system, user, Duration::from_secs(timeout_secs)).await {
            Ok(text) => match extract_json(&text) {
                Ok(v) => return Ok(v),
                Err(e) => last = e.to_string(),
            },
            Err(f) => {
                last = f.message;
                if crate::quota::is_quota(&last) {
                    crate::quota::rest(cand, crate::quota::retry_seconds(&last).unwrap_or(1800));
                    return Err(last);
                }
                if !f.transient {
                    return Err(last);
                }
            }
        }
    }
    Err(last)
}

/// A short, plain reason for one failed model.
fn short_reason(raw: &str) -> String {
    let t = raw.to_lowercase();
    if crate::quota::is_quota(raw) {
        "free limit used up for today".into()
    } else if t.contains("api key") || t.contains("401") || t.contains("403") || t.contains("unauthorized") {
        "key not accepted".into()
    } else if t.contains("404") || t.contains("not found") {
        "model not available".into()
    } else if t.contains("network") || t.contains("timed out") || t.contains("connect") {
        "no connection".into()
    } else {
        "did not answer".into()
    }
}

/// Ask the model for one JSON object. Falls back through other free models so one empty quota never stops the work.
pub async fn json_call(state: &State, settings: &Settings, model_name: &str, system: &str, user: &str, timeout_secs: u64) -> Result<Value> {
    if let Some(v) = try_groq(state, settings, model_name, system, user, timeout_secs, false).await? {
        crate::quota::note(state, &format!("groq:{}", groq_model(settings)), true);
        return Ok(v);
    }
    let mut notes: Vec<String> = Vec::new();
    let mut tried_any = false;
    for cand in candidates(state, settings, model_name).await {
        if config_error(settings, &cand).is_some() {
            if cand == model_name {
                notes.push(format!("{}: {}", pretty(&cand), config_error(settings, &cand).unwrap_or_default().trim_end_matches('.')));
            }
            continue;
        }
        if crate::quota::cooling(&cand) {
            notes.push(format!("{}: resting (free limit used up)", pretty(&cand)));
            continue;
        }
        tried_any = true;
        match attempt_model(state, settings, &cand, system, user, timeout_secs).await {
            Ok(v) => {
                crate::quota::note(state, &cand, true);
                if cand != model_name {
                    crate::applog::log(&format!("AI: {} was busy, {} answered", pretty(model_name), pretty(&cand)));
                }
                return Ok(v);
            }
            Err(e) => {
                crate::quota::note(state, &cand, false);
                notes.push(format!("{}: {}", pretty(&cand), short_reason(&e)));
                crate::applog::log(&format!("AI {} failed: {}", cand, e.chars().take(200).collect::<String>()));
            }
        }
    }
    if !tried_any && config_error(settings, model_name).is_some() && notes.len() <= 1 {
        bail!("[ai_setup] {}", config_error(settings, model_name).unwrap_or_default());
    }
    bail!("[ai_busy] No AI model could answer right now. {}", notes.join(" · "))
}

/// Test exactly one model (no fallback) so the Settings button tells the truth about it.
pub async fn test_model(state: &State, settings: &Settings, model_name: &str) -> Result<Value> {
    if let Some(problem) = config_error(settings, model_name) {
        bail!("[ai_setup] {problem}");
    }
    let started = std::time::Instant::now();
    match attempt_model(state, settings, model_name, "You answer with JSON only.", "Return exactly this JSON object: {\"ok\": true}", 60).await {
        Ok(v) if v["ok"].as_bool() == Some(true) => {
            crate::quota::note(state, model_name, true);
            Ok(json!({"model": model_name, "name": pretty(model_name), "ms": started.elapsed().as_millis() as u64}))
        }
        Ok(_) => bail!("[ai_bad_answer] {} answered, but not with the test answer. Pick another model.", pretty(model_name)),
        Err(e) => {
            crate::quota::note(state, model_name, false);
            if crate::quota::is_quota(&e) {
                bail!("[ai_quota] {} has used up its free allowance. {}", pretty(model_name), crate::quota::retry_seconds(&e).map(|s| format!("It resets in about {}.", human_wait(s))).unwrap_or_else(|| "It resets daily.".into()));
            }
            let l = e.to_lowercase();
            if l.contains("api key") || l.contains("401") || l.contains("403") || l.contains("unauthorized") || l.contains("permission") {
                bail!("[ai_key] {} did not accept the key. Check the key in Settings.", pretty(model_name));
            }
            if l.contains("404") || l.contains("not found") {
                bail!("[ai_model] {} is not available for this key. Pick another model.", pretty(model_name));
            }
            bail!("[ai_down] {} did not answer. {}", pretty(model_name), short_reason(&e))
        }
    }
}

pub fn human_wait(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} h {} min", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
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
    fn waits_read_naturally() {
        assert_eq!(human_wait(45), "45 s");
        assert_eq!(human_wait(600), "10 min");
        assert_eq!(human_wait(8000), "2 h 13 min");
    }

    #[test]
    fn reasons_are_short_and_plain() {
        assert_eq!(short_reason("google-gla returned 429 Too Many Requests: quota"), "free limit used up for today");
        assert_eq!(short_reason("returned 403 permission"), "key not accepted");
        assert_eq!(short_reason("weird"), "did not answer");
    }

    #[test]
    fn groq_status_without_a_key() {
        let s = Settings::default();
        assert_eq!(groq_status(&s)["state"], "not_configured");
    }
}
