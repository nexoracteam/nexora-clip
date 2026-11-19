//! Live streams: watch a live stream (YouTube, Twitch, Kick), keep the last minutes on disk, and make clips from it.
//!
//! Each watched stream is recorded in short pieces without re-encoding (ffmpeg, `-c copy`), and only the newest
//! ten minutes are kept. The Live tab plays those pieces back one after another as a preview (a few seconds behind).
//!
//! Good moments are caught from three signals, each compared with how this stream has been behaving:
//! the sound getting much louder, a burst of action on screen, and (on YouTube) the chat suddenly speeding up.
//! Moments wait in the Live tab; "Make clip" sends one through the normal pipeline (transcribe, pick, caption, render).

use crate::ffmpeg::{command, tool};
use crate::models::{Task, TaskOptions};
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Length of one buffered piece, in seconds.
const PIECE: i64 = 6;
const KEEP_SECONDS: i64 = 600;
const MAX_STREAMS: usize = 3;
const MOMENTS_KEY: &str = "live_moments";
/// A moment is only called after this many pieces have been seen (two minutes).
const WARMUP_PIECES: usize = 20;

#[derive(Clone, Serialize)]
struct Info {
    id: String,
    url: String,
    title: String,
    channel: String,
    /// connecting | live | waiting | stopped
    status: String,
    message: String,
    started: i64,
    buffered: i64,
    /// Save moments by itself.
    auto: bool,
    /// Also turn the strongest moments into clips by itself (this uses the AI).
    auto_make: bool,
    /// 4.5 more moments, 6 normal, 8 fewer.
    sensitivity: f64,
    /// Loudness of the last piece (dB), action (scene changes in it), and chat messages in the last ten seconds.
    level: f64,
    motion: f64,
    chat: Option<f64>,
    last_spike: i64,
    /// The newest finished pieces, for the preview player.
    pieces: Vec<String>,
}

