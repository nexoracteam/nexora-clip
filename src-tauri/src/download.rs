//! Source download through yt-dlp:
//! probe first (`-J`), then download with a strict H.264/m4a format, fall back to a
//! relaxed format, keep `.info.json`, report speed + ETA, and explain failures.

use crate::ffmpeg::{command, tool};
use crate::models::Settings;
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub struct Downloaded {
    pub path: PathBuf,
    pub title: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Probe {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: f64,
    pub thumbnail: String,
    pub url: String,
    pub is_live: bool,
    pub heights: Vec<i64>,
    pub site: String,
}

pub fn friendly_error(text: &str) -> String {
    let t = text.to_lowercase();
    let table: [(&[&str], &str); 8] = [
        (&["sign in to confirm", "not a bot"], "YouTube asked for a human check. In Settings set \"Use cookies from browser\" to the browser where you are signed in to YouTube (chrome, edge or firefox), then try again."),
        (&["age-restricted", "private video", "members-only", "members only", "login required", "sign in"], "This video needs a signed-in account. In Settings set \"Use cookies from browser\" to a browser where you are signed in, then try again."),
        (&["not available in your country", "geo-restricted", "geo restricted"], "This video is not available in your region."),
        (&["video unavailable", "has been removed", "does not exist", "no longer available"], "This video was removed or is unavailable."),
        (&["live event will begin", "is live", "premieres in"], "Live streams are not supported. Wait until the stream has ended and try again."),
        (&["no space left", "not enough space"], "The disk is full. Free some space and try again."),
        (&["unsupported url"], "This link is not supported. Use a YouTube, Twitch or Kick video link."),
        (&["unable to download", "connection", "timed out", "resolve", "urlopen error", "getaddrinfo"], "The connection failed. Check your internet connection and try again."),
    ];
    for (markers, message) in table {
        if markers.iter().any(|m| t.contains(m)) {
            return message.to_string();
        }
    }
    let last = text.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("").trim();
    if last.is_empty() {
        "The download failed. Open Settings and press Update on yt-dlp, then try again.".into()
    } else {
        format!("The download failed ({}). Press Update on yt-dlp in Settings, then try again.", last.trim_start_matches("ERROR:").trim().chars().take(160).collect::<String>())
    }
}

pub fn check_url(url: &str) -> Result<String> {
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        bail!("Paste a full video link that starts with https://");
    }
    Ok(u.to_string())
}

/// JS runtime for YouTube signature solving: deno we installed, else deno/node on PATH.
fn js_runtime_args(state: &State) -> Vec<String> {
    let local = state.data_dir.join("tools").join(format!("deno{}", crate::ffmpeg::EXE));
    if local.is_file() {
        return vec!["--js-runtimes".into(), format!("deno:{}", local.to_string_lossy())];
    }
    let has = |p: &str| std::process::Command::new(p).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    if has("deno") {
        return vec![];
    }
    if has("node") {
        return vec!["--js-runtimes".into(), "node".into()];
    }
    vec![]
}

pub(crate) fn base_args(state: &State, settings: &Settings) -> Vec<String> {
    let mut a: Vec<String> = ["--ignore-config", "--encoding", "utf-8", "--no-playlist", "--no-colors", "--newline", "--windows-filenames",
        "--socket-timeout", "20", "--retries", "3", "--fragment-retries", "3"].iter().map(|s| s.to_string()).collect();
    a.extend(js_runtime_args(state));
    let ffmpeg = tool(state, "ffmpeg");
    if let Some(dir) = ffmpeg.parent().filter(|d| d.is_dir() && !d.as_os_str().is_empty()) {
        a.push("--ffmpeg-location".into());
        a.push(dir.to_string_lossy().into());
    }
    if !settings.cookies_from_browser.trim().is_empty() {
        a.push("--cookies-from-browser".into());
        a.push(settings.cookies_from_browser.trim().to_string());
    }
    a
}

