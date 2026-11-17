//! Outro: the last screen of every video. Generated once with ffmpeg (dark glow,
//! logo, gold headline, white tagline) and cached as an mp4 + a preview PNG.

use crate::ffmpeg::{self, tool};
use crate::models::Settings;
use crate::render;
use crate::state::State;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub fn logo_path(state: &State) -> PathBuf {
    let custom = state.data_dir.join("assets").join("outro_logo.png");
    if custom.is_file() {
        return custom;
    }
    state.res_dir.join("outro_logo.png")
}

fn key(state: &State, s: &Settings, w: i64, h: i64) -> String {
    let logo = logo_path(state);
    let mtime = std::fs::metadata(&logo).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let raw = format!("{}|{}|{}|{}|{}|{}|{}|v3", s.outro_line1, s.outro_line2, s.outro_seconds, w, h, mtime, logo.display());
    hex::encode(&Sha256::digest(raw.as_bytes())[..6])
}

fn esc(t: &str) -> String {
    t.replace('\\', "\\\\").replace('{', "\\{").replace('}', "\\}").replace(['\n', '\r'], " ")
}

fn ass(s: &Settings, w: i64, h: i64, has_logo: bool) -> String {
    let y1 = if has_logo { (h as f64 * 0.36) as i64 + 430 } else { (h as f64 * 0.42) as i64 };
    let y2 = y1 + 105;
    let mut events = String::new();
    let line1 = s.outro_line1.trim().to_uppercase();
    let line2 = s.outro_line2.trim().to_string();
    if !line1.is_empty() {
        events.push_str(&format!("Dialogue: 0,0:00:00.00,0:01:00.00,Gold,,0,0,0,,{{\\an5\\pos({},{})\\fad(450,0)}}{}\n", w / 2, y1, esc(&line1)));
    }
    if !line2.is_empty() {
        events.push_str(&format!("Dialogue: 0,0:00:00.00,0:01:00.00,White,,0,0,0,,{{\\an5\\pos({},{})\\fad(450,0)}}{}\n", w / 2, if line1.is_empty() { y1 } else { y2 }, esc(&line2)));
    }
    format!("[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Gold,Poppins ExtraBold,78,&H0000C4FF&,&H000000FF,&H0000141A&,&H00000000,0,0,0,0,100,100,0,0,1,7,0,5,60,60,60,1\nStyle: White,Poppins ExtraBold,44,&H00FFFFFF&,&H000000FF,&H00000000&,&H00000000,0,0,0,0,100,100,0,0,1,3,0,5,60,60,60,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n{events}")
}