struct Rt {
    info: Info,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Moment {
    id: String,
    session: String,
    channel: String,
    title: String,
    created: i64,
    seconds: f64,
    /// auto | manual
    kind: String,
    /// Why it was caught, in words ("Chat jumped 4.1×, much louder").
    reason: String,
    /// 1.0 is "worth a look"; higher is stronger.
    strength: f64,
    /// new | made | dismissed
    status: String,
    task_id: String,
    path: String,
    thumb: String,
}

static SESSIONS: Mutex<Vec<Rt>> = Mutex::new(Vec::new());
static MOMENTS: Mutex<Option<Vec<Moment>>> = Mutex::new(None);

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

// ---------- state shared with the screen ----------

fn moments(state: &State) -> Vec<Moment> {
    let mut g = MOMENTS.lock().unwrap();
    g.get_or_insert_with(|| {
        let mut v: Vec<Moment> = state.kv_get(MOMENTS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // Files can disappear (cleaned up, moved): forget moments whose clip is gone.
        v.retain(|m| m.status != "dismissed" && (m.status == "made" || Path::new(&m.path).is_file()));
        v
    }).clone()
}

fn save_moments(state: &State, v: Vec<Moment>) {
    let keep: Vec<Moment> = v.into_iter().rev().take(120).collect::<Vec<_>>().into_iter().rev().collect();
    let _ = state.kv_set(MOMENTS_KEY, &serde_json::to_string(&keep).unwrap_or_else(|_| "[]".into()));
    *MOMENTS.lock().unwrap() = Some(keep);
}

fn edit_moment(state: &State, id: &str, f: impl FnOnce(&mut Moment)) -> Option<Moment> {
    let mut all = moments(state);
    let m = all.iter_mut().find(|m| m.id == id)?;
    f(m);
    let out = m.clone();
    save_moments(state, all);
    Some(out)
}

fn with_info<R>(id: &str, f: impl FnOnce(&mut Info) -> R) -> Option<R> {
    SESSIONS.lock().unwrap().iter_mut().find(|r| r.info.id == id).map(|r| f(&mut r.info))
}

pub fn list(state: &State) -> Value {
    let sessions: Vec<Info> = SESSIONS.lock().unwrap().iter().map(|r| r.info.clone()).collect();
    let mut ms = moments(state);
    ms.reverse();
    json!({"sessions": sessions, "moments": ms, "max_streams": MAX_STREAMS})
}

fn push(app: &AppHandle, state: &State) {
    let _ = app.emit("live-update", list(state));
}

// ---------- starting and stopping ----------

pub async fn start(app: &AppHandle, state: &State, url: &str) -> Result<String> {
    let url = crate::download::check_url(url)?;
    if let Some(r) = SESSIONS.lock().unwrap().iter().find(|r| r.info.url == url && r.info.status != "stopped") {
        return Ok(r.info.id.clone());
    }
    if SESSIONS.lock().unwrap().iter().filter(|r| r.info.status != "stopped").count() >= MAX_STREAMS {
        bail!("You can watch {MAX_STREAMS} streams at once. Stop one first.");
    }
    if !crate::ffmpeg::tool_works(state, "ffmpeg") {
        bail!("FFmpeg is missing. Open Settings, Tools, and install everything missing.");
    }
    crate::tools::ensure_downloader(state, app).await?;
    let (title, channel) = names(state, &url).await;
    let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    SESSIONS.lock().unwrap().retain(|r| r.info.status != "stopped");
    SESSIONS.lock().unwrap().push(Rt {
        info: Info {
            id: id.clone(), url: url.clone(), title, channel, status: "connecting".into(), message: "Opening the stream…".into(), started: now(), buffered: 0,
            auto: true, auto_make: false, sensitivity: 6.0, level: 0.0, motion: 0.0, chat: None, last_spike: 0, pieces: Vec::new(),
        },
        cancel: cancel.clone(),
    });
    push(app, state);
    let (app2, state2, id2) = (app.clone(), state.clone(), id.clone());
    tauri::async_runtime::spawn(async move {
        record(app2.clone(), state2.clone(), id2.clone(), url, cancel).await;
        clean_buffer(&state2.data_dir.join("live").join(&id2));
        with_info(&id2, |i| { i.status = "stopped".into(); i.message = "Stopped".into(); i.pieces.clear(); });
        push(&app2, &state2);
    });
    Ok(id)
}

pub fn stop(app: &AppHandle, state: &State, id: &str) {
    if let Some(r) = SESSIONS.lock().unwrap().iter().find(|r| r.info.id == id) {
        r.cancel.store(true, Ordering::SeqCst);
    }
    with_info(id, |i| { i.status = "stopped".into(); i.message = "Stopped".into(); i.pieces.clear(); });
    push(app, state);
}

/// Remove the buffered pieces of a stream that is no longer watched (saved moments stay).
fn clean_buffer(dir: &Path) {
    for (name, _) in pieces_in(dir) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("chat.") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir.join("moments")); // only succeeds when empty
    let _ = std::fs::remove_dir(dir);
}

/// At start-up: buffers left behind by a crash or a forced quit.
pub fn sweep(state: &State) {
    if let Ok(entries) = std::fs::read_dir(state.data_dir.join("live")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                clean_buffer(&e.path());
            }
        }
    }
}

pub fn stop_all() {
    for r in SESSIONS.lock().unwrap().iter() {
        r.cancel.store(true, Ordering::SeqCst);
    }
}

pub fn set_options(app: &AppHandle, state: &State, id: &str, auto: bool, sensitivity: f64, make: bool) {
    with_info(id, |i| { i.auto = auto; i.auto_make = make; i.sensitivity = sensitivity.clamp(3.0, 12.0); });
    push(app, state);
}

/// Title and channel of the stream, best effort.
async fn names(state: &State, url: &str) -> (String, String) {
    let settings = state.settings();
    let out = command(tool(state, "yt-dlp"))
        .args(crate::download::base_args(state, &settings))
        .args(["--no-warnings", "--skip-download", "--print", "%(title)s\t%(channel,uploader)s", url])
        .stdout(Stdio::piped()).stderr(Stdio::null()).output().await;
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
            let (t, c) = line.split_once('\t').unwrap_or((line, ""));
            let clean = |s: &str| { let s = s.trim(); if s == "NA" { String::new() } else { s.to_string() } };
            return (clean(t), clean(c));
        }
    }
    (url.to_string(), String::new())
}