pub async fn probe(state: &State, settings: &Settings, url: &str) -> Result<Probe> {
    let url = check_url(url)?;
    let out = command(tool(state, "yt-dlp"))
        .args(base_args(state, settings))
        .args(["-J", "--no-warnings", "--skip-download", &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("yt-dlp is not installed ({e}). Open Settings → Tools and install it."))?;
    if !out.status.success() {
        bail!("{}", friendly_error(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| anyhow!("The site returned unreadable video details."))?;
    if v["is_live"].as_bool() == Some(true) {
        bail!("{}", friendly_error("is live"));
    }
    let mut heights: Vec<i64> = v["formats"].as_array().cloned().unwrap_or_default().iter().filter_map(|f| f["height"].as_i64()).collect();
    heights.sort();
    heights.dedup();
    Ok(Probe {
        id: v["id"].as_str().unwrap_or("").into(),
        title: v["title"].as_str().unwrap_or("Video").into(),
        channel: v["channel"].as_str().or(v["uploader"].as_str()).unwrap_or("").into(),
        duration: v["duration"].as_f64().unwrap_or(0.0),
        thumbnail: v["thumbnail"].as_str().unwrap_or("").into(),
        url: v["webpage_url"].as_str().unwrap_or(&url).into(),
        is_live: false,
        heights,
        site: v["extractor_key"].as_str().unwrap_or("").into(),
    })
}

async fn run_once(
    state: &State,
    settings: &Settings,
    url: &str,
    dest_dir: &Path,
    format: &str,
    section: Option<(f64, f64)>,
    cancel: &AtomicBool,
    on: &mut impl FnMut(f64, &str),
) -> std::result::Result<PathBuf, String> {
    let mut cmd = command(tool(state, "yt-dlp"));
    cmd.args(base_args(state, settings));
    cmd.args(["--no-quiet", "--progress", "--progress-delta", "0.5", "-f", format, "--merge-output-format", "mp4",
        "--write-info-json", "--write-thumbnail",
        "--progress-template", "download:NEXORA|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s",
        "--print", "after_move:NEXORA_FILE|%(filepath)j", "-o"]);
    cmd.arg(dest_dir.join("%(title).150B [%(id)s].%(ext)s"));
    if let Some((s, e)) = section {
        cmd.args(["--download-sections", &format!("*{s}-{e}"), "--force-keyframes-at-cuts"]);
    }
    cmd.arg(url);
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|e| format!("yt-dlp is not installed ({e}). Open Settings → Tools and install it."))?;
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(300));
    let mut file: Option<PathBuf> = None;
    let mut log: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    let l = l.trim().to_string();
                    if let Some(rest) = l.strip_prefix("NEXORA|") {
                        let p: Vec<&str> = rest.split('|').collect();
                        let pct = p.first().and_then(|x| x.trim().trim_end_matches('%').trim().parse::<f64>().ok()).unwrap_or(0.0);
                        let speed = p.get(1).map(|x| x.trim()).unwrap_or("");
                        let eta = p.get(2).map(|x| x.trim()).unwrap_or("…");
                        on(pct / 100.0, &format!("Downloading · {speed} · ETA {eta}"));
                    } else if let Some(json) = l.strip_prefix("NEXORA_FILE|") {
                        file = serde_json::from_str::<String>(json).ok().map(PathBuf::from);
                    } else if l.contains("[Merger]") {
                        on(0.99, "Joining video and audio…");
                    } else if l.to_lowercase().contains("retrying") {
                        on(0.0, "Connection interrupted; retrying…");
                    }
                    log.push(l);
                    if log.len() > 60 { log.remove(0); }
                }
                _ => break,
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    return Err("cancelled".into());
                }
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let err = err_task.await.unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if !status.success() {
        return Err(format!("{err}\n{}", log.join("\n")));
    }
    match file {
        Some(p) if p.is_file() => Ok(p),
        _ => Err("The downloaded video could not be located in its download folder.".into()),
    }
}

pub async fn download(
    state: &State,
    settings: &Settings,
    url: &str,
    task_id: &str,
    quality: &str,
    section: Option<(f64, f64)>,
    cancel: &AtomicBool,
    app: &tauri::AppHandle,
    mut on: impl FnMut(f64, &str),
) -> Result<Downloaded> {
    let url = check_url(url)?;
    on(0.0, "Reading video details…");
    let info = probe(state, settings, &url).await?;
    if let Some((s, e)) = section {
        if !(0.0 <= s && s < e && (info.duration <= 0.0 || e <= info.duration)) {
            bail!("The start and end times must stay inside the video.");
        }
    }
    let dest = state.data_dir.join("sources").join(task_id);
    std::fs::create_dir_all(&dest)?;
    let height = if quality == "720p" { 720 } else { settings.youtube_max_height.clamp(360, 2160) as i64 };
    let strict = format!("bv*[height<={height}][fps<=60][vcodec^=avc1]+ba[ext=m4a]/bv*[height<={height}][fps<=60]+ba/b[height<={height}][fps<=60]");
    let relaxed = format!("bv*[height<={height}]+ba/b[height<={height}]/bv*+ba/b");
    on(0.0, "Connecting to video streams…");
    let path = match run_once(state, settings, &url, &dest, &strict, section, cancel, &mut on).await {
        Ok(p) => p,
        Err(e) if e == "cancelled" => bail!("cancelled"),
        Err(first) => {
            // A strict format can match nothing on some sites (Twitch, Kick, odd uploads).
            on(0.0, "Trying another video format…");
            match run_once(state, settings, &url, &dest, &relaxed, section, cancel, &mut on).await {
                Ok(p) => p,
                Err(e) if e == "cancelled" => bail!("cancelled"),
                Err(second) => {
                    let combined = format!("{first}\n{second}");
                    // YouTube changes often. If the failure is not a clear user-side problem, update yt-dlp once and retry.
                    if friendly_error(&combined).starts_with("The download failed") {
                        on(0.0, "Updating the downloader…");
                        if crate::tools::install(state, app, "yt-dlp").await.is_ok() {
                            on(0.0, "Trying again with the newest downloader…");
                            match run_once(state, settings, &url, &dest, &strict, section, cancel, &mut on).await {
                                Ok(p) => return Ok(Downloaded { path: p, title: info.title }),
                                Err(e) if e == "cancelled" => bail!("cancelled"),
                                Err(third) => bail!("{}", friendly_error(&format!("{combined}\n{third}"))),
                            }
                        }
                    }
                    for part in std::fs::read_dir(&dest).into_iter().flatten().flatten() {
                        if part.file_name().to_string_lossy().contains(".part") {
                            let _ = std::fs::remove_file(part.path());
                        }
                    }
                    bail!("{}", friendly_error(&format!("{first}\n{second}")));
                }
            }
        }
    };
    Ok(Downloaded { path, title: info.title })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_explained() {
        assert!(friendly_error("ERROR: Sign in to confirm you're not a bot").contains("cookies"));
        assert!(friendly_error("Video unavailable. This video has been removed").contains("removed"));
        assert!(friendly_error("something odd").starts_with("The download failed"));
        assert!(friendly_error("ERROR: unable to download webpage: getaddrinfo failed").contains("connection"));
    }

    #[test]
    fn only_real_links_are_accepted() {
        assert!(check_url("https://youtu.be/abc").is_ok());
        assert!(check_url("file:///C:/x.mp4").is_err());
        assert!(check_url("not a link").is_err());
    }
}
