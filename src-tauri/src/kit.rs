//! Publish kit: for one clip, AI-written titles, descriptions and hashtags for
//! YouTube Shorts, TikTok and Facebook Reels, plus a cover image, packed in a ZIP
//! next to the video file.

use crate::ffmpeg::{command, tool};
use crate::llm;
use crate::models::{Clip, Settings, Task};
use crate::state::State;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Kit {
    pub youtube_title: String,
    pub youtube_description: String,
    pub tiktok_caption: String,
    pub facebook_caption: String,
    pub hashtags: Vec<String>,
}

const SYSTEM: &str = "You write publishing text for one short vertical video clip. Write in the language of the clip text. \
Be specific and honest: no clickbait that the clip does not deliver, no emojis spam (at most one), no quotation marks around the whole text.\n\
Return a JSON object with exactly these keys:\n\
- youtube_title: under 90 characters, the clear hook of the clip.\n\
- youtube_description: 2 to 3 short lines saying what the viewer gets, no links.\n\
- tiktok_caption: under 140 characters, conversational, no hashtags inside.\n\
- facebook_caption: 1 to 2 sentences, a question or a strong statement, no hashtags inside.\n\
- hashtags: 5 to 8 relevant hashtags without the # sign, lowercase, no spaces, most specific first.";

pub fn clean_tag(t: &str) -> String {
    t.trim().trim_start_matches('#').chars().filter(|c| c.is_alphanumeric() || *c == '_').collect::<String>().to_lowercase()
}

fn limit(s: &str, max: usize) -> String {
    let s: String = s.replace(['<', '>'], "").split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) if i > max / 2 => cut[..i].trim_end_matches([',', ';', ':', '-']).to_string(),
        _ => cut,
    }
}

/// Normalise what the model returned so every platform accepts it.
pub fn clean(v: &Value, fallback_title: &str) -> Kit {
    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let mut tags: Vec<String> = v["hashtags"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str()).map(clean_tag).filter(|t| !t.is_empty() && t.len() <= 30).collect();
    tags.dedup();
    let mut seen = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    tags.truncate(8);
    let title = if s("youtube_title").is_empty() { fallback_title.to_string() } else { s("youtube_title") };
    let tiktok = if s("tiktok_caption").is_empty() { title.clone() } else { s("tiktok_caption") };
    let facebook = if s("facebook_caption").is_empty() { title.clone() } else { s("facebook_caption") };
    Kit {
        youtube_title: limit(&title, 95),
        youtube_description: s("youtube_description").replace(['<', '>'], "").chars().take(1500).collect::<String>().trim().to_string(),
        tiktok_caption: limit(&tiktok, 140),
        facebook_caption: limit(&facebook, 300),
        hashtags: tags,
    }
}

impl Kit {
    pub fn tag_line(&self, n: usize) -> String {
        self.hashtags.iter().take(n).map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")
    }
    /// What goes in the YouTube description box: text, then tags, with #Shorts.
    pub fn youtube_full(&self) -> String {
        let mut tags = self.tag_line(5);
        if !tags.to_lowercase().contains("#shorts") {
            tags = format!("{tags} #Shorts").trim().to_string();
        }
        format!("{}\n\n{}", self.youtube_description.trim(), tags).trim().to_string()
    }
    pub fn tiktok_full(&self) -> String {
        format!("{} {}", self.tiktok_caption, self.tag_line(5)).trim().to_string()
    }
    pub fn facebook_full(&self) -> String {
        format!("{}\n\n{}", self.facebook_caption, self.tag_line(4)).trim().to_string()
    }
    pub fn to_text(&self) -> String {
        format!("YOUTUBE SHORTS\nTitle: {}\n\n{}\n\nTIKTOK\n{}\n\nFACEBOOK REELS\n{}\n", self.youtube_title, self.youtube_full(), self.tiktok_full(), self.facebook_full())
    }
}

fn kv_key(clip_id: &str) -> String {
    format!("kit:{clip_id}")
}

pub fn cached(state: &State, clip_id: &str) -> Option<Kit> {
    serde_json::from_str(&state.kv_get(&kv_key(clip_id))?).ok()
}