/// The stream's media address(es) from yt-dlp. A live address expires, so this runs again after every drop.
/// YouTube live streams come as a separate video and audio address, so up to two come back.
async fn resolve(state: &State, url: &str) -> Result<Vec<String>> {
    let settings = state.settings();
    let out = command(tool(state, "yt-dlp"))
        .args(crate::download::base_args(state, &settings))
        .args(["-f", "bv*[height<=720]+ba/b[height<=720]/b", "-g", "--no-warnings", url])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output().await
        .map_err(|e| anyhow!("yt-dlp is not installed ({e})."))?;
    if !out.status.success() {
        bail!("{}", crate::download::friendly_error(&String::from_utf8_lossy(&out.stderr)));
    }
    let urls: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| l.starts_with("http")).take(2).collect();
    if urls.is_empty() {
        bail!("The stream did not give a video address. It may not be live yet.");
    }
    Ok(urls)
}

async fn pause(secs: u64, cancel: &AtomicBool) {
    for _ in 0..secs {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ---------- chat (YouTube) ----------

/// Follows the live chat that yt-dlp writes to disk while the stream runs, and counts messages.
struct Chat {
    dir: PathBuf,
    offset: u64,
    /// Messages and the people behind them, per look (about every five seconds), newest last.
    buckets: VecDeque<(i64, u32, HashSet<String>)>,
    started: i64,
    _child: Option<tokio::process::Child>,
}

impl Chat {
    fn start(state: &State, dir: &Path, url: &str) -> Option<Chat> {
        if !(url.contains("youtube.com") || url.contains("youtu.be")) {
            return None;
        }
        let settings = state.settings();
        let child = command(tool(state, "yt-dlp"))
            .args(crate::download::base_args(state, &settings))
            .args(["--skip-download", "--write-subs", "--sub-langs", "live_chat", "--no-warnings", "-o"]).arg(dir.join("chat")).arg(url)
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;
        Some(Chat { dir: dir.to_path_buf(), offset: 0, buckets: VecDeque::new(), started: now(), _child: Some(child) })
    }

    fn file(&self) -> Option<PathBuf> {
        std::fs::read_dir(&self.dir).ok()?.flatten().map(|e| e.path()).find(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            n.starts_with("chat.live_chat.json") && !n.contains("-Frag")
        })
    }

    /// Read what was added since the last look. The first seconds are history from before we joined, so they are skipped.
    fn poll(&mut self) {
        let Some(path) = self.file() else { return };
        let Ok(mut f) = std::fs::File::open(&path) else { return };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        if size < self.offset {
            self.offset = 0;
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        self.offset += buf.len() as u64;
        let t = now();
        if t - self.started < 20 {
            return; // backlog
        }
        let (msgs, users) = count_chat(&String::from_utf8_lossy(&buf));
        self.buckets.push_back((t, msgs, users));
        while self.buckets.front().map(|b| t - b.0 > 900).unwrap_or(false) {
            self.buckets.pop_front();
        }
    }

    /// Messages and different people in the last ten seconds, once chat has been seen at all.
    fn last_ten(&self) -> Option<(f64, usize)> {
        if self.buckets.is_empty() {
            return None;
        }
        let t = now();
        let mut users: HashSet<&String> = HashSet::new();
        let mut n = 0u32;
        for (_, m, u) in self.buckets.iter().filter(|(ts, _, _)| t - ts <= 10) {
            n += m;
            users.extend(u.iter());
        }
        Some((n as f64, users.len()))
    }
}

/// (messages, different people) in a chunk of yt-dlp's live chat file.
fn count_chat(text: &str) -> (u32, HashSet<String>) {
    let msgs = text.matches("addChatItemAction").count() as u32;
    let mut users = HashSet::new();
    if let Ok(re) = regex::Regex::new(r#""authorExternalChannelId":\s*"([^"]+)""#) {
        for c in re.captures_iter(text) {
            users.insert(c[1].to_string());
        }
    }
    if let Ok(re) = regex::Regex::new(r#""authorName":\s*\{\s*"(?:simpleText|content)":\s*"([^"]+)""#) {
        for c in re.captures_iter(text) {
            users.insert(c[1].to_string());
        }
    }
    (msgs, users)
}

// ---------- judging a moment ----------

#[derive(Default)]
struct History {
    levels: VecDeque<f64>,
    motions: VecDeque<f64>,
    chats: VecDeque<f64>,
}

fn median(v: &VecDeque<f64>, last: usize) -> f64 {
    let mut x: Vec<f64> = v.iter().rev().take(last).copied().collect();
    if x.is_empty() {
        return 0.0;
    }
    x.sort_by(|a, b| a.total_cmp(b));
    x[x.len() / 2]
}

fn keep(v: &mut VecDeque<f64>, x: f64, cap: usize) {
    v.push_back(x);
    while v.len() > cap {
        v.pop_front();
    }
}

struct Verdict {
    strength: f64,
    reason: String,
    /// The chat led. Chat runs behind the picture, so the event itself is further back.
    chat_led: bool,
}

/// What one piece of the stream looks like compared with the stream's recent past, and whether that is a moment.
/// `chat` is (messages in ten seconds, different people), when chat is available.
fn judge(h: &History, level: f64, motion: f64, chat: Option<(f64, usize)>, sensitivity: f64) -> Option<Verdict> {
    if h.levels.len() < WARMUP_PIECES {
        return None;
    }
    // Each signal is capped, so one wild reading (a layout switch, a flash) cannot stand for a great moment.
    let sa = if level < -40.0 { 0.0 } else { ((level - median(&h.levels, 100)) / 9.0).clamp(0.0, 2.0) };
    let base_m = median(&h.motions, 100);
    // Action alone counts a little less: a scene cut looks like action but is not always a moment.
    let sm = ((motion - base_m) / (2.0 * base_m).max(3.0)).clamp(0.0, 2.0) * 0.7;
    let mut ratio = 0.0;
    let sc = match chat {
        Some((n, people)) if h.chats.len() >= 24 && n >= 12.0 && people >= 6 => {
            ratio = n / median(&h.chats, 120).max(3.0);
            ((ratio - 1.0) / 2.0).clamp(0.0, 2.0)
        }
        _ => 0.0,
    };
    let top = sa.max(sm).max(sc);
    let strength = top + 0.4 * (sa + sm + sc - top).min(1.0);
    let threshold = sensitivity / 6.0;
    if strength < threshold || top < 0.6 * threshold {
        return None;
    }
    let mut why: Vec<String> = Vec::new();
    if sc >= 0.5 {
        why.push(format!("Chat jumped {ratio:.1}×"));
    }
    if sa >= 0.4 {
        why.push(format!("much louder (+{:.0} dB)", sa * 9.0));
    }
    if sm >= 0.5 {
        why.push("big action on screen".into());
    }
    if why.is_empty() {
        why.push("several signals together".into());
    }
    let mut reason = why.join(", ");
    if let Some(c) = reason.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    Some(Verdict { strength, reason, chat_led: sc >= sa.max(sm) && sc > 0.0 })
}

// ---------- recording ----------

async fn record(app: AppHandle, state: State, id: String, url: String, cancel: Arc<AtomicBool>) {
    let dir = state.data_dir.join("live").join(&id);
    let _ = std::fs::create_dir_all(dir.join("moments"));
    let mut hist = History::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut spikes: VecDeque<i64> = VecDeque::new();
    let mut chat = Chat::start(&state, &dir, &url);
    let say = |status: &str, message: &str| {
        with_info(&id, |i| { i.status = status.into(); i.message = message.into(); });
        push(&app, &state);
    };
    while !cancel.load(Ordering::SeqCst) {
        say("connecting", "Opening the stream…");
        let urls = match resolve(&state, &url).await {
            Ok(u) => u,
            Err(e) => {
                say("waiting", &format!("{e} Trying again in 30 seconds."));
                pause(30, &cancel).await;
                continue;
            }
        };
        let mut cmd = command(tool(&state, "ffmpeg"));
        cmd.args(["-hide_banner", "-loglevel", "error", "-i", &urls[0]]);
        if urls.len() > 1 {
            cmd.args(["-i", &urls[1], "-map", "0:v:0", "-map", "1:a:0"]);
        } else {
            cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);
        }
        cmd.args(["-c", "copy", "-f", "segment", "-segment_time", &PIECE.to_string(), "-segment_format", "mp4", "-segment_format_options", "movflags=+faststart", "-strftime", "1", "-reset_timestamps", "1"])
            .arg(dir.join("seg_%Y%m%d_%H%M%S.mp4"))
            .stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                say("waiting", &format!("Could not start FFmpeg: {e}"));
                pause(30, &cancel).await;
                continue;
            }
        };
        say("live", "Watching. The last minutes are kept so you can clip them.");
        loop {
            pause(5, &cancel).await;
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill().await;
                break;
            }
            if matches!(child.try_wait(), Ok(Some(_))) {
                say("waiting", "The stream stopped. Waiting for it to come back…");
                break;
            }
            tidy(&dir);
            let pieces = pieces_in(&dir);
            // Everything but the newest piece is finished (the newest is still being written).
            let done: Vec<(String, i64)> = pieces.iter().take(pieces.len().saturating_sub(1)).cloned().collect();
            if let Some(c) = chat.as_mut() {
                c.poll();
                if let Some((n, _)) = c.last_ten() {
                    keep(&mut hist.chats, n, 200);
                }
            }
            let chat_now = chat.as_ref().and_then(|c| c.last_ten());
            with_info(&id, |i| {
                i.buffered = pieces.len() as i64 * PIECE;
                i.chat = chat_now.map(|c| c.0);
                i.pieces = done.iter().rev().take(8).rev().map(|(n, _)| dir.join(n).to_string_lossy().to_string()).collect();
            });
            for (name, start) in &done {
                if !seen.insert(name.clone()) {
                    continue;
                }
                let Some((level, motion)) = analyse(&state, &dir.join(name)).await else { continue };
                let (auto, sens, last, make) = with_info(&id, |i| { i.level = level; i.motion = motion; (i.auto, i.sensitivity, i.last_spike, i.auto_make) }).unwrap_or((true, 6.0, 0, false));
                let verdict = judge(&hist, level, motion, chat_now, sens);
                keep(&mut hist.levels, level, 200);
                keep(&mut hist.motions, motion, 200);
                let t = now();
                spikes.retain(|s| t - *s < 3600);
                if let (true, Some(v)) = (auto, verdict) {
                    if t - last >= 45 && spikes.len() < 8 {
                        spikes.push_back(t);
                        with_info(&id, |i| i.last_spike = t);
                        let (app2, state2, id2) = (app.clone(), state.clone(), id.clone());
                        let end = start + PIECE;
                        let (from, to) = if v.chat_led { (end - 45, end + 5) } else { (end - 24, end + 10) };
                        tauri::async_runtime::spawn(async move {
                            match cut(&app2, &state2, &id2, from, to, "auto", &v.reason, v.strength).await {
                                Ok(m) => {
                                    if make && v.strength >= 1.5 {
                                        let _ = make_task(&app2, &state2, &m.id);
                                    }
                                }
                                Err(e) => crate::applog::log(&format!("live: could not save a moment: {e}")),
                            }
                        });
                    }
                }
            }
            seen.retain(|n| dir.join(n).is_file());
            push(&app, &state);
        }
        pause(10, &cancel).await;
    }
}