/// Build (or reuse) the outro mp4. Returns its path and duration.
pub async fn ensure(state: &State, settings: &Settings, cancel: &AtomicBool) -> Result<(PathBuf, f64)> {
    let (w, h) = render::output_size(settings);
    let secs = settings.outro_seconds.clamp(1.0, 8.0);
    let k = key(state, settings, w, h);
    let cache = state.data_dir.join("cache");
    std::fs::create_dir_all(&cache)?;
    let out = cache.join(format!("outro_{k}.mp4"));
    if out.is_file() {
        return Ok((out, secs));
    }
    let logo = logo_path(state);
    let has_logo = logo.is_file();
    let work = state.data_dir.join("work");
    std::fs::create_dir_all(&work)?;
    let bg = work.join(format!("outro_bg_{k}.png"));
    let ass_path = work.join(format!("outro_{k}.ass"));
    std::fs::write(&ass_path, ass(settings, w, h, has_logo))?;
    // One frame of the glow background, looped for the whole outro.
    let glow = format!(
        "color=s={w}x{h}:c=black:d=1,format=rgb24,geq=r='5+47*pow(max(0,1-hypot((X-W/2)/W,(Y-H*0.36)/W)/0.75),2)':g='5+35*pow(max(0,1-hypot((X-W/2)/W,(Y-H*0.36)/W)/0.75),2)':b='6+0*X'"
    );
    ffmpeg::run(state, vec!["-f".into(), "lavfi".into(), "-i".into(), glow, "-frames:v".into(), "1".into(), bg.to_string_lossy().into()], cancel, |_| {}).await?;
    let mut args: Vec<String> = vec!["-loop".into(), "1".into(), "-framerate".into(), "30".into(), "-t".into(), format!("{secs}"), "-i".into(), bg.to_string_lossy().into()];
    let mut chain = String::new();
    let mut cur = "0:v".to_string();
    if has_logo {
        args.extend(["-loop".into(), "1".into(), "-framerate".into(), "30".into(), "-t".into(), format!("{secs}"), "-i".into(), logo.to_string_lossy().into()]);
        chain.push_str("[1:v]scale=620:-1:flags=lanczos,format=rgba,fade=t=in:st=0:d=0.45:alpha=1[lg];");
        chain.push_str(&format!("[0:v][lg]overlay=x=(W-w)/2:y=H*0.36-h/2[bgl];"));
        cur = "bgl".into();
    }
    chain.push_str(&format!("[{cur}]subtitles={}:fontsdir=fonts,format=yuv420p[v]", ffmpeg::rel(state, &ass_path)));
    args.extend(["-f".into(), "lavfi".into(), "-t".into(), format!("{secs}"), "-i".into(), "anullsrc=r=48000:cl=stereo".into()]);
    let audio_idx = if has_logo { 2 } else { 1 };
    args.extend(["-filter_complex".into(), chain, "-map".into(), "[v]".into(), "-map".into(), format!("{audio_idx}:a")]);
    args.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), "18".into(), "-pix_fmt".into(), "yuv420p".into(), "-r".into(), "30".into()]);
    args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-shortest".into(), "-t".into(), format!("{secs}"), out.to_string_lossy().into()]);
    let r = ffmpeg::run(state, args, cancel, |_| {}).await;
    let _ = std::fs::remove_file(&bg);
    let _ = std::fs::remove_file(&ass_path);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&out);
        bail!("Could not build the outro: {e}");
    }
    Ok((out, secs))
}

/// PNG of the settled last frame, for the Settings page.
pub async fn preview(state: &State, settings: &Settings) -> Result<PathBuf> {
    let never = AtomicBool::new(false);
    let (video, secs) = ensure(state, settings, &never).await?;
    let png = video.with_extension("png");
    if !png.is_file() {
        let st = ffmpeg::command(tool(state, "ffmpeg"))
            .args(["-hide_banner", "-y", "-loglevel", "error", "-ss", &format!("{:.2}", (secs - 0.1).max(0.0)), "-i"])
            .arg(&video)
            .args(["-frames:v", "1", "-vf", "scale=540:-2"])
            .arg(&png)
            .status()
            .await?;
        if !st.success() {
            bail!("Could not draw the outro preview.");
        }
    }
    Ok(png)
}

/// Keep a PNG copy of the picked logo so the original can move.
pub async fn set_logo(state: &State, src: &Path) -> Result<()> {
    let probe = ffmpeg::probe(state, src).await.map_err(|_| anyhow::anyhow!("Choose a PNG, JPG or WebP picture for the logo."))?;
    if probe.width.min(probe.height) < 64 {
        bail!("The logo is too small. Use a picture at least 64 × 64 pixels.");
    }
    let dst = state.data_dir.join("assets");
    std::fs::create_dir_all(&dst)?;
    let out = dst.join("outro_logo.png");
    let tmp = dst.join("outro_logo.tmp.png");
    let st = ffmpeg::command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-y", "-loglevel", "error", "-i"])
        .arg(src)
        .args(["-vf", "scale='min(1024,iw)':-2", "-frames:v", "1", "-pix_fmt", "rgba"])
        .arg(&tmp)
        .status()
        .await?;
    if !st.success() {
        bail!("Choose a PNG, JPG or WebP picture for the logo.");
    }
    std::fs::rename(&tmp, &out)?;
    Ok(())
}
