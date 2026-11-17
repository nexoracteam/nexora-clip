//! Which AI models are resting after hitting a free-tier limit, and how many requests each one served today.
//! A model that says "quota exceeded" is skipped until its limit resets, so a busy model costs one request, not one per call.

use crate::state::State;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static COOL: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn with_cool<R>(f: impl FnOnce(&mut HashMap<String, Instant>) -> R) -> R {
    let mut g = COOL.lock().unwrap();
    f(g.get_or_insert_with(HashMap::new))
}

/// True while the model is resting after a quota answer.
pub fn cooling(model: &str) -> bool {
    with_cool(|m| match m.get(model) {
        Some(t) if *t > Instant::now() => true,
        Some(_) => {
            m.remove(model);
            false
        }
        None => false,
    })
}

pub fn rest(model: &str, secs: u64) {
    with_cool(|m| {
        m.insert(model.to_string(), Instant::now() + Duration::from_secs(secs.clamp(30, 6 * 3600)));
    });
}

pub fn reset() {
    with_cool(|m| m.clear());
}

/// "Please retry in 22h13m34.2s" or "retry in 3.5s" -> seconds.
pub fn retry_seconds(text: &str) -> Option<u64> {
    let i = text.find("retry in ")?;
    let rest = &text[i + 9..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'h' || c == 'm' || c == 's')).unwrap_or(rest.len());
    let token = &rest[..end];
    let (mut total, mut num) = (0.0f64, String::new());
    for c in token.chars() {
        match c {
            'h' => { total += num.parse::<f64>().unwrap_or(0.0) * 3600.0; num.clear(); }
            'm' => { total += num.parse::<f64>().unwrap_or(0.0) * 60.0; num.clear(); }
            's' => { total += num.parse::<f64>().unwrap_or(0.0); num.clear(); }
            d => num.push(d),
        }
    }
    (total > 0.0).then_some(total.ceil() as u64)
}

/// Does this error text mean "you used up the free allowance"?
pub fn is_quota(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("quota") || t.contains("resource_exhausted") || t.contains("rate limit") || t.contains("rate_limit") || t.contains("429") || t.contains("too many requests")
}

const KEY: &str = "ai_usage";

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Count one answered (or failed) request for a model.
pub fn note(state: &State, model: &str, ok: bool) {
    let mut v: Value = state.kv_get(KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    if v["date"].as_str() != Some(&today()) {
        v = json!({"date": today(), "models": {}});
    }
    let e = &mut v["models"][model];
    let (mut a, mut b) = (e[0].as_i64().unwrap_or(0), e[1].as_i64().unwrap_or(0));
    if ok { a += 1 } else { b += 1 }
    *e = json!([a, b]);
    let _ = state.kv_set(KEY, &v.to_string());
}

/// Today's counts, newest data only: {date, models: [{model, ok, failed, resting}]}.
pub fn usage(state: &State) -> Value {
    let v: Value = state.kv_get(KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let same = v["date"].as_str() == Some(&today());
    let mut rows = Vec::new();
    if same {
        if let Some(map) = v["models"].as_object() {
            for (k, e) in map {
                rows.push(json!({"model": k, "ok": e[0].as_i64().unwrap_or(0), "failed": e[1].as_i64().unwrap_or(0), "resting": cooling(k)}));
            }
        }
    }
    rows.sort_by(|a, b| b["ok"].as_i64().cmp(&a["ok"].as_i64()));
    json!({"date": today(), "models": rows})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_times_are_read() {
        assert_eq!(retry_seconds("Please retry in 22h13m34.219838759s."), Some(22 * 3600 + 13 * 60 + 35));
        assert_eq!(retry_seconds("retry in 3.5s"), Some(4));
        assert_eq!(retry_seconds("nothing here"), None);
    }

    #[test]
    fn quota_answers_are_recognised() {
        assert!(is_quota("google-gla returned 429 Too Many Requests: You exceeded your current quota"));
        assert!(is_quota("RESOURCE_EXHAUSTED"));
        assert!(!is_quota("API key not valid"));
    }

    #[test]
    fn resting_models_are_skipped_until_reset() {
        assert!(!cooling("test:a"));
        rest("test:a", 120);
        assert!(cooling("test:a"));
        reset();
        assert!(!cooling("test:a"));
    }
}