/// (file name, start time) of every buffered piece, oldest first.
fn pieces_in(dir: &Path) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); piece_time(&n).map(|t| (n, t)) }).collect();
    v.sort_by_key(|(_, t)| *t);
    v
}

/// `seg_20261119_144805.mp4` is the piece that started at 14:48:05 local time.
fn piece_time(name: &str) -> Option<i64> {
    use chrono::TimeZone;
    let stem = name.strip_prefix("seg_")?.strip_suffix(".mp4")?;
    let naive = chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d_%H%M%S").ok()?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp())
}

/// Delete pieces older than the buffer.
fn tidy(dir: &Path) {
    let cutoff = now() - KEEP_SECONDS;
    for (name, t) in pieces_in(dir) {
        if t < cutoff {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
}

/// (loudness in dB, number of scene changes) of one piece, from a single pass of FFmpeg.
async fn analyse(state: &State, file: &Path) -> Option<(f64, f64)> {
    let out = command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-nostats", "-i"]).arg(file)
        .args(["-vf", "scale=320:-2,select='gt(scene,0.2)',showinfo", "-af", "volumedetect", "-f", "null", "-"])
        .stdout(Stdio::null()).stderr(Stdio::piped()).output().await.ok()?;
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    parse_analysis(&text)
}

fn parse_analysis(text: &str) -> Option<(f64, f64)> {
    let level: f64 = regex::Regex::new(r"mean_volume:\s*(-?[0-9.]+) dB").ok()?.captures(text)?.get(1)?.as_str().parse().ok()?;
    let re = regex::Regex::new(r"pts_time:\s*([0-9.]+)").ok()?;
    // The first frame of a piece is always "new", so changes in its first moments do not count.
    let changes = text.lines().filter(|l| l.contains("showinfo")).filter_map(|l| re.captures(l)?.get(1)?.as_str().parse::<f64>().ok()).filter(|t| *t >= 0.3).count();
    Some((level, changes as f64))
}

// ---------- moments ----------

/// Save the stretch of the buffer from `from` to `to` (seconds since 1970) as a moment.
#[allow(clippy::too_many_arguments)]
async fn cut(app: &AppHandle, state: &State, session: &str, from: i64, to: i64, kind: &str, reason: &str, strength: f64) -> Result<Moment> {
    // Wait until the stream has reached the end of the window.
    while now() < to {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let dir = state.data_dir.join("live").join(session);
    let chosen: Vec<(String, i64)> = pieces_in(&dir).into_iter().filter(|(_, t)| *t + PIECE > from && *t < to).collect();
    if chosen.is_empty() {
        bail!("The buffer has nothing for that moment yet. Wait a little and try again.");
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let mdir = dir.join("moments");
    std::fs::create_dir_all(&mdir)?;
    let list = mdir.join(format!("{id}.txt"));
    std::fs::write(&list, chosen.iter().map(|(n, _)| format!("file '{}'\n", dir.join(n).to_string_lossy().replace('\\', "/").replace('\'', "'\\''"))).collect::<String>())?;
    let out = mdir.join(format!("{id}.mp4"));
    let ffmpeg = tool(state, "ffmpeg");
    let copy = command(&ffmpeg).args(["-y", "-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]).arg(&list)
        .args(["-c", "copy", "-movflags", "+faststart"]).arg(&out)
        .stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    if !matches!(copy, Ok(s) if s.success()) || std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < 10_000 {
        let enc = command(&ffmpeg).args(["-y", "-hide_banner", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]).arg(&list)
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-c:a", "aac", "-movflags", "+faststart"]).arg(&out)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().await?;
        if !enc.success() {
            let _ = std::fs::remove_file(&list);
            bail!("Could not cut that part of the stream.");
        }
    }
    let _ = std::fs::remove_file(&list);
    let thumb = mdir.join(format!("{id}.jpg"));
    let _ = command(&ffmpeg).args(["-y", "-hide_banner", "-loglevel", "error", "-ss", "3", "-i"]).arg(&out)
        .args(["-frames:v", "1", "-vf", "scale=480:-2"]).arg(&thumb).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    let seconds = command(tool(state, "ffprobe")).args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).arg(&out)
        .stdout(Stdio::piped()).stderr(Stdio::null()).output().await.ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .unwrap_or((chosen.len() as i64 * PIECE) as f64);
    let info = SESSIONS.lock().unwrap().iter().find(|r| r.info.id == session).map(|r| r.info.clone());
    let moment = Moment {
        id, session: session.to_string(),
        channel: info.as_ref().map(|i| i.channel.clone()).unwrap_or_default(),
        title: info.as_ref().map(|i| i.title.clone()).unwrap_or_default(),
        created: now(), seconds, kind: kind.to_string(), reason: reason.to_string(), strength, status: "new".into(), task_id: String::new(),
        path: out.to_string_lossy().to_string(),
        thumb: if thumb.is_file() { thumb.to_string_lossy().to_string() } else { String::new() },
    };
    let mut all = moments(state);
    all.push(moment.clone());
    save_moments(state, all);
    push(app, state);
    Ok(moment)
}

/// "Clip now": the last `pre` seconds and the next `post` seconds.
pub async fn clip_now(app: &AppHandle, state: &State, session: &str, pre: f64, post: f64) -> Result<Value> {
    if !SESSIONS.lock().unwrap().iter().any(|r| r.info.id == session && r.info.status == "live") {
        bail!("This stream is not live right now.");
    }
    let t = now();
    let m = cut(app, state, session, t - pre.clamp(5.0, 120.0) as i64, t + post.clamp(0.0, 60.0) as i64, "manual", "You pressed Clip now", 0.0).await?;
    Ok(serde_json::to_value(m)?)
}

fn make_task(app: &AppHandle, state: &State, moment_id: &str) -> Result<Task> {
    make(app, state, moment_id)
}

/// Send a moment through the normal pipeline as a new task.
pub fn make(app: &AppHandle, state: &State, moment_id: &str) -> Result<Task> {
    let m = moments(state).into_iter().find(|m| m.id == moment_id).ok_or_else(|| anyhow!("That moment is gone."))?;
    if !Path::new(&m.path).is_file() {
        bail!("The clip file is gone.");
    }
    crate::preflight(state, "highlights").map_err(|e| anyhow!(e))?;
    let id = uuid::Uuid::new_v4().to_string();
    let dir = state.data_dir.join("sources").join(&id);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join("live.mp4");
    std::fs::copy(&m.path, &dest)?;
    let at = chrono::DateTime::from_timestamp(m.created, 0).map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
    let who = if m.channel.is_empty() { "Live stream".to_string() } else { m.channel.clone() };
    let task = Task {
        id: id.clone(), title: format!("{who} live · {at}"), source_type: "file".into(), source_path: dest.to_string_lossy().to_string(),
        status: "queued".into(), message: "Waiting to start".into(), workflow: "highlights".into(), options: TaskOptions::default(),
        created_at: crate::state::now(), updated_at: crate::state::now(), ..Default::default()
    };
    state.insert_task(&task)?;
    crate::spawn_task(app, state, &id);
    edit_moment(state, moment_id, |m| { m.status = "made".into(); m.task_id = id.clone(); });
    push(app, state);
    Ok(task)
}

pub fn dismiss(app: &AppHandle, state: &State, moment_id: &str) {
    if let Some(m) = moments(state).into_iter().find(|m| m.id == moment_id) {
        let _ = std::fs::remove_file(&m.path);
        let _ = std::fs::remove_file(&m.thumb);
    }
    let rest: Vec<Moment> = moments(state).into_iter().filter(|m| m.id != moment_id).collect();
    save_moments(state, rest);
    push(app, state);
}

/// Live streams about a topic, from YouTube's "Live" search filter.
pub async fn find(app: &AppHandle, state: &State, query: &str) -> Result<Value> {
    let q: String = query.trim().chars().take(80).collect();
    if q.is_empty() {
        bail!("Type what to look for, for example: GTA 6 live.");
    }
    let enc: String = q.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let url = format!("https://www.youtube.com/results?search_query={enc}&sp=EgJAAQ%253D%253D");
    let settings = state.settings();
    crate::tools::ensure_downloader(state, app).await?;
    let out = command(tool(state, "yt-dlp"))
        .args(crate::download::base_args(state, &settings))
        .args(["--flat-playlist", "-J", "--no-warnings", "--playlist-end", "15", &url])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output().await
        .map_err(|e| anyhow!("yt-dlp is not installed ({e})."))?;
    if !out.status.success() {
        bail!("{}", crate::download::friendly_error(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| anyhow!("The search gave an unreadable answer."))?;
    let found: Vec<Value> = v["entries"].as_array().cloned().unwrap_or_default().iter().filter_map(|e| {
        let id = e["id"].as_str()?;
        Some(json!({
            "title": e["title"].as_str().unwrap_or(""), "channel": e["channel"].as_str().or(e["uploader"].as_str()).unwrap_or(""),
            "url": format!("https://www.youtube.com/watch?v={id}"), "viewers": e["concurrent_view_count"].as_i64().or(e["view_count"].as_i64()),
            "live": e["live_status"].as_str().map(|s| s == "is_live").unwrap_or(true),
        }))
    }).collect();
    Ok(json!({"results": found}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calm() -> History {
        let mut h = History::default();
        for _ in 0..30 {
            keep(&mut h.levels, -30.0, 200);
            keep(&mut h.motions, 2.0, 200);
            keep(&mut h.chats, 10.0, 200);
        }
        h
    }

    #[test]
    fn nothing_is_called_before_two_minutes_are_known() {
        let mut h = History::default();
        for _ in 0..5 {
            keep(&mut h.levels, -30.0, 200);
        }
        assert!(judge(&h, -10.0, 30.0, None, 6.0).is_none());
    }

    #[test]
    fn a_much_louder_piece_is_a_moment_and_a_slightly_louder_one_is_not() {
        let h = calm();
        let v = judge(&h, -21.0, 2.0, None, 6.0).expect("9 dB louder");
        assert!(v.reason.contains("louder"));
        assert!(judge(&h, -26.0, 2.0, None, 6.0).is_none());
        assert!(judge(&h, -45.0, 2.0, None, 1.0).is_none(), "silence is never a moment");
    }

    #[test]
    fn a_burst_of_action_counts() {
        let h = calm();
        let v = judge(&h, -30.0, 12.0, None, 6.0).expect("far more scene changes than usual");
        assert!(v.reason.contains("action"));
    }

    #[test]
    fn a_chat_jump_leads_and_needs_enough_people() {
        let h = calm();
        let v = judge(&h, -30.0, 2.0, Some((40.0, 20)), 6.0).expect("chat four times faster");
        assert!(v.chat_led);
        assert!(v.reason.starts_with("Chat jumped"));
        assert!(judge(&h, -30.0, 2.0, Some((40.0, 2)), 6.0).is_none(), "two people spamming is not a crowd");
        assert!(judge(&h, -30.0, 2.0, Some((8.0, 20)), 6.0).is_none(), "too few messages");
    }

    #[test]
    fn signals_together_beat_a_threshold_that_one_alone_misses() {
        let h = calm();
        assert!(judge(&h, -24.5, 2.0, None, 6.0).is_none());
        assert!(judge(&h, -24.5, 9.0, None, 6.0).is_some());
    }

    #[test]
    fn sensitivity_changes_how_many_moments_there_are() {
        let h = calm();
        assert!(judge(&h, -23.0, 2.0, None, 4.5).is_some());
        assert!(judge(&h, -23.0, 2.0, None, 8.0).is_none());
    }

    #[test]
    fn the_usual_level_follows_the_stream() {
        let mut h = History::default();
        for _ in 0..30 {
            keep(&mut h.levels, -34.0, 200);
        }
        for _ in 0..80 {
            keep(&mut h.levels, -20.0, 200);
        }
        assert!(judge(&h, -14.0, 0.0, None, 6.0).is_none(), "after a loud stretch, loud is normal");
    }

    #[test]
    fn piece_names_carry_their_start_time() {
        let a = piece_time("seg_20261119_144805.mp4").unwrap();
        let b = piece_time("seg_20261119_144815.mp4").unwrap();
        assert_eq!(b - a, 10);
        assert!(piece_time("seg_bad.mp4").is_none());
        assert!(piece_time("seg_20261119_144805.ts").is_none());
    }

    #[test]
    fn chat_is_counted_from_the_file() {
        let text = r#"{"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {"authorExternalChannelId": "UCa"}}}} {"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {"authorExternalChannelId": "UCb"}}}} {"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {"authorExternalChannelId": "UCa"}}}}"#;
        let (n, people) = count_chat(text);
        assert_eq!(n, 3);
        assert_eq!(people.len(), 2);
    }

    #[test]
    fn ffmpeg_output_is_read() {
        let text = "[Parsed_showinfo_2 @ 0x1] n:   0 pts:      0 pts_time:0 pos: 1\n[Parsed_showinfo_2 @ 0x1] n:   1 pts:  90000 pts_time:1.5 pos: 2\n[Parsed_showinfo_2 @ 0x1] n:   2 pts: 180000 pts_time:3 pos: 3\n[Parsed_volumedetect_0 @ 0x2] mean_volume: -27.3 dB\n";
        assert_eq!(parse_analysis(text), Some((-27.3, 2.0)));
        assert_eq!(parse_analysis("nothing useful"), None);
    }
}
