//! ffmpeg / ffprobe process helpers.

use crate::models::Settings;
use crate::state::State;
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

pub const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// A child-process command that never flashes a console window.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c
}

/// Bundled/downloaded tool in `<data>/tools`, else whatever is on PATH.
pub fn tool(state: &State, name: &str) -> PathBuf {
    let local = state.data_dir.join("tools").join(format!("{name}{EXE}"));
    if local.is_file() {
        return local;
    }
    let nested = state.data_dir.join("tools").join(name).join(format!("{name}{EXE}"));
    if nested.is_file() {
        return nested;
    }
    PathBuf::from(name)
}

pub fn round_even(v: f64) -> i64 {
    let n = v.round() as i64;
    if n % 2 == 0 { n } else { n - 1 }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Probe {
    pub duration: f64,
    pub width: i64,
    pub height: i64,
    pub has_audio: bool,
    pub has_video: bool,
    pub fps: f64,
}

pub async fn probe(state: &State, path: &Path) -> Result<Probe> {
    let out = command(tool(state, "ffprobe"))
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .await
        .map_err(|e| anyhow!("ffprobe is not available ({e}). Install the tools in Settings."))?;
    if !out.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut p = Probe::default();
    p.duration = v["format"]["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    for s in v["streams"].as_array().cloned().unwrap_or_default() {
        match s["codec_type"].as_str() {
            Some("audio") => p.has_audio = true,
            Some("video") if !p.has_video && s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                p.has_video = true;
                let (mut w, mut h) = (s["width"].as_i64().unwrap_or(0), s["height"].as_i64().unwrap_or(0));
                let mut rot = s["tags"]["rotate"].as_str().and_then(|r| r.parse::<f64>().ok()).unwrap_or(0.0);
                for sd in s["side_data_list"].as_array().cloned().unwrap_or_default() {
                    if let Some(r) = sd["rotation"].as_f64() {
                        rot = r;
                    }
                }
                if (rot.abs().round() as i64) % 180 == 90 {
                    std::mem::swap(&mut w, &mut h);
                }
                p.width = w;
                p.height = h;
                let rate = s["avg_frame_rate"].as_str().unwrap_or("30/1");
                let mut it = rate.split('/');
                let n: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(30.0);
                let d: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
                p.fps = if d > 0.0 && n > 0.0 { n / d } else { 30.0 };
                if p.duration <= 0.0 {
                    p.duration = s["duration"].as_str().and_then(|x| x.parse().ok()).unwrap_or(0.0);
                }
            }
            _ => {}
        }
    }
    Ok(p)
}

/// Run ffmpeg with the data dir as cwd (so filter paths can stay relative and
/// never need Windows drive-letter escaping). Reports seconds encoded.
pub async fn run(
    state: &State,
    args: Vec<String>,
    cancel: &AtomicBool,
    mut on_time: impl FnMut(f64),
) -> Result<()> {
    let mut child = command(tool(state, "ffmpeg"))
        .current_dir(&state.data_dir)
        .args(["-hide_banner", "-y", "-nostdin", "-loglevel", "error", "-nostats", "-progress", "pipe:1"])
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("ffmpeg is not available ({e}). Install the tools in Settings."))?;
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if let Some(v) = l.strip_prefix("out_time_us=").or_else(|| l.strip_prefix("out_time_ms=")) {
                        if let Ok(us) = v.trim().parse::<f64>() { on_time(us / 1_000_000.0); }
                    }
                }
                _ => break,
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    bail!("cancelled");
                }
            }
        }
    }
    let status = child.wait().await?;
    let err = err_task.await.unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    if !status.success() {
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        bail!("ffmpeg failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | "));
    }
    Ok(())
}

async fn encoder_works(state: &State, enc: &str) -> bool {
    command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=640x360:r=30:d=0.3",
               "-c:v", enc, "-pix_fmt", "yuv420p", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve the "auto" encoder once: a working GPU encoder, else libx264.
pub async fn resolve_encoder(state: &State, settings: &Settings) -> String {
    if settings.encoder != "auto" && !settings.encoder.is_empty() {
        return settings.encoder.clone();
    }
    if let Some(e) = state.encoder_cache.lock().unwrap().clone() {
        return e;
    }
    let mut picked = "libx264".to_string();
    for enc in ["h264_nvenc", "h264_qsv", "h264_amf"] {
        if encoder_works(state, enc).await {
            picked = enc.to_string();
            break;
        }
    }
    *state.encoder_cache.lock().unwrap() = Some(picked.clone());
    picked
}

pub fn video_encode_args(encoder: &str, settings: &Settings, max_bitrate: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = match encoder {
        "h264_nvenc" => ["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "21", "-b:v", "0"].iter().map(|s| s.to_string()).collect(),
        "h264_qsv" => ["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "22"].iter().map(|s| s.to_string()).collect(),
        "h264_amf" => ["-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "20", "-qp_p", "22"].iter().map(|s| s.to_string()).collect(),
        _ => vec!["-c:v".into(), "libx264".into(), "-preset".into(), settings.x264_preset.clone(), "-crf".into(), "20".into()],
    };
    if let Some(b) = max_bitrate {
        let doubled = b
            .strip_suffix('M')
            .and_then(|n| n.parse::<f64>().ok())
            .map(|n| format!("{}M", (n * 2.0) as i64))
            .unwrap_or_else(|| b.to_string());
        a.extend(["-maxrate".into(), b.to_string(), "-bufsize".into(), doubled]);
    }
    a.extend(["-pix_fmt", "yuv420p", "-profile:v", "high", "-movflags", "+faststart"].iter().map(|s| s.to_string()));
    a
}

pub fn audio_encode_args(bitrate: &str) -> Vec<String> {
    ["-c:a", "aac", "-b:a", bitrate, "-ar", "48000", "-ac", "2"].iter().map(|s| s.to_string()).collect()
}

/// Path relative to the data dir with forward slashes, for use inside filters.
pub fn rel(state: &State, path: &Path) -> String {
    path.strip_prefix(&state.data_dir).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

pub async fn thumbnail(state: &State, video: &Path, out: &Path, at: f64) -> Result<()> {
    let st = command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-y", "-loglevel", "error", "-ss", &format!("{at:.2}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", "scale=360:-2", "-q:v", "4"])
        .arg(out)
        .status()
        .await?;
    if !st.success() {
        bail!("thumbnail failed");
    }
    Ok(())
}

/// Does this tool start? (blocking, no console window)
pub fn tool_works(state: &State, name: &str) -> bool {
    let mut c = std::process::Command::new(tool(state, name));
    c.arg(if name == "yt-dlp" { "--version" } else { "-version" }).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.status().map(|s| s.success()).unwrap_or(false)
}

/// Free bytes on the drive holding `path` (u64::MAX when unknown).
pub fn free_space(path: &Path) -> u64 {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free: u64 = 0;
        if GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
            return free;
        }
    }
    let _ = path;
    u64::MAX
}
