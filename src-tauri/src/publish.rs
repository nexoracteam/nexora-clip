//! Publishing to YouTube (Shorts), Facebook Page Reels and TikTok.
//! Personal accounts, your own API apps. No request that can create a post is ever retried blindly.

use crate::models::Clip;
use crate::state::{now, State};
use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GRAPH: &str = "https://graph.facebook.com/v21.0/";
const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
const YT_SCOPES: &str = "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly";
const META_SCOPES: [&str; 3] = ["pages_show_list", "pages_read_engagement", "pages_manage_posts"];
pub const TIKTOK_PORT: u16 = 17863;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Vault {
    pub youtube: Option<YouTube>,
    pub meta: Option<Meta>,
    pub tiktok: Option<TikTok>,
}
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct YouTube { pub client_id: String, pub client_secret: String, pub refresh_token: String, pub id: String, pub name: String }
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Page { pub id: String, pub name: String, pub token: String }
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Meta { pub pages: Vec<Page>, pub selected: String }
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TikTok { pub client_key: String, pub client_secret: String, pub access_token: String, pub refresh_token: String, pub open_id: String, pub name: String, pub expires_at: i64 }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Receipt {
    pub id: String,
    pub provider: String,
    pub account_name: String,
    pub clip_id: String,
    pub title: String,
    pub status: String, // queued | uploading | processing | published | uploaded | failed | uncertain
    pub progress: i64,
    pub message: String,
    pub url: String,
    pub created: String,
}

