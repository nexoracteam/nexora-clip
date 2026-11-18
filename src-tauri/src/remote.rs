//! Remote access: a small web server inside the app so a phone (or any browser) can use this PC's Nexora Clip.
//!
//! The server serves the same interface the window shows, a few JSON endpoints that run an allowed list of the
//! app's commands, a live event stream, clip files (with range requests so video plays), and uploads.
//! It listens on 127.0.0.1 by default (so Tailscale Serve can put HTTPS in front of it); "LAN mode" listens on
//! the home network too. Every request needs the pairing cookie, which the QR code / pairing link sets.
//! Keys and tokens never leave the PC: settings are sent with every secret blanked, and saving settings,
//! signing in to accounts, publishing and editing the machine are not offered remotely.

use crate::models::NewTask;
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as AxPath, Query, Request, State as AxState};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, EventId, Listener, Manager};
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const COOKIE: &str = "nx_remote";
const TOKEN_KEY: &str = "remote_token";
const TS_KEY: &str = "remote_ts_url";
const EVENTS: [&str; 8] = ["task-progress", "task-update", "publish-update", "publish-auth", "clip-update", "clip-progress", "tool-progress", "chatgpt-auth"];

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    state: State,
    events: tokio::sync::broadcast::Sender<String>,
}

struct Running {
    stop: tokio::sync::oneshot::Sender<()>,
    listeners: Vec<EventId>,
}

static RUN: Mutex<Option<Running>> = Mutex::new(None);
static ERROR: Mutex<String> = Mutex::new(String::new());
static UP: AtomicBool = AtomicBool::new(false);

// ---------- start / stop ----------

pub fn stop(app: &AppHandle) {
    if let Some(r) = RUN.lock().unwrap().take() {
        let _ = r.stop.send(());
        for id in r.listeners {
            app.unlisten(id);
        }
    }
}

/// Start, stop or restart the server to match the settings.
pub fn reconfigure(app: &AppHandle) {
    stop(app);
    ERROR.lock().unwrap().clear();
    let Some(state) = app.try_state::<State>() else { return };
    let state: State = state.inner().clone();
    let settings = state.settings();
    if !settings.remote_enabled {
        return;
    }
    if token(&state).is_empty() {
        let _ = reset_token(&state);
    }
    let _ = std::fs::remove_dir_all(state.data_dir.join("remote_out"));
    let (tx, _) = tokio::sync::broadcast::channel::<String>(256);
    let mut listeners = Vec::new();
    for name in EVENTS {
        let tx = tx.clone();
        listeners.push(app.listen_any(name, move |ev| {
            let payload: Value = serde_json::from_str(ev.payload()).unwrap_or(Value::Null);
            let _ = tx.send(json!({"event": name, "payload": payload}).to_string());
        }));
    }
    let ctx = Ctx { app: app.clone(), state, events: tx };
    let ip = if settings.remote_lan { IpAddr::V4(Ipv4Addr::UNSPECIFIED) } else { IpAddr::V4(Ipv4Addr::LOCALHOST) };
    let addr = SocketAddr::new(ip, settings.remote_port);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    *RUN.lock().unwrap() = Some(Running { stop: stop_tx, listeners });
    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                UP.store(true, Ordering::SeqCst);
                crate::applog::log(&format!("Remote access listening on {addr}"));
                let served = axum::serve(listener, router(ctx)).with_graceful_shutdown(async move {
                    let _ = stop_rx.await;
                }).await;
                UP.store(false, Ordering::SeqCst);
                if let Err(e) = served {
                    *ERROR.lock().unwrap() = format!("Remote access stopped: {e}");
                }
            }
            Err(e) => {
                *ERROR.lock().unwrap() = format!("Could not start remote access on port {}: {e}. Another program may be using it.", addr.port());
            }
        }
    });
}

fn router(ctx: Ctx) -> Router {
    let protected = Router::new()
        .route("/api/:cmd", post(api))
        .route("/events", get(events))
        .route("/f/:p", get(file))
        .route("/dl/:id/:name", get(download))
        .route("/upload", post(upload))
        .route("/manifest.webmanifest", get(manifest))
        .fallback(get(static_file))
        .layer(middleware::from_fn_with_state(ctx.clone(), auth));
    Router::new().route("/pair", get(pair)).merge(protected).layer(DefaultBodyLimit::disable()).with_state(ctx)
}

