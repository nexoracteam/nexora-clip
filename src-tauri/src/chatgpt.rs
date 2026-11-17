//! "Sign in with ChatGPT": OpenAI's official token-sharing flow for open-source, locally run apps.
//! The user signs in at OpenAI, and AI calls use their own ChatGPT plan (Plus or Pro) instead of an API key.
//!
//! Follows OpenAI's published contract only: dynamic client registration, authorization code with PKCE on a
//! loopback redirect, rotating refresh tokens, and the Responses API with `store:false` and streaming.
//! Nothing here uses another app's client id or any unofficial endpoint.
//!
//! Tokens are stored with Windows DPAPI (`state::SECRET_KEYS`) and never written to the log.
//! The ID token comes straight from OpenAI's token endpoint over TLS, so its claims (issuer, audience,
//! expiry, nonce) are checked, and the signature check is left to that TLS channel (OIDC Core 3.1.3.7).

use crate::state::State;
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;
use tokio::net::TcpListener;

const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const API: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const CALLBACK_PATH: &str = "/auth/callback";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const KEY: &str = "chatgpt_auth";

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Creds {
    /// The client id OpenAI issued to this install (`oaiapp_…`); reused for every later sign-in.
    client_id: String,
    /// Random id of this install, sent when registering.
    host_id: String,
    refresh_token: String,
    access_token: String,
    expires_at: i64,
    email: String,
    sub: String,
    /// The model slug chosen in Settings.
    model: String,
}

/// (retry later is sensible, message)
pub type Fail = (bool, String);

static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static MESSAGE: Mutex<String> = Mutex::new(String::new());

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn load(state: &State) -> Creds {
    state.kv_get(KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(state: &State, c: &Creds) -> Result<()> {
    state.kv_set(KEY, &serde_json::to_string(c)?)
}

fn say(app: Option<&AppHandle>, state: &State, text: &str) {
    *MESSAGE.lock().unwrap() = text.to_string();
    if let Some(app) = app {
        let _ = app.emit("chatgpt-auth", json!({"message": text, "connected": connected(state)}));
    }
}

pub fn connected(state: &State) -> bool {
    !load(state).refresh_token.is_empty()
}

pub fn status(state: &State) -> Value {
    let c = load(state);
    json!({
        "connected": !c.refresh_token.is_empty(),
        "email": c.email,
        "model": c.model,
        "message": MESSAGE.lock().unwrap().clone(),
    })
}

/// The model slug to use when an AI call names none.
pub fn chosen_model(state: &State) -> String {
    load(state).model
}

pub fn set_model(state: &State, model: &str) -> Result<()> {
    let mut c = load(state);
    c.model = model.trim().to_string();
    save(state, &c)
}

fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub(crate) fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn authorize_url(client_id: &str, host_id: &str, redirect: &str, state_nonce: &str, nonce: &str, challenge: &str) -> String {
    let mut q: Vec<(&str, String)> = vec![
        ("response_type", "code".into()),
        ("client_id", client_id.into()),
        ("redirect_uri", redirect.into()),
        ("scope", SCOPE.into()),
        ("resource", API.into()),
        ("state", state_nonce.into()),
        ("nonce", nonce.into()),
        ("code_challenge", challenge.into()),
        ("code_challenge_method", "S256".into()),
    ];
    if client_id == DYNAMIC_CLIENT {
        q.push(("agent_name_hint", "Nexora Clip".into()));
        q.push(("ext_agent_host_id", host_id.into()));
    }
    let query: Vec<String> = q.iter().map(|(k, v)| format!("{k}={}", pct(v))).collect();
    format!("{AUTHORIZE}?{}", query.join("&"))
}

/// Check the claims of the ID token and return (sub, email).
fn id_claims(id_token: &str, client_id: &str, nonce: &str, now: i64) -> Result<(String, String)> {
    let payload = id_token.split('.').nth(1).ok_or_else(|| anyhow!("OpenAI sent an unreadable sign-in token."))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|_| anyhow!("OpenAI sent an unreadable sign-in token."))?;
    let c: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow!("OpenAI sent an unreadable sign-in token."))?;
    if c["iss"].as_str() != Some(ISSUER) {
        bail!("The sign-in token came from an unexpected issuer. Sign-in stopped.");
    }
    let aud_ok = match &c["aud"] {
        Value::String(s) => s == client_id,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        bail!("The sign-in token was meant for another app. Sign-in stopped.");
    }
    if c["exp"].as_i64().map(|e| e <= now).unwrap_or(true) {
        bail!("The sign-in token had already expired. Try again.");
    }
    if c["nonce"].as_str() != Some(nonce) {
        bail!("The sign-in response did not match this request. Try again.");
    }
    let sub = c["sub"].as_str().unwrap_or("").to_string();
    if sub.is_empty() {
        bail!("The sign-in token has no account id.");
    }
    Ok((sub, c["email"].as_str().unwrap_or("").to_string()))
}

