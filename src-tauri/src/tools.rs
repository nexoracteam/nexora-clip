//! External tools: detection and one-click install into `<data>/tools`.

use crate::ffmpeg::{command, tool, EXE};
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

#[derive(Serialize, Clone, Debug)]
pub struct ToolStatus {
    pub name: String,
    pub label: String,
    pub installed: bool,
    pub version: String,
    pub path: String,
    pub required: bool,
    pub note: String,
}

pub fn whisper_exe(state: &State) -> Option<PathBuf> {
    let dir = state.data_dir.join("tools").join("whisper");
    for name in ["whisper-cli", "main", "whisper"] {
        let p = dir.join(format!("{name}{EXE}"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn whisper_model_path(state: &State, model: &str) -> PathBuf {
    state.data_dir.join("models").join(format!("ggml-{model}.bin"))
}

async fn version_of(program: &Path, arg: &str) -> Option<String> {
    let out = command(program).arg(arg).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Some(text.lines().next().unwrap_or("").trim().chars().take(80).collect())
}

pub async fn status(state: &State) -> Vec<ToolStatus> {
    let settings = state.settings();
    let specs = [
        ("ffmpeg", "FFmpeg", "-version", true, "Cuts, crops, captions and encodes every clip."),
        ("ffprobe", "FFprobe", "-version", true, "Reads video size and duration."),
        ("yt-dlp", "yt-dlp", "--version", false, "Needed for YouTube, Twitch and Kick links."),
        ("deno", "Deno (YouTube helper)", "--version", false, "Lets yt-dlp solve YouTube's checks. Install it if YouTube downloads fail."),
    ];
    // Each check starts a program (yt-dlp alone takes over a second to wake up): ask all at once.
    let paths: Vec<PathBuf> = specs.iter().map(|s| tool(state, s.0)).collect();
    let versions = futures_util::future::join_all(specs.iter().zip(paths.iter()).map(|(s, p)| version_of(p, s.2))).await;
    let mut list = Vec::new();
    for ((name, label, _arg, required, note), (path, version)) in specs.into_iter().zip(paths.into_iter().zip(versions)) {
        list.push(ToolStatus {
            name: name.into(),
            label: label.into(),
            installed: version.is_some(),
            version: version.unwrap_or_default(),
            path: path.to_string_lossy().into(),
            required,
            note: note.into(),
        });
    }
    let exe = whisper_exe(state);
    list.push(ToolStatus {
        name: "whisper".into(),
        label: "whisper.cpp".into(),
        installed: exe.is_some(),
        version: if exe.is_some() { "installed".into() } else { String::new() },
        path: exe.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        required: false,
        note: "Free offline transcription on this PC.".into(),
    });
    let model = whisper_model_path(state, &settings.whisper_model);
    let size = std::fs::metadata(&model).map(|m| m.len()).unwrap_or(0);
    list.push(ToolStatus {
        name: "whisper-model".into(),
        label: format!("Whisper model ({})", settings.whisper_model),
        installed: size > 1_000_000,
        version: if size > 0 { format!("{} MB", size / 1_048_576) } else { String::new() },
        path: model.to_string_lossy().into(),
        required: false,
        note: "Speech model used by whisper.cpp.".into(),
    });
    list
}

pub async fn download_file(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    mut on: impl FnMut(f64),
) -> Result<()> {
    let resp = http.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let part = dest.with_extension("part");
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut last = -1.0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if total > 0 {
            let f = done as f64 / total as f64;
            if f - last >= 0.01 {
                last = f;
                on(f);
            }
        }
    }
    file.flush().await?;
    drop(file);
    let _ = tokio::fs::remove_file(dest).await;
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

/// Extract zip entries whose file name passes `keep`, flattened into `dest_dir`.
fn unzip_flat(zip_path: &Path, dest_dir: &Path, keep: impl Fn(&str) -> bool) -> Result<usize> {
    std::fs::create_dir_all(dest_dir)?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name().and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string())) else {
            continue;
        };
        if !keep(&name) {
            continue;
        }
        let mut out = std::fs::File::create(dest_dir.join(&name))?;
        std::io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    Ok(count)
}

pub async fn install(state: &State, app: &AppHandle, name: &str) -> Result<()> {
    let tools = state.data_dir.join("tools");
    let emit = |fraction: f64, message: &str| {
        let _ = app.emit("tool-progress", serde_json::json!({"name": name, "fraction": fraction, "message": message}));
    };
    emit(0.0, "Starting download");
    match name {
        "deno" => {
            let url = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
            let zip_path = tools.join("deno.zip");
            download_file(&state.http, url, &zip_path, |f| emit(f * 0.9, "Downloading Deno")).await?;
            emit(0.93, "Unpacking");
            let (z, t) = (zip_path.clone(), tools.clone());
            let n = tokio::task::spawn_blocking(move || unzip_flat(&z, &t, |n| n == "deno.exe")).await??;
            let _ = tokio::fs::remove_file(&zip_path).await;
            if n < 1 {
                bail!("The Deno archive did not contain deno.exe");
            }
        }
        "yt-dlp" => {
            let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
            download_file(&state.http, url, &tools.join("yt-dlp.exe"), |f| emit(f, "Downloading yt-dlp")).await?;
        }
        "ffmpeg" | "ffprobe" => {
            let url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
            let zip_path = tools.join("ffmpeg.zip");
            download_file(&state.http, url, &zip_path, |f| emit(f * 0.9, "Downloading FFmpeg")).await?;
            emit(0.92, "Unpacking");
            let (z, t) = (zip_path.clone(), tools.clone());
            let n = tokio::task::spawn_blocking(move || unzip_flat(&z, &t, |n| n == "ffmpeg.exe" || n == "ffprobe.exe")).await??;
            let _ = tokio::fs::remove_file(&zip_path).await;
            if n < 2 {
                bail!("The FFmpeg archive did not contain ffmpeg.exe and ffprobe.exe");
            }
            *state.encoder_cache.lock().unwrap() = None;
        }
        "whisper" => {
            // The tagged "latest" release often carries no binaries; they are
            // attached to separate build releases, so scan the recent ones.
            let releases: serde_json::Value = state
                .http
                .get("https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=20")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let releases = releases.as_array().cloned().unwrap_or_default();
            let pick = |want: &str| {
                releases.iter().find_map(|r| {
                    r["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(want)).and_then(|a| a["browser_download_url"].as_str().map(String::from))
                })
            };
            let url = pick("whisper-bin-x64.zip")
                .or_else(|| pick("whisper-blas-bin-x64.zip"))
                .ok_or_else(|| anyhow!("No Windows x64 build found in the latest whisper.cpp release"))?;
            let zip_path = tools.join("whisper.zip");
            download_file(&state.http, &url, &zip_path, |f| emit(f * 0.9, "Downloading whisper.cpp")).await?;
            emit(0.92, "Unpacking");
            let (z, t) = (zip_path.clone(), tools.join("whisper"));
            tokio::task::spawn_blocking(move || unzip_flat(&z, &t, |n| n.ends_with(".exe") || n.ends_with(".dll"))).await??;
            let _ = tokio::fs::remove_file(&zip_path).await;
            if whisper_exe(state).is_none() {
                bail!("whisper.cpp was downloaded but its executable was not found");
            }
        }
        "whisper-model" => {
            let model = state.settings().whisper_model;
            let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}.bin");
            let dest = whisper_model_path(state, &model);
            download_file(&state.http, &url, &dest, |f| emit(f, "Downloading speech model")).await?;
        }
        other => bail!("Unknown tool: {other}"),
    }
    emit(1.0, "Done");
    Ok(())
}

/// Make sure yt-dlp (and a JavaScript runtime for YouTube) exist; download them the first time.
pub async fn ensure_downloader(state: &State, app: &AppHandle) -> Result<()> {
    if version_of(&tool_path(state, "yt-dlp"), "--version").await.is_none() {
        install(state, app, "yt-dlp").await.map_err(|e| anyhow!("Could not set up the video downloader: {e}"))?;
    }
    let deno_local = state.data_dir.join("tools").join(format!("deno{EXE}"));
    let has = |p: &str| std::process::Command::new(p).arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    if !deno_local.is_file() && !has("deno") && !has("node") {
        // Best effort: without a runtime YouTube still works for many videos.
        let _ = install(state, app, "deno").await;
    }
    Ok(())
}

fn tool_path(state: &State, name: &str) -> PathBuf {
    tool(state, name)
}