// ---------- pairing and auth ----------

fn token(state: &State) -> String {
    state.kv_get(TOKEN_KEY).unwrap_or_default()
}

/// A new pairing token. Phones paired before stop working.
pub fn reset_token(state: &State) -> Result<()> {
    let t = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    state.kv_set(TOKEN_KEY, &t)
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == COOKIE).map(|(_, v)| v.to_string())
}

fn denied() -> Response {
    (StatusCode::UNAUTHORIZED, Html("<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'><body style='font-family:sans-serif;padding:32px;background:#1c1917;color:#eee'><h2>Nexora Clip</h2><p>This phone is not paired yet. On your PC open Settings, Remote access, and scan the QR code with your phone camera (or copy the pairing link). If you just did, scan it again: the link must be the one with <code>/pair?t=</code> in it, not the plain address.</p></body>")).into_response()
}

async fn auth(AxState(ctx): AxState<Ctx>, req: Request, next: Next) -> Response {
    let t = token(&ctx.state);
    let ok = !t.is_empty() && cookie_value(req.headers()).map(|c| same(&c, &t)).unwrap_or(false);
    if ok { next.run(req).await } else { denied() }
}

async fn pair(AxState(ctx): AxState<Ctx>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Response {
    let t = token(&ctx.state);
    if t.is_empty() || !q.get("t").map(|v| same(v, &t)).unwrap_or(false) {
        return denied();
    }
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    // Lax, not Strict: a QR code or a link from another app starts the visit from "outside", and a Strict cookie is
    // then withheld on the very next request. The page below also moves on by script, not by redirect, for the same reason.
    let cookie = format!("{COOKIE}={t}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{}", if secure { "; Secure" } else { "" });
    let mut resp = Html("<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><meta http-equiv=refresh content='1;url=/'><body style='font-family:sans-serif;padding:32px;background:#1c1917;color:#eee'><h2>Nexora Clip</h2><p>Paired. Opening your library...</p><p><a style='color:#f97316' href='/'>Open Nexora Clip</a></p><script>location.replace('/')</script></body>").into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

// ---------- commands ----------

fn a<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> std::result::Result<T, String> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null)).map_err(|_| format!("Missing or invalid \"{key}\""))
}

fn val<T: serde::Serialize>(v: T) -> std::result::Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn mask_secrets(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        for (k, x) in o.iter_mut() {
            if k.ends_with("_api_key") {
                if x.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                    *x = json!("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}");
                }
            }
        }
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.len() < 160 && !name.contains("..") && name.chars().all(|c| c.is_alphanumeric() || " ._-()".contains(c))
}

fn safe_id(id: &str) -> bool {
    id.len() >= 8 && id.len() <= 48 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Files the phone wants saved are written under `remote_out` and then downloaded.
fn out_path(ctx: &Ctx, args: &Value) -> std::result::Result<String, String> {
    let dest: String = a(args, "dest")?;
    let rest = dest.strip_prefix("__remote__/").ok_or("This only works from the phone.")?;
    let (id, name) = rest.split_once('/').ok_or("Bad download name.")?;
    if !safe_id(id) || !safe_name(name) {
        return Err("Bad download name.".into());
    }
    let dir = ctx.state.data_dir.join("remote_out").join(id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name).to_string_lossy().to_string())
}

/// Is `p` inside `root`? Compared by name, never following links, and `..` is refused outright. Only this app writes
/// into the data folder (uploads are plain files), so there is nothing inside it that points elsewhere.
fn inside(root: &Path, p: &Path) -> bool {
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)) {
        return false;
    }
    let norm = |x: &Path| x.to_string_lossy().replace('/', "\\").to_lowercase();
    let prefix = format!("{}\\", norm(root).trim_end_matches('\\'));
    norm(p).starts_with(&prefix)
}