struct TokenError {
    code: String,
    message: String,
    /// A network or server problem; the refresh token is still good.
    transient: bool,
}

async fn token_request(state: &State, form: &[(&str, &str)]) -> std::result::Result<Value, TokenError> {
    let resp = state.http.post(TOKEN).form(form).timeout(Duration::from_secs(30)).send().await
        .map_err(|_| TokenError { code: "network".into(), message: "Could not reach OpenAI. Check your connection.".into(), transient: true })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(v);
    }
    let code = v["error"].as_str().or_else(|| v["error"]["code"].as_str()).unwrap_or("").to_string();
    let message = v["error_description"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("OpenAI refused the request").to_string();
    Err(TokenError { code, message, transient: matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504) })
}

/// Start the browser sign-in. Returns at once; progress comes through the `chatgpt-auth` event.
pub async fn login(app: AppHandle, state: State) -> Result<()> {
    let mut creds = load(&state);
    if creds.host_id.is_empty() {
        creds.host_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
        save(&state, &creds)?;
    }
    let client_id = if creds.client_id.is_empty() { DYNAMIC_CLIENT.to_string() } else { creds.client_id.clone() };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}{CALLBACK_PATH}", listener.local_addr()?.port());
    let (verifier, state_nonce, nonce) = (random_token(), uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
    let url = authorize_url(&client_id, &creds.host_id, &redirect, &state_nonce, &nonce, &challenge(&verifier));
    say(Some(&app), &state, "Waiting for ChatGPT sign-in in your browser…");
    app.opener().open_url(url, None::<&str>).map_err(|e| anyhow!("Could not open the browser: {e}"))?;
    tauri::async_runtime::spawn(async move {
        let result: Result<String> = async {
            let q = crate::publish::wait_callback_at(listener, &state_nonce, "ChatGPT", CALLBACK_PATH).await?;
            let code = q.get("code").ok_or_else(|| anyhow!("Sign-in was cancelled."))?;
            // On first sign-in OpenAI issues this install its own client id and returns it here.
            let issued = if client_id == DYNAMIC_CLIENT {
                q.get("client_id").cloned().filter(|c| !c.is_empty()).ok_or_else(|| anyhow!("OpenAI did not register this app. Try again later."))?
            } else {
                client_id.clone()
            };
            let t = token_request(&state, &[("grant_type", "authorization_code"), ("client_id", &issued), ("code", code), ("code_verifier", &verifier), ("redirect_uri", &redirect), ("resource", API)])
                .await.map_err(|e| anyhow!("{}", e.message))?;
            let scope = t["scope"].as_str().unwrap_or("");
            if !scope.split_whitespace().any(|s| s == PLAN_SCOPE) {
                bail!("OpenAI did not share your ChatGPT plan with this app. Your account may not be eligible (Plus or Pro is needed).");
            }
            let refresh = t["refresh_token"].as_str().ok_or_else(|| anyhow!("OpenAI did not return a refresh token."))?.to_string();
            let access = t["access_token"].as_str().ok_or_else(|| anyhow!("OpenAI did not return an access token."))?.to_string();
            let (sub, email) = id_claims(t["id_token"].as_str().unwrap_or(""), &issued, &nonce, now_secs())?;
            let mut c = load(&state);
            c.client_id = issued;
            c.refresh_token = refresh;
            c.access_token = access;
            c.expires_at = now_secs() + t["expires_in"].as_i64().unwrap_or(3600);
            c.sub = sub;
            c.email = email.clone();
            save(&state, &c)?;
            Ok(email)
        }.await;
        let msg = match result {
            Ok(email) if email.is_empty() => "ChatGPT connected.".to_string(),
            Ok(email) => format!("ChatGPT connected as {email}."),
            Err(e) => e.to_string(),
        };
        say(Some(&app), &state, &msg);
    });
    Ok(())
}