pub async fn generate(state: &State, settings: &Settings, clip: &Clip, task: &Task) -> Result<Kit> {
    let text: String = clip.doc.text.chars().take(3000).collect();
    if text.trim().is_empty() && clip.doc.hook_title.is_none() {
        bail!("This clip has no text to write about.");
    }
    let user = format!(
        "Video title: {}\nKey topics: {}\nOn-screen hook of the clip: {}\nWhat is said in the clip:\n{}",
        task.title,
        task.key_topics.join(", "),
        clip.doc.hook_title.clone().unwrap_or_default(),
        text
    );
    let v = llm::json_call(state, settings, &settings.llm, SYSTEM, &user, 90).await?;
    let fallback = clip.doc.hook_title.clone().unwrap_or_else(|| task.title.clone());
    let kit = clean(&v, &fallback);
    state.kv_set(&kv_key(&clip.id), &serde_json::to_string(&kit)?)?;
    Ok(kit)
}

pub fn to_json(kit: &Kit) -> Value {
    json!({
        "youtube_title": kit.youtube_title, "youtube_description": kit.youtube_full(),
        "tiktok_caption": kit.tiktok_full(), "facebook_caption": kit.facebook_full(), "hashtags": kit.hashtags,
    })
}

/// Cover frame: a second or so in, where the on-screen hook title is showing.
async fn cover(state: &State, video: &Path, out: &Path, duration: f64) -> bool {
    let at = (duration * 0.2).clamp(0.5, 2.0);
    command(tool(state, "ffmpeg"))
        .args(["-hide_banner", "-y", "-loglevel", "error", "-ss", &format!("{at:.2}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(out)
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn write_zip(state: &State, clip: &Clip, kit: &Kit, dest: &Path) -> Result<()> {
    use std::io::Write;
    if !Path::new(&clip.file_path).is_file() {
        bail!("This clip is not rendered yet.");
    }
    let tmp = state.data_dir.join("work").join(format!("{}-cover.jpg", clip.id));
    let _ = std::fs::create_dir_all(state.data_dir.join("work"));
    let has_cover = cover(state, Path::new(&clip.file_path), &tmp, clip.duration).await;
    let (video, cover_path, kit_text, kit_json) = (clip.file_path.clone(), tmp.clone(), kit.to_text(), serde_json::to_string_pretty(&to_json(kit))?);
    let dest = dest.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest)?);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
        zip.start_file("clip.mp4", stored)?;
        std::io::copy(&mut std::fs::File::open(video)?, &mut zip)?;
        if has_cover {
            zip.start_file("cover.jpg", stored)?;
            std::io::copy(&mut std::fs::File::open(&cover_path)?, &mut zip)?;
        }
        let text = zip::write::SimpleFileOptions::default();
        zip.start_file("publish.txt", text)?;
        zip.write_all(kit_text.as_bytes())?;
        zip.start_file("publish.json", text)?;
        zip.write_all(kit_json.as_bytes())?;
        zip.finish()?;
        Ok(())
    })
    .await?;
    let _ = std::fs::remove_file(tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_output_is_made_safe() {
        let v = json!({"youtube_title": "A <b>very</b> long title ".repeat(10), "youtube_description": "Line one\nLine two",
            "tiktok_caption": "", "facebook_caption": "Why?", "hashtags": ["#Gaming", "gaming", "GTA 6", "", "x".repeat(40), "tips"]});
        let k = clean(&v, "fallback");
        assert!(k.youtube_title.chars().count() <= 95 && !k.youtube_title.contains('<'));
        assert_eq!(k.hashtags, vec!["gaming", "gta6", "tips"]);
        assert!(!k.tiktok_caption.is_empty() && k.tiktok_caption.chars().count() <= 140);
        assert!(k.youtube_full().ends_with("#Shorts"));
        assert!(k.tiktok_full().contains("#gaming"));
    }

    #[test]
    fn empty_answer_falls_back_to_the_hook() {
        let k = clean(&json!({}), "The hook");
        assert_eq!(k.youtube_title, "The hook");
        assert_eq!(k.facebook_caption, "The hook");
        assert!(k.hashtags.is_empty());
        assert!(k.to_text().contains("The hook"));
    }
}
