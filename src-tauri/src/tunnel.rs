//! Runs `nexora-tunnel`, the helper bundled with the app that puts the remote-access page on the internet through
//! Tailscale Funnel. The person needs no Tailscale app, only a free Tailscale account they sign in to once.
//! The helper speaks one JSON object per line (see tunnel/main.go); this module keeps the latest state for the UI.

use crate::state::State;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Remembered across runs: the person connected the tunnel and wants it back after a restart.
const ON_KEY: &str = "remote_tunnel_on";

#[derive(Clone, Default)]
struct Info {
    running: bool,
    /// Tailscale's own words: NeedsLogin, Starting, Running...
    state: String,
    login_url: String,
    url: String,
    message: String,
    /// A one-time step the person must do in their Tailscale account, and where.
    action_url: String,
}

static INFO: Mutex<Option<Info>> = Mutex::new(None);
static STOP: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);
/// Each start gets a number; only the newest helper may write the shared state.
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn current(gen: u64) -> bool {
    GEN.load(std::sync::atomic::Ordering::SeqCst) == gen
}

fn with<R>(f: impl FnOnce(&mut Info) -> R) -> R {
    let mut g = INFO.lock().unwrap();
    f(g.get_or_insert_with(Info::default))
}

fn push(app: &AppHandle) {
    let _ = app.emit("remote-tunnel", status());
}

pub fn status() -> Value {
    with(|i| json!({"running": i.running, "state": i.state, "login_url": i.login_url, "url": i.url, "message": i.message, "action_url": i.action_url}))
}

/// The public address, once the helper has reported it.
pub fn url() -> Option<String> {
    with(|i| (i.running && !i.url.is_empty()).then(|| i.url.clone()))
}

pub fn wanted(state: &State) -> bool {
    state.kv_get(ON_KEY).as_deref() == Some("1")
}

fn exe(app: &AppHandle) -> Option<PathBuf> {
    let res = app.path().resource_dir().ok()?;
    let p = res.join("tunnel").join("nexora-tunnel.exe");
    p.is_file().then_some(p)
}

/// Start the helper (no-op when it already runs).
pub fn start(app: &AppHandle, state: &State) -> Result<()> {
    if with(|i| i.running) && STOP.lock().unwrap().is_some() {
        return Ok(());
    }
    let program = exe(app).ok_or_else(|| anyhow!("The built-in tunnel is missing from this install. Reinstall Nexora Clip."))?;
    let port = state.settings().remote_port;
    let mut cmd = crate::ffmpeg::command(&program);
    cmd.arg("--dir").arg(state.data_dir.join("tunnel")).arg("--target").arg(format!("127.0.0.1:{port}"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow!("Could not start the tunnel: {e}"))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("Could not read the tunnel."))?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    *STOP.lock().unwrap() = Some(stop_tx);
    let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    with(|i| *i = Info { running: true, state: "Starting".into(), ..Default::default() });
    push(app);

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _keep_input_open = stdin; // closing it tells the helper to stop
        let mut lines = BufReader::new(stdout).lines();
        let mut opened = String::new();
        let mut stop_rx = stop_rx;
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(l)) => {
                        if !current(gen) {
                            break;
                        }
                        let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
                        match v["event"].as_str().unwrap_or("") {
                            "login" => {
                                let u = v["url"].as_str().unwrap_or("").to_string();
                                if !u.is_empty() && u != opened {
                                    opened = u.clone();
                                    let _ = app2.opener().open_url(u.clone(), None::<&str>);
                                }
                                with(|i| { i.login_url = u; i.message = "Sign in to Tailscale in your browser, then come back.".into(); });
                            }
                            "state" => with(|i| {
                                i.state = v["state"].as_str().unwrap_or("").to_string();
                                if i.state == "Running" { i.login_url.clear(); i.message.clear(); }
                            }),
                            "action" => {
                                let link = v["url"].as_str().unwrap_or("").to_string();
                                if !link.is_empty() && link != opened {
                                    opened = link.clone();
                                    let _ = app2.opener().open_url(link.clone(), None::<&str>);
                                }
                                with(|i| { i.message = v["message"].as_str().unwrap_or("").to_string(); i.action_url = link; i.login_url.clear(); });
                            }
                            "ready" => with(|i| { i.url = v["url"].as_str().unwrap_or("").to_string(); i.login_url.clear(); i.action_url.clear(); i.message.clear(); }),
                            "error" => with(|i| i.message = v["message"].as_str().unwrap_or("The tunnel stopped.").to_string()),
                            _ => {}
                        }
                        push(&app2);
                    }
                    _ => break,
                },
                _ = &mut stop_rx => break,
            }
        }
        let _ = child.kill().await;
        if current(gen) {
            with(|i| { i.running = false; i.url.clear(); i.login_url.clear(); i.state = "Stopped".into(); });
            push(&app2);
        }
    });
    Ok(())
}

pub fn stop() {
    if let Some(tx) = STOP.lock().unwrap().take() {
        let _ = tx.send(());
    }
    GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    with(|i| *i = Info { state: "Stopped".into(), ..Default::default() });
}

/// Connect from the Settings button: remember the wish, then start.
pub fn connect(app: &AppHandle, state: &State) -> Result<()> {
    if !state.settings().remote_enabled {
        bail!("Turn on remote access first.");
    }
    state.kv_set(ON_KEY, "1")?;
    start(app, state)
}

pub fn disconnect(app: &AppHandle, state: &State) {
    let _ = state.kv_set(ON_KEY, "");
    stop();
    push(app);
}

/// Forget this PC's Tailscale sign-in on this PC (the next connect asks to sign in again).
pub fn forget(app: &AppHandle, state: &State) {
    disconnect(app, state);
    let dir = state.data_dir.join("tunnel");
    // Give the helper a moment to let go of its files.
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let _ = std::fs::remove_dir_all(dir);
    });
}