/// Disconnect: ask OpenAI to revoke the token (best effort), then forget it. The client id is kept for the next sign-in.
pub async fn disconnect(state: &State) -> Result<()> {
    let mut c = load(state);
    if !c.refresh_token.is_empty() && !c.client_id.is_empty() {
        let endpoint = async {
            let d: Value = state.http.get(DISCOVERY).timeout(Duration::from_secs(10)).send().await.ok()?.json().await.ok()?;
            d["revocation_endpoint"].as_str().map(String::from)
        }.await;
        if let Some(url) = endpoint {
            let _ = state.http.post(url).form(&[("token", c.refresh_token.as_str()), ("token_type_hint", "refresh_token"), ("client_id", c.client_id.as_str())]).timeout(Duration::from_secs(10)).send().await;
        }
    }
    c.refresh_token.clear();
    c.access_token.clear();
    c.expires_at = 0;
    c.email.clear();
    c.sub.clear();
    save(state, &c)?;
    say(None, state, "");
    Ok(())
}

/// A valid access token, refreshing it first when it is about to expire. Refreshes run one at a time because refresh tokens rotate.
async fn access_token(state: &State) -> std::result::Result<String, Fail> {
    let fresh = |c: &Creds| !c.access_token.is_empty() && c.expires_at - now_secs() > 60;
    let c = load(state);
    if c.refresh_token.is_empty() {
        return Err((false, "ChatGPT is not connected (401). Sign in with ChatGPT in Settings.".into()));
    }
    if fresh(&c) {
        return Ok(c.access_token);
    }
    let _guard = REFRESH.lock().await;
    let mut c = load(state);
    if c.refresh_token.is_empty() {
        return Err((false, "ChatGPT is not connected (401). Sign in with ChatGPT in Settings.".into()));
    }
    if fresh(&c) {
        return Ok(c.access_token);
    }
    match token_request(state, &[("grant_type", "refresh_token"), ("client_id", &c.client_id), ("refresh_token", &c.refresh_token), ("resource", API)]).await {
        Ok(t) => {
            let access = t["access_token"].as_str().ok_or((true, "OpenAI did not return an access token.".to_string()))?.to_string();
            if let Some(r) = t["refresh_token"].as_str() {
                c.refresh_token = r.to_string();
            }
            c.access_token = access.clone();
            c.expires_at = now_secs() + t["expires_in"].as_i64().unwrap_or(3600);
            save(state, &c).map_err(|e| (false, format!("Could not save the ChatGPT sign-in: {e}")))?;
            Ok(access)
        }
        Err(e) if e.transient => Err((true, format!("ChatGPT sign-in refresh failed: {}", e.message))),
        Err(e) => {
            // The grant is gone (revoked, expired, or the user disconnected in ChatGPT). Sign in again with the saved client id.
            crate::applog::log(&format!("ChatGPT refresh refused: {}", e.code));
            c.refresh_token.clear();
            c.access_token.clear();
            c.expires_at = 0;
            let _ = save(state, &c);
            say(None, state, "ChatGPT sign-in ended. Connect again in Settings.");
            Err((false, "ChatGPT sign-in ended (401). Connect again in Settings.".into()))
        }
    }
}

/// Models this account can use (`GET /v1/models`, only the ones OpenAI lists for apps).
pub async fn models(state: &State) -> Result<Vec<Value>> {
    let token = access_token(state).await.map_err(|(_, m)| anyhow!(m))?;
    let resp = state.http.get(format!("{API}/models")).bearer_auth(token).timeout(Duration::from_secs(30)).send().await.map_err(|_| anyhow!("Could not reach OpenAI."))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{}", map_error(status.as_u16(), &text).1);
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| anyhow!("OpenAI returned an unreadable model list."))?;
    Ok(listed_models(&v))
}

fn listed_models(v: &Value) -> Vec<Value> {
    let items = v["models"].as_array().or_else(|| v["data"].as_array()).or_else(|| v.as_array()).cloned().unwrap_or_default();
    let any_visibility = items.iter().any(|m| m.get("visibility").is_some());
    items.iter().filter(|m| !any_visibility || m["visibility"].as_str() == Some("list")).filter_map(|m| {
        let slug = m["slug"].as_str().or_else(|| m["id"].as_str())?;
        Some(json!({"slug": slug, "name": m["display_name"].as_str().unwrap_or(slug)}))
    }).collect()
}