pub fn vault(state: &State) -> Vault {
    state.kv_get("publish_vault").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_vault(state: &State, v: &Vault) -> Result<()> {
    state.kv_set("publish_vault", &serde_json::to_string(v)?)
}
pub fn history(state: &State) -> Vec<Receipt> {
    state.kv_get("publish_history").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_history(state: &State, h: &[Receipt]) {
    let _ = state.kv_set("publish_history", &serde_json::to_string(h).unwrap_or_default());
}
fn update_receipt(app: &AppHandle, state: &State, id: &str, f: impl FnOnce(&mut Receipt)) {
    let mut h = history(state);
    if let Some(r) = h.iter_mut().find(|r| r.id == id) {
        f(r);
        let r = r.clone();
        save_history(state, &h);
        let _ = app.emit("publish-update", &r);
    }
}

pub fn status(state: &State) -> Value {
    let v = vault(state);
    let auth = state.auth_status.lock().unwrap().clone();
    json!({
        "youtube": {"configured": v.youtube.as_ref().map(|y| !y.client_id.is_empty()).unwrap_or(false),
                    "connected": v.youtube.as_ref().map(|y| !y.refresh_token.is_empty()).unwrap_or(false),
                    "name": v.youtube.as_ref().map(|y| y.name.clone()).unwrap_or_default(), "message": auth.get("youtube")},
        "facebook": {"connected": v.meta.as_ref().map(|m| !m.pages.is_empty()).unwrap_or(false),
                     "selected": v.meta.as_ref().map(|m| m.selected.clone()).unwrap_or_default(),
                     "pages": v.meta.as_ref().map(|m| m.pages.iter().map(|p| json!({"id": p.id, "name": p.name})).collect::<Vec<_>>()).unwrap_or_default()},
        "tiktok": {"configured": v.tiktok.as_ref().map(|t| !t.client_key.is_empty()).unwrap_or(false),
                   "connected": v.tiktok.as_ref().map(|t| !t.refresh_token.is_empty()).unwrap_or(false),
                   "name": v.tiktok.as_ref().map(|t| t.name.clone()).unwrap_or_default(), "message": auth.get("tiktok"),
                   "redirect": format!("http://127.0.0.1:{TIKTOK_PORT}/callback/")},
    })
}

pub fn disconnect(state: &State, provider: &str) -> Result<()> {
    let mut v = vault(state);
    match provider {
        "youtube" => v.youtube = None,
        "facebook" => v.meta = None,
        "tiktok" => v.tiktok = None,
        _ => bail!("Unknown provider"),
    }
    state.auth_status.lock().unwrap().remove(provider);
    save_vault(state, &v)
}

// ---------- shared HTTP ----------

async fn send_json(req: reqwest::RequestBuilder) -> Result<Value> {
    let resp = req.send().await.map_err(|_| anyhow!("Connection interrupted. Check your internet and the upload history before retrying."))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = v["error"]["message"].as_str().or(v["error"]["error_user_msg"].as_str()).or(v["error_description"].as_str())
            .or(v["error"]["message"].as_str()).map(String::from)
            .or_else(|| v["error"].as_str().map(String::from))
            .unwrap_or_else(|| text.chars().take(200).collect());
        let hint = match status.as_u16() {
            401 => " Reconnect the account in Publishing.",
            403 => " Check app permissions, account roles and API quota.",
            429 => " Rate limit reached; wait before trying again.",
            _ => "",
        };
        bail!("Provider rejected the request (HTTP {}): {}.{}", status.as_u16(), detail.trim_end_matches('.'), hint);
    }
    Ok(v)
}

fn pkce() -> (String, String) {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let digest = Sha256::digest(verifier.as_bytes());
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let _ = &b64;
    (verifier, hex::encode(digest))
}
fn pkce_b64(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Wait for one browser redirect on 127.0.0.1 and return its query parameters.
async fn wait_callback(listener: TcpListener, state_nonce: &str, brand: &str) -> Result<std::collections::HashMap<String, String>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let (mut sock, _) = tokio::time::timeout_at(deadline, listener.accept()).await.map_err(|_| anyhow!("Sign-in timed out. Press Connect again."))??;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let line = req.lines().next().unwrap_or("");
        let path = line.split_whitespace().nth(1).unwrap_or("");
        let mut query = std::collections::HashMap::new();
        if let Some((_, q)) = path.split_once('?') {
            for pair in q.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    let v = v.replace('+', " ");
                    let decoded: Vec<u8> = {
                        let b = v.as_bytes();
                        let mut out = Vec::new();
                        let mut i = 0;
                        while i < b.len() {
                            if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 + 0 {
                                if let Ok(x) = u8::from_str_radix(&v[i + 1..i + 3], 16) { out.push(x); i += 3; continue; }
                            }
                            out.push(b[i]);
                            i += 1;
                        }
                        out
                    };
                    query.insert(k.to_string(), String::from_utf8_lossy(&decoded).to_string());
                }
            }
        }
        let valid = path.starts_with("/callback") && query.get("state").map(|s| s == state_nonce).unwrap_or(false);
        let body = if valid { format!("<html><body style='font-family:sans-serif;padding:40px'><h2>{brand} connected</h2><p>You can close this tab and return to Nexora Clip.</p></body></html>") } else { "Invalid sign-in response.".to_string() };
        let _ = sock.write_all(format!("HTTP/1.1 {} OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", if valid { 200 } else { 400 }, body.len(), body).as_bytes()).await;
        if valid {
            return Ok(query);
        }
    }
}

fn set_auth(app: &AppHandle, state: &State, provider: &str, message: &str) {
    state.auth_status.lock().unwrap().insert(provider.to_string(), message.to_string());
    let _ = app.emit("publish-auth", json!({"provider": provider, "message": message}));
}

// ---------- YouTube ----------

pub fn youtube_configure(state: &State, document: &str) -> Result<()> {
    let v: Value = serde_json::from_str(document).map_err(|_| anyhow!("That is not valid JSON. Import the OAuth client JSON you downloaded from Google Cloud."))?;
    let client = v.get("installed").ok_or_else(|| anyhow!("Import the OAuth client JSON of type \"Desktop app\" (it starts with {{\"installed\":…)."))?;
    let client_id = client["client_id"].as_str().unwrap_or("");
    if !client_id.ends_with(".apps.googleusercontent.com") {
        bail!("The client ID is missing or malformed.");
    }
    let mut vt = vault(state);
    vt.youtube = Some(YouTube { client_id: client_id.into(), client_secret: client["client_secret"].as_str().unwrap_or("").into(), ..Default::default() });
    save_vault(state, &vt)
}