async fn dispatch(ctx: &Ctx, cmd: &str, args: Value) -> std::result::Result<Value, String> {
    let app = &ctx.app;
    let st = || app.state::<State>();
    match cmd {
        "log_client_error" => Ok(Value::Null),
        "app_info" => crate::app_info(st()).await,
        "get_settings" => {
            let mut v = val(crate::get_settings(st()))?;
            mask_secrets(&mut v);
            Ok(v)
        }
        "list_templates" => val(crate::list_templates()),
        "list_fonts" => val(crate::list_fonts(st())),
        "list_tasks" => val(crate::list_tasks(st())?),
        "get_task" => crate::get_task(st(), a(&args, "id")?),
        "create_task" => {
            let input: NewTask = a(&args, "input")?;
            if let Some(p) = input.path.as_deref().filter(|p| !p.trim().is_empty()) {
                if !inside(&ctx.state.data_dir.join("uploads"), Path::new(p.trim())) {
                    return Err("Choose a video from this phone with the upload button.".into());
                }
            }
            val(crate::create_task(st(), app.clone(), input)?)
        }
        "cancel_task" => crate::cancel_task(st(), app.clone(), a(&args, "id")?).map(|_| Value::Null),
        "resume_task" => crate::resume_task(st(), app.clone(), a(&args, "id")?).map(|_| Value::Null),
        "delete_task" => crate::delete_task(st(), a(&args, "id")?).map(|_| Value::Null),
        "probe_url" => val(crate::probe_url(st(), app.clone(), a(&args, "url")?).await?),
        "tools_status" => val(crate::tools_status(st()).await?),
        "tool_updates" => crate::tool_updates(st(), args.get("force").and_then(|v| v.as_bool())).await,
        "list_ollama" => val(crate::list_ollama(st()).await?),
        "groq_status" => Ok(crate::groq_status(st())),
        "chatgpt_status" => Ok(crate::chatgpt_status(st())),
        "ai_usage" => Ok(crate::ai_usage(st())),
        "storage_info" => crate::storage_info(st()).await,
        "publish_status" => Ok(crate::publish_status(st())),
        "publish_history" => val(crate::publish_history(st())),
        "delete_clip" => crate::delete_clip(st(), a(&args, "clipId")?).map(|_| Value::Null),
        "render_candidate" => val(crate::render_candidate(st(), app.clone(), a(&args, "clipId")?)?),
        "clip_words" => crate::clip_words(st(), a(&args, "clipId")?),
        "get_kit" => crate::get_kit(st(), a(&args, "clipId")?),
        "make_kit" => crate::make_kit(st(), a(&args, "clipId")?).await,
        "suggested_name" => val(crate::suggested_name(st(), a(&args, "clipId")?)?),
        "save_clip" => {
            let dest = out_path(ctx, &args)?;
            crate::save_clip(st(), a(&args, "clipId")?, dest).await.map(|_| Value::Null)
        }
        "save_kit" => {
            let dest = out_path(ctx, &args)?;
            crate::save_kit(st(), a(&args, "clipId")?, dest).await.map(|_| Value::Null)
        }
        "export_clip" => {
            let dest = out_path(ctx, &args)?;
            crate::export_clip(st(), a(&args, "clipId")?, a(&args, "preset")?, dest).await.map(|_| Value::Null)
        }
        "export_zip" => {
            let dest = out_path(ctx, &args)?;
            val(crate::export_zip(st(), a(&args, "taskId")?, dest).await?)
        }
        _ => Err("That action is only available on the PC.".into()),
    }
}

async fn api(AxState(ctx): AxState<Ctx>, AxPath(cmd): AxPath<String>, body: axum::body::Bytes) -> Response {
    let args: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body).unwrap_or_else(|_| json!({})) };
    match dispatch(&ctx, &cmd, args).await {
        Ok(v) => Json(v).into_response(),
        Err(m) => (StatusCode::BAD_REQUEST, Json(json!({"error": m}))).into_response(),
    }
}

// ---------- events, files, uploads, pages ----------