fn map_error(status: u16, body: &str) -> Fail {
    let t = body.to_lowercase();
    let detail = serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or_default();
    let has = |k: &str| t.contains(k);
    if has("subscription_sharing_usage_limit_exceeded") {
        (false, format!("chatgpt returned 429: your ChatGPT plan usage quota is used up. Open ChatGPT Settings, then Usage, to see when it frees up. {detail}"))
    } else if has("subscription_sharing_user_not_eligible") {
        (false, format!("chatgpt returned 403: this ChatGPT account cannot share its plan with apps (Plus or Pro is needed). {detail}"))
    } else if has("subscription_sharing_usage_unavailable") || has("subscription_sharing_user_unavailable") {
        (true, format!("chatgpt returned 503: ChatGPT plan usage is not available right now. {detail}"))
    } else if has("subscription_sharing_unsupported_capability") {
        (false, format!("chatgpt returned 400: the plan does not allow that request. {detail}"))
    } else if has("subscription_sharing_route_not_supported") {
        (false, format!("chatgpt returned 403: this request type is not available with ChatGPT sign-in. {detail}"))
    } else if has("subscription_sharing_invalid_user") || status == 401 {
        (false, format!("chatgpt returned 401: sign in with ChatGPT again. {detail}"))
    } else {
        (matches!(status, 408 | 429 | 500 | 502 | 503 | 504), format!("chatgpt returned {status}: {}", if detail.is_empty() { body.chars().take(200).collect::<String>() } else { detail }))
    }
}

/// Collects one streamed Responses answer. Success only counts after `response.completed`.
#[derive(Default)]
struct Collector {
    text: String,
    done: bool,
    failed: Option<String>,
}

impl Collector {
    fn feed(&mut self, ev: &Value) {
        match ev["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => self.text.push_str(ev["delta"].as_str().unwrap_or("")),
            "response.completed" => {
                self.done = true;
                if self.text.is_empty() {
                    for item in ev["response"]["output"].as_array().into_iter().flatten() {
                        for part in item["content"].as_array().into_iter().flatten() {
                            if let Some(t) = part["text"].as_str() {
                                self.text.push_str(t);
                            }
                        }
                    }
                }
            }
            "response.failed" => self.failed = Some(ev["response"]["error"]["message"].as_str().unwrap_or("ChatGPT could not finish the answer.").to_string()),
            "response.incomplete" => self.failed = Some(format!("ChatGPT stopped early ({}).", ev["response"]["incomplete_details"]["reason"].as_str().unwrap_or("unknown reason"))),
            "error" => self.failed = Some(ev["message"].as_str().or_else(|| ev["error"]["message"].as_str()).unwrap_or("ChatGPT returned an error.").to_string()),
            _ => {}
        }
    }

    fn feed_block(&mut self, block: &str) {
        let data: Vec<&str> = block.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.trim_start()).collect();
        let joined = data.join("\n");
        if joined.is_empty() || joined == "[DONE]" {
            return;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&joined) {
            self.feed(&v);
        }
    }
}