pub async fn youtube_login(app: AppHandle, state: State) -> Result<()> {
    let client = vault(&state).youtube.filter(|y| !y.client_id.is_empty()).ok_or_else(|| anyhow!("Import your Google OAuth client JSON first."))?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
    let nonce = uuid::Uuid::new_v4().to_string();
    let (verifier, _) = pkce();
    let url = format!("https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        pct(&client.client_id), pct(&redirect), pct(YT_SCOPES), nonce, pkce_b64(&verifier));
    set_auth(&app, &state, "youtube", "Waiting for Google sign-in in your browser…");
    app.opener().open_url(url, None::<&str>).map_err(|e| anyhow!("Could not open the browser: {e}"))?;
    tauri::async_runtime::spawn(async move {
        let result: Result<()> = async {
            let q = wait_callback(listener, &nonce, "YouTube").await?;
            let code = q.get("code").ok_or_else(|| anyhow!("Sign-in was cancelled."))?;
            let t = send_json(state.http.post(GOOGLE_TOKEN).form(&[("code", code.as_str()), ("client_id", &client.client_id), ("client_secret", &client.client_secret),
                ("redirect_uri", &redirect), ("grant_type", "authorization_code"), ("code_verifier", &verifier)])).await?;
            let refresh = t["refresh_token"].as_str().ok_or_else(|| anyhow!("Google did not return a refresh token. Remove the app at myaccount.google.com/permissions and connect again."))?.to_string();
            let scope = t["scope"].as_str().unwrap_or("");
            if !YT_SCOPES.split(' ').all(|s| scope.contains(s)) {
                bail!("Google did not grant both upload and channel access. Connect again and allow both permissions.");
            }
            let ch = send_json(state.http.get("https://www.googleapis.com/youtube/v3/channels").bearer_auth(t["access_token"].as_str().unwrap_or("")).query(&[("part", "snippet"), ("mine", "true")])).await?;
            let item = ch["items"].get(0).ok_or_else(|| anyhow!("This Google account has no YouTube channel. Create a channel, then connect again."))?;
            let mut v = vault(&state);
            v.youtube = Some(YouTube { refresh_token: refresh, id: item["id"].as_str().unwrap_or("").into(), name: item["snippet"]["title"].as_str().unwrap_or("").into(), ..client.clone() });
            save_vault(&state, &v)?;
            Ok(())
        }.await;
        set_auth(&app, &state, "youtube", &match result { Ok(()) => "YouTube connected.".to_string(), Err(e) => e.to_string() });
    });
    Ok(())
}

async fn youtube_token(state: &State, y: &YouTube) -> Result<String> {
    let t = send_json(state.http.post(GOOGLE_TOKEN).form(&[("client_id", y.client_id.as_str()), ("client_secret", &y.client_secret), ("refresh_token", &y.refresh_token), ("grant_type", "refresh_token")])).await?;
    Ok(t["access_token"].as_str().ok_or_else(|| anyhow!("Google did not return an access token. Reconnect YouTube."))?.to_string())
}

async fn upload_youtube(app: &AppHandle, state: &State, rid: &str, file: &Path, y: &YouTube, title: &str, description: &str, privacy: &str, kids: bool) -> Result<String> {
    let token = youtube_token(state, y).await?;
    let size = tokio::fs::metadata(file).await?.len();
    let init = state.http.post("https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status")
        .bearer_auth(&token).header("X-Upload-Content-Length", size).header("X-Upload-Content-Type", "video/mp4")
        .json(&json!({"snippet": {"title": title, "description": description, "categoryId": "22"},
                      "status": {"privacyStatus": privacy, "selfDeclaredMadeForKids": kids}}))
        .send().await.map_err(|_| anyhow!("Connection interrupted."))?;
    if !init.status().is_success() {
        let body = init.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or(body.chars().take(200).collect());
        bail!("YouTube refused the upload: {msg}");
    }
    let location = init.headers().get("location").and_then(|l| l.to_str().ok()).ok_or_else(|| anyhow!("YouTube returned no upload address."))?.to_string();
    update_receipt(app, state, rid, |r| { r.status = "uploading".into(); r.message = "Uploading to YouTube".into(); });
    let mut f = tokio::fs::File::open(file).await?;
    const CHUNK: u64 = 8 * 1024 * 1024;
    let mut sent: u64 = 0;
    loop {
        let len = CHUNK.min(size - sent);
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).await?;
        let resp = state.http.put(&location).header("Content-Length", len).header("Content-Range", format!("bytes {}-{}/{}", sent, sent + len - 1, size)).body(buf)
            .timeout(std::time::Duration::from_secs(600)).send().await.map_err(|_| anyhow!("Connection interrupted during upload."))?;
        sent += len;
        let code = resp.status().as_u16();
        let pct_done = (sent as f64 / size as f64 * 95.0) as i64;
        update_receipt(app, state, rid, |r| r.progress = pct_done);
        if code == 200 || code == 201 {
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            return v["id"].as_str().map(String::from).ok_or_else(|| anyhow!("YouTube did not return a video id. Check YouTube Studio."));
        }
        if code != 308 {
            bail!("YouTube upload failed (HTTP {code}).");
        }
    }
}