async fn events(AxState(ctx): AxState<Ctx>) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = ctx.events.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|m| async move { m.ok().map(|d| Ok(Event::default().data(d))) });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn serve_path(path: PathBuf, req: Request, attachment: Option<String>) -> Response {
    let mut resp = match ServeFile::new(path).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if let Some(name) = attachment {
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
}

/// `/f/<base64url path>`: only files inside the app's data folder.
async fn file(AxState(ctx): AxState<Ctx>, AxPath(p): AxPath<String>, Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')) else { return StatusCode::BAD_REQUEST.into_response() };
    let path = PathBuf::from(String::from_utf8_lossy(&bytes).to_string());
    if !inside(&ctx.state.data_dir, &path) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let name = q.get("dl").map(|_| path.file_name().map(|n| n.to_string_lossy().replace('"', "")).unwrap_or_else(|| "file".into()));
    serve_path(path, req, name).await
}

async fn download(AxState(ctx): AxState<Ctx>, AxPath((id, name)): AxPath<(String, String)>, req: Request) -> Response {
    if !safe_id(&id) || !safe_name(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let path = ctx.state.data_dir.join("remote_out").join(&id).join(&name);
    serve_path(path, req, Some(name.replace('"', ""))).await
}

async fn upload(AxState(ctx): AxState<Ctx>, Query(q): Query<HashMap<String, String>>, body: Body) -> Response {
    let name = q.get("name").cloned().unwrap_or_default();
    let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !safe_name(&name) || !["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a"].contains(&ext.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Choose a video or audio file."}))).into_response();
    }
    let dir = ctx.state.data_dir.join("uploads").join(uuid::Uuid::new_v4().to_string());
    let path = dir.join(&name);
    let result: Result<()> = async {
        tokio::fs::create_dir_all(&dir).await?;
        let mut f = tokio::fs::File::create(&path).await?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            f.write_all(&chunk.map_err(|e| anyhow!("Upload interrupted: {e}"))?).await?;
        }
        f.flush().await?;
        Ok(())
    }.await;
    match result {
        Ok(()) => Json(json!(path.to_string_lossy())).into_response(),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

async fn manifest() -> Response {
    let body = json!({
        "name": "Nexora Clip", "short_name": "Nexora Clip", "start_url": "/", "display": "standalone",
        "background_color": "#1c1917", "theme_color": "#1c1917",
        "icons": [{"src": "/assets/logo.png", "sizes": "512x512", "type": "image/png", "purpose": "any"}]
    });
    ([(header::CONTENT_TYPE, "application/manifest+json")], body.to_string()).into_response()
}

/// The interface files, taken from the same bundle the window uses. The page gets the browser shim first.
async fn static_file(AxState(ctx): AxState<Ctx>, uri: Uri) -> Response {
    let mut path = uri.path().to_string();
    if path == "/" || path.is_empty() {
        path = "/index.html".into();
    }
    let Some(asset) = ctx.app.asset_resolver().get(path.clone()) else { return StatusCode::NOT_FOUND.into_response() };
    let mut bytes = asset.bytes;
    if path == "/index.html" {
        let html = String::from_utf8_lossy(&bytes).to_string();
        let html = html
            .replace("<script src=\"app.js\"></script>", "<script src=\"/remote-shim.js\"></script><script src=\"app.js\"></script>")
            .replace("</head>", "<link rel=\"manifest\" href=\"/manifest.webmanifest\" /><meta name=\"theme-color\" content=\"#1c1917\" /></head>");
        bytes = html.into_bytes();
    }
    let mut resp = Response::new(Body::from(bytes));
    if let Ok(v) = HeaderValue::from_str(&asset.mime_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ---------- status, QR, Tailscale ----------

fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

fn qr_svg(text: &str) -> String {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => code.render::<qrcode::render::svg::Color>().min_dimensions(260, 260).quiet_zone(true).build(),
        Err(_) => String::new(),
    }
}

pub fn status(state: &State) -> Value {
    let s = state.settings();
    let base = state.kv_get(TS_KEY).filter(|u| !u.is_empty());
    let local = format!("http://127.0.0.1:{}", s.remote_port);
    let lan = if s.remote_lan { lan_ip().map(|ip| format!("http://{ip}:{}", s.remote_port)) } else { None };
    let open = base.clone().or(lan.clone()).unwrap_or(local.clone());
    let t = token(state);
    let pair = if s.remote_enabled && !t.is_empty() { Some(format!("{open}/pair?t={t}")) } else { None };
    json!({
        "enabled": s.remote_enabled, "running": UP.load(Ordering::SeqCst), "error": ERROR.lock().unwrap().clone(),
        "lan": s.remote_lan, "port": s.remote_port,
        "local_url": local, "lan_url": lan, "tailscale_url": base,
        "pair_url": pair, "qr_svg": pair.as_deref().map(qr_svg).unwrap_or_default(),
    })
}

fn tailscale_exe() -> Option<PathBuf> {
    for p in ["C:\\Program Files\\Tailscale\\tailscale.exe", "C:\\Program Files (x86)\\Tailscale\\tailscale.exe"] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    Some(PathBuf::from("tailscale"))
}

/// Put HTTPS in front of the server with Tailscale Serve (private to the user's tailnet) and remember the address.
pub async fn tailscale_setup(state: &State) -> Result<String> {
    let exe = tailscale_exe().ok_or_else(|| anyhow!("Install Tailscale first."))?;
    let out = crate::ffmpeg::command(&exe).args(["status", "--json"]).output().await
        .map_err(|_| anyhow!("Tailscale was not found. Install it from tailscale.com/download and sign in, then try again."))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| anyhow!("Tailscale did not answer. Open the Tailscale app and sign in."))?;
    if v["BackendState"].as_str() != Some("Running") {
        bail!("Tailscale is installed but not connected. Open the Tailscale app and sign in.");
    }
    let dns = v["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string();
    if dns.is_empty() {
        bail!("Tailscale gave no address for this PC. Turn on MagicDNS in the Tailscale admin page.");
    }
    let port = state.settings().remote_port;
    let served = crate::ffmpeg::command(&exe).args(["serve", "--bg", &format!("http://127.0.0.1:{port}")]).output().await?;
    if !served.status.success() {
        let text = String::from_utf8_lossy(&served.stderr).to_string() + &String::from_utf8_lossy(&served.stdout);
        bail!("Tailscale Serve did not start: {}", text.trim().chars().take(400).collect::<String>());
    }
    let url = format!("https://{dns}");
    state.kv_set(TS_KEY, &url)?;
    Ok(url)
}

pub fn forget_tailscale(state: &State) {
    let _ = state.kv_set(TS_KEY, "");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_inside_the_data_folder_are_served() {
        let root = Path::new(r"C:\Users\x\AppData\Local\com.nexora.clip");
        assert!(inside(root, Path::new(r"C:\Users\x\AppData\Local\com.nexora.clip\clips\a\b.mp4")));
        assert!(inside(root, Path::new("c:/users/x/appdata/local/com.nexora.clip/clips/b.mp4")));
        assert!(!inside(root, Path::new(r"C:\Windows\win.ini")));
        assert!(!inside(root, Path::new(r"C:\Users\x\AppData\Local\com.nexora.clip\..\..\secret.txt")));
        assert!(!inside(root, Path::new(r"C:\Users\x\AppData\Local\com.nexora.clip-evil\a.mp4")));
        assert!(!inside(root, root));
    }

    #[test]
    fn names_and_ids_are_checked() {
        assert!(safe_name("My clip (1).mp4"));
        assert!(!safe_name("../x.mp4"));
        assert!(!safe_name("a/b.mp4"));
        assert!(!safe_name(""));
        assert!(safe_id("1234abcd-ef"));
        assert!(!safe_id("../../x"));
        assert!(!safe_id("short"));
    }

    #[test]
    fn tokens_compare_in_full() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn secrets_are_blanked_before_they_leave() {
        let mut v = json!({"google_api_key": "AIza-secret", "groq_api_key": "", "llm": "x:y"});
        mask_secrets(&mut v);
        assert_ne!(v["google_api_key"], "AIza-secret");
        assert_eq!(v["groq_api_key"], "");
        assert_eq!(v["llm"], "x:y");
    }
}