/// One streamed answer from ChatGPT for a system + user prompt.
pub async fn generate(state: &State, model: &str, system: &str, user: &str, timeout: Duration) -> std::result::Result<String, Fail> {
    use futures_util::StreamExt;
    let token = access_token(state).await?;
    let model = if model.trim().is_empty() { chosen_model(state) } else { model.trim().to_string() };
    if model.is_empty() {
        return Err((false, "chatgpt: pick a ChatGPT model in Settings (404).".into()));
    }
    let body = json!({
        "model": model,
        "instructions": system,
        "input": [{"role": "user", "content": [{"type": "input_text", "text": user}]}],
        "store": false,
        "stream": true,
    });
    let net = |_: reqwest::Error| (true, "Network error talking to chatgpt.".to_string());
    let resp = state.http.post(format!("{API}/responses")).bearer_auth(token).header("accept", "text/event-stream").json(&body).timeout(timeout).send().await.map_err(net)?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(map_error(status.as_u16(), &text));
    }
    let mut c = Collector::default();
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(net)?;
        buf.extend(chunk.iter().filter(|b| **b != b'\r'));
        while let Some(i) = buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = buf.drain(..i + 2).collect();
            c.feed_block(&String::from_utf8_lossy(&block));
        }
        if c.done || c.failed.is_some() {
            break;
        }
    }
    if !buf.is_empty() && !c.done && c.failed.is_none() {
        c.feed_block(&String::from_utf8_lossy(&buf));
    }
    if let Some(f) = c.failed {
        return Err((true, format!("chatgpt: {f}")));
    }
    if !c.done {
        return Err((true, "chatgpt: the answer stopped before it was complete.".into()));
    }
    if c.text.trim().is_empty() {
        return Err((true, "chatgpt returned an empty answer".into()));
    }
    Ok(c.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_with(claims: Value) -> String {
        let e = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!("{}.{}.sig", e(b"{\"alg\":\"RS256\"}"), e(claims.to_string().as_bytes()))
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn first_sign_in_registers_and_later_ones_do_not() {
        let first = authorize_url(DYNAMIC_CLIENT, "urn:uuid:abc", "http://127.0.0.1:5000/auth/callback", "st", "no", "ch");
        assert!(first.starts_with(AUTHORIZE));
        assert!(first.contains("client_id=dynamic_agent_client"));
        assert!(first.contains("agent_name_hint=Nexora%20Clip"));
        assert!(first.contains("ext_agent_host_id=urn%3Auuid%3Aabc"));
        assert!(first.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fauth%2Fcallback"));
        assert!(first.contains("code_challenge_method=S256"));
        assert!(first.contains("chatgpt.tokens.use.direct"));
        assert!(first.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"));
        let later = authorize_url("oaiapp_123", "urn:uuid:abc", "http://127.0.0.1:5000/auth/callback", "st", "no", "ch");
        assert!(later.contains("client_id=oaiapp_123"));
        assert!(!later.contains("agent_name_hint"));
    }

    #[test]
    fn id_token_claims_are_checked() {
        let ok = token_with(json!({"iss": ISSUER, "aud": ["x", "oaiapp_1"], "exp": 2000, "nonce": "n1", "sub": "user-1", "email": "a@b.c"}));
        assert_eq!(id_claims(&ok, "oaiapp_1", "n1", 1000).unwrap(), ("user-1".to_string(), "a@b.c".to_string()));
        assert!(id_claims(&ok, "oaiapp_1", "other", 1000).is_err());
        assert!(id_claims(&ok, "oaiapp_2", "n1", 1000).is_err());
        assert!(id_claims(&ok, "oaiapp_1", "n1", 3000).is_err());
        let bad_iss = token_with(json!({"iss": "https://evil.example", "aud": "oaiapp_1", "exp": 2000, "nonce": "n1", "sub": "u"}));
        assert!(id_claims(&bad_iss, "oaiapp_1", "n1", 1000).is_err());
        assert!(id_claims("garbage", "oaiapp_1", "n1", 1000).is_err());
    }

    #[test]
    fn streamed_answers_count_only_after_completed() {
        let mut c = Collector::default();
        c.feed_block("event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"ok\\\":\"}\n\n");
        c.feed_block("data: {\"type\":\"response.output_text.delta\",\"delta\":\" true}\"}\n\n");
        assert!(!c.done);
        c.feed_block("data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n");
        assert!(c.done);
        assert_eq!(c.text, "{\"ok\": true}");
        let mut f = Collector::default();
        f.feed_block("data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n");
        assert_eq!(f.failed.as_deref(), Some("boom"));
        let mut i = Collector::default();
        i.feed_block("data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n");
        assert!(i.failed.unwrap().contains("max_output_tokens"));
    }

    #[test]
    fn completed_event_can_carry_the_text() {
        let mut c = Collector::default();
        c.feed(&json!({"type": "response.completed", "response": {"output": [{"content": [{"type": "output_text", "text": "hi"}]}]}}));
        assert_eq!(c.text, "hi");
    }

    #[test]
    fn plan_errors_become_clear_messages() {
        let (retry, msg) = map_error(429, "{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}");
        assert!(!retry);
        assert!(crate::quota::is_quota(&msg));
        assert!(map_error(403, "subscription_sharing_user_not_eligible").1.contains("Plus or Pro"));
        assert!(map_error(503, "subscription_sharing_usage_unavailable").0);
        assert!(!map_error(401, "").0);
        assert!(map_error(500, "oops").0);
    }

    #[test]
    fn only_listed_models_are_offered() {
        let v = json!({"models": [
            {"slug": "a", "display_name": "Model A", "visibility": "list"},
            {"slug": "b", "display_name": "Hidden", "visibility": "hide"},
        ]});
        let m = listed_models(&v);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["slug"], "a");
        assert_eq!(m[0]["name"], "Model A");
        assert_eq!(listed_models(&json!({"data": [{"id": "x"}]}))[0]["slug"], "x");
    }
}