// ---------- Facebook ----------

async fn graph(state: &State, path: &str, token: &str, post: bool, fields: &[(&str, &str)]) -> Result<Value> {
    let url = format!("{GRAPH}{path}");
    let req = if post { state.http.post(url).bearer_auth(token).form(fields) } else { state.http.get(url).bearer_auth(token).query(fields) };
    send_json(req).await
}

pub async fn facebook_connect(state: &State, app_id: &str, app_secret: &str, user_token: &str) -> Result<()> {
    let (app_id, app_secret, user_token) = (app_id.trim(), app_secret.trim(), user_token.trim());
    if app_id.is_empty() || app_secret.is_empty() || user_token.is_empty() {
        bail!("Enter the Meta app ID, app secret and the access token.");
    }
    let dbg = graph(state, "debug_token", &format!("{app_id}|{app_secret}"), false, &[("input_token", user_token)]).await?;
    let d = &dbg["data"];
    if d["is_valid"].as_bool() != Some(true) || d["app_id"].as_str() != Some(app_id) {
        bail!("This token is expired or belongs to a different Meta app.");
    }
    let scopes: Vec<String> = d["scopes"].as_array().cloned().unwrap_or_default().iter().filter_map(|s| s.as_str().map(String::from)).collect();
    let missing: Vec<&str> = META_SCOPES.iter().copied().filter(|s| !scopes.iter().any(|x| x == s)).collect();
    if !missing.is_empty() {
        bail!("The token is missing these permissions: {}.", missing.join(", "));
    }
    let long = graph(state, "oauth/access_token", user_token, false, &[("grant_type", "fb_exchange_token"), ("client_id", app_id), ("client_secret", app_secret), ("fb_exchange_token", user_token)]).await?;
    let token = long["access_token"].as_str().ok_or_else(|| anyhow!("Meta did not return a long-lived token."))?.to_string();
    let pages_json = graph(state, "me/accounts", &token, false, &[("fields", "id,name,access_token,tasks"), ("limit", "100")]).await?;
    let mut pages = Vec::new();
    for p in pages_json["data"].as_array().cloned().unwrap_or_default() {
        let tasks: Vec<&str> = p["tasks"].as_array().map(|a| a.iter().filter_map(|t| t.as_str()).collect()).unwrap_or_default();
        if p["access_token"].as_str().is_some() && tasks.iter().any(|t| ["CREATE_CONTENT", "MANAGE", "PROFILE_PLUS_CREATE_CONTENT"].contains(t)) {
            pages.push(Page { id: p["id"].as_str().unwrap_or("").into(), name: p["name"].as_str().unwrap_or("").into(), token: p["access_token"].as_str().unwrap_or("").into() });
        }
    }
    if pages.is_empty() {
        bail!("No Facebook Pages you can post to were found. Make sure you are an admin of the Page and granted it to the app.");
    }
    let mut v = vault(state);
    let old = v.meta.as_ref().map(|m| m.selected.clone()).unwrap_or_default();
    let selected = if pages.iter().any(|p| p.id == old) { old } else { pages[0].id.clone() };
    v.meta = Some(Meta { pages, selected });
    save_vault(state, &v)
}

pub fn facebook_select(state: &State, id: &str) -> Result<()> {
    let mut v = vault(state);
    let m = v.meta.as_mut().ok_or_else(|| anyhow!("Connect Facebook first."))?;
    if !m.pages.iter().any(|p| p.id == id) {
        bail!("That Page is not connected.");
    }
    m.selected = id.to_string();
    save_vault(state, &v)
}

async fn upload_facebook(app: &AppHandle, state: &State, rid: &str, file: &Path, page: &Page, title: &str, description: &str) -> Result<String> {
    let size = tokio::fs::metadata(file).await?.len();
    if size > 1_000_000_000 {
        bail!("Facebook accepts videos up to 1 GB; render a shorter clip.");
    }
    let created = graph(state, &format!("{}/video_reels", page.id), &page.token, true, &[("upload_phase", "start")]).await?;
    let video_id = created["video_id"].as_str().map(String::from).or_else(|| created["video_id"].as_i64().map(|n| n.to_string())).ok_or_else(|| anyhow!("Facebook did not start the upload."))?;
    let upload_url = created["upload_url"].as_str().ok_or_else(|| anyhow!("Facebook returned no upload address."))?.to_string();
    update_receipt(app, state, rid, |r| { r.status = "uploading".into(); r.message = "Uploading to Facebook".into(); r.progress = 10; });
    let body = tokio::fs::read(file).await?;
    let resp = state.http.post(&upload_url).header("Authorization", format!("OAuth {}", page.token)).header("offset", "0").header("file_size", size.to_string()).body(body)
        .timeout(std::time::Duration::from_secs(900)).send().await.map_err(|_| anyhow!("Connection interrupted during upload."))?;
    if !resp.status().is_success() {
        bail!("Facebook rejected the video data (HTTP {}).", resp.status().as_u16());
    }
    update_receipt(app, state, rid, |r| { r.status = "processing".into(); r.message = "Facebook is processing the video".into(); r.progress = 80; });
    let fin = graph(state, &format!("{}/video_reels", page.id), &page.token, true, &[("upload_phase", "finish"), ("video_id", &video_id), ("video_state", "PUBLISHED"), ("title", title), ("description", description)]).await?;
    if fin["success"].as_bool() != Some(true) {
        bail!("Facebook did not confirm the publish request.");
    }
    for _ in 0..120 {
        let st = graph(state, &video_id, &page.token, false, &[("fields", "status")]).await?;
        let s = &st["status"];
        if s["publishing_phase"]["status"].as_str() == Some("complete") {
            return Ok(video_id);
        }
        if ["uploading_phase", "processing_phase", "publishing_phase"].iter().any(|k| s[k]["status"].as_str() == Some("error")) {
            bail!("Facebook could not process this Reel. Check the Page before retrying.");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    bail!("Facebook is still processing. Check the Page for the final result.")
}

// ---------- TikTok ----------

pub fn tiktok_configure(state: &State, key: &str, secret: &str) -> Result<()> {
    if key.trim().is_empty() || secret.trim().is_empty() {
        bail!("Enter both the client key and the client secret.");
    }
    let mut v = vault(state);
    v.tiktok = Some(TikTok { client_key: key.trim().into(), client_secret: secret.trim().into(), ..Default::default() });
    save_vault(state, &v)
}

pub async fn tiktok_login(app: AppHandle, state: State) -> Result<()> {
    let t = vault(&state).tiktok.filter(|t| !t.client_key.is_empty()).ok_or_else(|| anyhow!("Enter your TikTok client key and secret first."))?;
    let listener = TcpListener::bind(("127.0.0.1", TIKTOK_PORT)).await.map_err(|_| anyhow!("Port {TIKTOK_PORT} is busy. Close the other sign-in window and try again."))?;
    let redirect = format!("http://127.0.0.1:{TIKTOK_PORT}/callback/");
    let nonce = uuid::Uuid::new_v4().to_string();
    let (verifier, challenge) = pkce();
    let url = format!("https://www.tiktok.com/v2/auth/authorize/?client_key={}&scope={}&response_type=code&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
        pct(&t.client_key), pct("user.info.basic,video.publish"), pct(&redirect), nonce, challenge);
    set_auth(&app, &state, "tiktok", "Waiting for TikTok sign-in in your browser…");
    app.opener().open_url(url, None::<&str>).map_err(|e| anyhow!("Could not open the browser: {e}"))?;
    tauri::async_runtime::spawn(async move {
        let result: Result<()> = async {
            let q = wait_callback(listener, &nonce, "TikTok").await?;
            let code = q.get("code").ok_or_else(|| anyhow!("Sign-in was cancelled: {}", q.get("error_description").cloned().unwrap_or_default()))?;
            let tok = send_json(state.http.post("https://open.tiktokapis.com/v2/oauth/token/").form(&[("client_key", t.client_key.as_str()), ("client_secret", &t.client_secret),
                ("code", code), ("grant_type", "authorization_code"), ("redirect_uri", &redirect), ("code_verifier", &verifier)])).await?;
            let access = tok["access_token"].as_str().ok_or_else(|| anyhow!("TikTok did not return a token: {}", tok["error_description"].as_str().unwrap_or("unknown error")))?.to_string();
            let info = send_json(state.http.get("https://open.tiktokapis.com/v2/user/info/").bearer_auth(&access).query(&[("fields", "display_name")])).await.unwrap_or(Value::Null);
            let mut v = vault(&state);
            v.tiktok = Some(TikTok { access_token: access, refresh_token: tok["refresh_token"].as_str().unwrap_or("").into(), open_id: tok["open_id"].as_str().unwrap_or("").into(),
                name: info["data"]["user"]["display_name"].as_str().unwrap_or("TikTok account").into(),
                expires_at: chrono::Utc::now().timestamp() + tok["expires_in"].as_i64().unwrap_or(86400) - 120, ..t.clone() });
            save_vault(&state, &v)?;
            Ok(())
        }.await;
        set_auth(&app, &state, "tiktok", &match result { Ok(()) => "TikTok connected.".to_string(), Err(e) => e.to_string() });
    });
    Ok(())
}

async fn tiktok_token(state: &State) -> Result<TikTok> {
    let mut v = vault(state);
    let t = v.tiktok.clone().filter(|t| !t.refresh_token.is_empty()).ok_or_else(|| anyhow!("Connect TikTok first."))?;
    if chrono::Utc::now().timestamp() < t.expires_at {
        return Ok(t);
    }
    let tok = send_json(state.http.post("https://open.tiktokapis.com/v2/oauth/token/").form(&[("client_key", t.client_key.as_str()), ("client_secret", &t.client_secret),
        ("grant_type", "refresh_token"), ("refresh_token", &t.refresh_token)])).await?;
    let access = tok["access_token"].as_str().ok_or_else(|| anyhow!("TikTok sign-in expired. Reconnect TikTok."))?.to_string();
    let n = TikTok { access_token: access, refresh_token: tok["refresh_token"].as_str().unwrap_or(&t.refresh_token).into(), expires_at: chrono::Utc::now().timestamp() + tok["expires_in"].as_i64().unwrap_or(86400) - 120, ..t };
    v.tiktok = Some(n.clone());
    save_vault(state, &v)?;
    Ok(n)
}

async fn upload_tiktok(app: &AppHandle, state: &State, rid: &str, file: &Path, title: &str, privacy: &str) -> Result<String> {
    let t = tiktok_token(state).await?;
    let size = tokio::fs::metadata(file).await?.len();
    let creator = send_json(state.http.post("https://open.tiktokapis.com/v2/post/publish/creator_info/query/").bearer_auth(&t.access_token).header("Content-Type", "application/json; charset=UTF-8").body("{}")).await?;
    if let Some(err) = creator["error"]["code"].as_str().filter(|c| *c != "ok") {
        bail!("TikTok refused: {} ({err})", creator["error"]["message"].as_str().unwrap_or(""));
    }
    let options: Vec<String> = creator["data"]["privacy_level_options"].as_array().cloned().unwrap_or_default().iter().filter_map(|o| o.as_str().map(String::from)).collect();
    // Map our privacy choice to what the account allows; unaudited apps only get SELF_ONLY.
    let wanted = match privacy { "public" => "PUBLIC_TO_EVERYONE", "unlisted" => "MUTUAL_FOLLOW_FRIENDS", _ => "SELF_ONLY" };
    let level = if options.iter().any(|o| o == wanted) { wanted.to_string() } else { options.first().cloned().unwrap_or_else(|| "SELF_ONLY".into()) };
    let (chunk, count) = if size <= 64 * 1024 * 1024 { (size, 1) } else { let c = 32u64 * 1024 * 1024; (c, size / c) };
    let init = send_json(state.http.post("https://open.tiktokapis.com/v2/post/publish/video/init/").bearer_auth(&t.access_token).header("Content-Type", "application/json; charset=UTF-8")
        .json(&json!({"post_info": {"title": title.chars().take(2200).collect::<String>(), "privacy_level": level, "disable_duet": false, "disable_comment": false, "disable_stitch": false},
                      "source_info": {"source": "FILE_UPLOAD", "video_size": size, "chunk_size": chunk, "total_chunk_count": count}}))).await?;
    if let Some(err) = init["error"]["code"].as_str().filter(|c| *c != "ok") {
        let hint = match err { "unaudited_client_can_only_post_to_private_accounts" => " Your TikTok app is not audited yet: set the TikTok account to private, or submit the app for audit.", "spam_risk_too_many_posts" => " Daily post limit reached.", _ => "" };
        bail!("TikTok refused the post: {} ({err}).{hint}", init["error"]["message"].as_str().unwrap_or(""));
    }
    let publish_id = init["data"]["publish_id"].as_str().ok_or_else(|| anyhow!("TikTok returned no publish id."))?.to_string();
    let upload_url = init["data"]["upload_url"].as_str().ok_or_else(|| anyhow!("TikTok returned no upload address."))?.to_string();
    update_receipt(app, state, rid, |r| { r.status = "uploading".into(); r.message = format!("Uploading to TikTok ({level})"); });
    let mut f = tokio::fs::File::open(file).await?;
    let mut sent: u64 = 0;
    for i in 0..count {
        let len = if i + 1 == count { size - sent } else { chunk };
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).await?;
        let resp = state.http.put(&upload_url).header("Content-Type", "video/mp4").header("Content-Length", len).header("Content-Range", format!("bytes {}-{}/{}", sent, sent + len - 1, size))
            .body(buf).timeout(std::time::Duration::from_secs(600)).send().await.map_err(|_| anyhow!("Connection interrupted during upload."))?;
        if !(resp.status().is_success() || resp.status().as_u16() == 206) {
            bail!("TikTok rejected the video data (HTTP {}).", resp.status().as_u16());
        }
        sent += len;
        let p = (sent as f64 / size as f64 * 85.0) as i64;
        update_receipt(app, state, rid, |r| r.progress = p);
    }
    update_receipt(app, state, rid, |r| { r.status = "processing".into(); r.message = "TikTok is processing the video".into(); r.progress = 90; });
    for _ in 0..90 {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let st = send_json(state.http.post("https://open.tiktokapis.com/v2/post/publish/status/fetch/").bearer_auth(&t.access_token).header("Content-Type", "application/json; charset=UTF-8").json(&json!({"publish_id": publish_id}))).await?;
        match st["data"]["status"].as_str() {
            Some("PUBLISH_COMPLETE") => return Ok(publish_id),
            Some("FAILED") => bail!("TikTok failed to publish: {}", st["data"]["fail_reason"].as_str().unwrap_or("unknown reason")),
            _ => {}
        }
    }
    bail!("TikTok is still processing. Check the TikTok app for the final result.")
}

// ---------- orchestration ----------

pub struct Request {
    pub clip: Clip,
    pub platforms: Vec<String>,
    pub title: String,
    pub description: String,
    pub privacy: String,
    pub made_for_kids: bool,
}

pub fn start(app: AppHandle, state: State, req: Request) -> Result<Vec<Receipt>> {
    let title = req.title.trim().to_string();
    if title.is_empty() || title.contains('<') || title.contains('>') {
        bail!("Enter a title without angle brackets.");
    }
    if req.clip.status != "ready" || !Path::new(&req.clip.file_path).is_file() {
        bail!("This clip is not rendered yet.");
    }
    if req.platforms.is_empty() {
        bail!("Choose at least one platform.");
    }
    let v = vault(&state);
    let mut rows = Vec::new();
    for p in &req.platforms {
        let name = match p.as_str() {
            "youtube" => v.youtube.as_ref().filter(|y| !y.refresh_token.is_empty()).map(|y| y.name.clone()),
            "facebook" => v.meta.as_ref().and_then(|m| m.pages.iter().find(|g| g.id == m.selected).map(|g| g.name.clone())),
            "tiktok" => v.tiktok.as_ref().filter(|t| !t.refresh_token.is_empty()).map(|t| t.name.clone()),
            _ => bail!("Unknown platform {p}"),
        };
        let name = name.ok_or_else(|| anyhow!("Connect {} first (Publishing → {}).", p, p))?;
        rows.push(Receipt { id: uuid::Uuid::new_v4().to_string(), provider: p.clone(), account_name: name, clip_id: req.clip.id.clone(), title: title.clone(),
            status: "queued".into(), message: "Queued".into(), created: now(), ..Default::default() });
    }
    let mut h = history(&state);
    h.extend(rows.iter().cloned());
    save_history(&state, &h);
    let ids: Vec<(String, String)> = rows.iter().map(|r| (r.id.clone(), r.provider.clone())).collect();
    let desc = req.description.clone();
    tauri::async_runtime::spawn(async move {
        let _permit = state.publishing.clone().acquire_owned().await;
        let file = std::path::PathBuf::from(&req.clip.file_path);
        for (rid, provider) in ids {
            update_receipt(&app, &state, &rid, |r| { r.status = "uploading".into(); r.message = "Preparing upload".into(); });
            let v = vault(&state);
            let outcome: Result<(String, String)> = match provider.as_str() {
                "youtube" => upload_youtube(&app, &state, &rid, &file, v.youtube.as_ref().unwrap(), &title, &desc, &req.privacy, req.made_for_kids).await.map(|id| (id.clone(), format!("https://www.youtube.com/shorts/{id}"))),
                "facebook" => {
                    let m = v.meta.as_ref().unwrap();
                    let page = m.pages.iter().find(|p| p.id == m.selected).cloned().unwrap_or_default();
                    upload_facebook(&app, &state, &rid, &file, &page, &title, &desc).await.map(|id| (id.clone(), format!("https://www.facebook.com/reel/{id}")))
                }
                _ => upload_tiktok(&app, &state, &rid, &file, &format!("{title}\n{desc}").trim().to_string(), &req.privacy).await.map(|id| (id, String::new())),
            };
            match outcome {
                Ok((_, url)) => update_receipt(&app, &state, &rid, |r| {
                    r.status = if provider == "youtube" { "uploaded".into() } else { "published".into() };
                    r.progress = 100;
                    r.url = url;
                    r.message = if provider == "youtube" { "Uploaded. YouTube may still be processing the video.".into() } else { "Published".into() };
                }),
                Err(e) => update_receipt(&app, &state, &rid, |r| {
                    let late = r.progress >= 80;
                    r.status = if late { "uncertain".into() } else { "failed".into() };
                    r.message = if late { format!("{e} Check the destination before trying again; the post may already exist.") } else { e.to_string() };
                }),
            }
        }
    });
    Ok(rows)
}
