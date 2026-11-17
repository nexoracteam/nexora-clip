//! Caption templates, font registry and animated word-synced ASS subtitles
//! (port of `caption_templates.py` + `media/captions.py`).

use crate::models::{ClipDoc, Transcript, Word};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug)]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub font_family: &'static str,
    pub font_size: u32,
    pub font_color: &'static str,
    pub highlight_color: &'static str,
    pub emphasis_color: Option<&'static str>,
    pub stroke_color: Option<&'static str>,
    pub stroke_width: i64,
    pub background: bool,
    pub background_color: Option<&'static str>,
    pub word_box: bool,
    pub word_box_color: Option<&'static str>,
    pub animation: &'static str,
    pub word_pop: bool,
    pub uppercase: bool,
    pub shadow: bool,
    pub glow: bool,
    pub max_words_per_line: usize,
    pub position_y: f64,
}

pub const TEMPLATES: &[Template] = &[
    Template { id: "gaming", name: "Gaming", description: "Big white captions with a thick outline, made for gameplay + facecam", font_family: "ArchivoBlack-Regular", font_size: 40, font_color: "#FFFFFF", highlight_color: "#FFE000", emphasis_color: Some("#FFE000"), stroke_color: Some("#000000"), stroke_width: 6, background: false, background_color: None, word_box: false, word_box_color: None, animation: "karaoke", word_pop: true, uppercase: true, shadow: true, glow: false, max_words_per_line: 3, position_y: 0.54 },
    Template { id: "default", name: "Default", description: "Punchy word-by-word captions with a pop highlight", font_family: "THEBOLDFONT", font_size: 32, font_color: "#FFFFFF", highlight_color: "#FFE000", emphasis_color: Some("#FFE000"), stroke_color: Some("#000000"), stroke_width: 3, background: false, background_color: None, word_box: false, word_box_color: None, animation: "karaoke", word_pop: true, uppercase: false, shadow: true, glow: false, max_words_per_line: 4, position_y: 0.80 },
    Template { id: "pill", name: "Green pill", description: "Bold green highlight with a pill behind the active word", font_family: "THEBOLDFONT", font_size: 32, font_color: "#FFFFFF", highlight_color: "#00FF66", emphasis_color: Some("#FFE000"), stroke_color: Some("#000000"), stroke_width: 4, background: false, background_color: None, word_box: true, word_box_color: Some("#00BF49"), animation: "karaoke", word_pop: true, uppercase: true, shadow: true, glow: false, max_words_per_line: 3, position_y: 0.74 },
    Template { id: "loud", name: "Loud", description: "Large yellow text with red pop highlights", font_family: "THEBOLDFONT", font_size: 34, font_color: "#FFFF00", highlight_color: "#FF2D2D", emphasis_color: Some("#FFFFFF"), stroke_color: Some("#000000"), stroke_width: 5, background: false, background_color: None, word_box: false, word_box_color: None, animation: "karaoke", word_pop: true, uppercase: true, shadow: true, glow: false, max_words_per_line: 3, position_y: 0.70 },
    Template { id: "minimal", name: "Minimal", description: "Clean, subtle captions with a soft background", font_family: "TikTokSans-Regular", font_size: 26, font_color: "#FFFFFF", highlight_color: "#FFFFFF", emphasis_color: None, stroke_color: None, stroke_width: 0, background: true, background_color: Some("#00000080"), word_box: false, word_box_color: None, animation: "fade", word_pop: false, uppercase: false, shadow: false, glow: false, max_words_per_line: 6, position_y: 0.82 },
    Template { id: "tiktok", name: "TikTok", description: "TikTok-style with pink pop highlights", font_family: "TikTokSans-Bold", font_size: 28, font_color: "#FFFFFF", highlight_color: "#FE2C55", emphasis_color: Some("#FE2C55"), stroke_color: Some("#000000"), stroke_width: 3, background: false, background_color: None, word_box: false, word_box_color: None, animation: "karaoke", word_pop: true, uppercase: false, shadow: true, glow: false, max_words_per_line: 4, position_y: 0.78 },
    Template { id: "neon", name: "Neon", description: "Glowing neon text with magenta highlights", font_family: "THEBOLDFONT", font_size: 30, font_color: "#00FFFF", highlight_color: "#FF00FF", emphasis_color: Some("#FF00FF"), stroke_color: Some("#002A6B"), stroke_width: 2, background: false, background_color: None, word_box: false, word_box_color: None, animation: "karaoke", word_pop: true, uppercase: false, shadow: false, glow: true, max_words_per_line: 4, position_y: 0.76 },
    Template { id: "podcast", name: "Podcast", description: "Professional podcast-style captions", font_family: "TikTokSans-Regular", font_size: 28, font_color: "#FFFFFF", highlight_color: "#FFB800", emphasis_color: Some("#FFB800"), stroke_color: Some("#1A1A1A"), stroke_width: 2, background: true, background_color: Some("#1A1A1ACC"), word_box: false, word_box_color: None, animation: "karaoke", word_pop: false, uppercase: false, shadow: false, glow: false, max_words_per_line: 5, position_y: 0.80 },
];

pub fn template(id: &str) -> &'static Template {
    TEMPLATES.iter().find(|t| t.id == id).or_else(|| TEMPLATES.iter().find(|t| t.id == "default")).unwrap_or(&TEMPLATES[0])
}

const POWER_WORDS: &[&str] = &[
    "never", "always", "everything", "nothing", "everyone", "nobody", "anyone", "best", "worst", "most", "biggest", "huge",
    "massive", "tiny", "every", "only", "first", "last", "free", "now", "today", "instantly", "forever", "guaranteed", "proven",
    "secret", "truth", "fact", "literally", "actually", "exactly", "must", "need", "stop", "warning", "danger", "critical", "key",
    "important", "remember", "mistake", "wrong", "right", "perfect", "ultimate", "powerful", "insane", "crazy", "incredible",
    "amazing", "shocking", "viral", "million", "billion", "thousand", "percent", "double", "triple", "ten", "money", "rich",
];


const EMOJI_MAP: &[(&str, &str)] = &[
    ("money", "💰"), ("cash", "💵"), ("dollar", "💵"), ("rich", "🤑"), ("wealth", "💰"), ("million", "💰"), ("billion", "💰"), ("profit", "📈"),
    ("revenue", "📈"), ("income", "💵"), ("salary", "💸"), ("price", "🏷️"), ("free", "🆓"), ("invest", "📊"), ("investing", "📊"), ("stock", "📈"),
    ("crypto", "🪙"), ("bitcoin", "₿"), ("business", "💼"), ("company", "🏢"), ("startup", "🚀"), ("deal", "🤝"), ("sell", "💸"), ("bank", "🏦"),
    ("tax", "🧾"), ("save", "🏦"), ("savings", "🏦"), ("debt", "💳"), ("growth", "📈"), ("grow", "📈"), ("success", "🏆"), ("win", "🏆"),
    ("winning", "🏆"), ("winner", "🥇"), ("goal", "🎯"), ("target", "🎯"), ("result", "✅"), ("best", "🥇"), ("champion", "🏆"), ("boost", "🚀"),
    ("idea", "💡"), ("think", "🤔"), ("thinking", "🤔"), ("smart", "🧠"), ("genius", "🧠"), ("brain", "🧠"), ("mind", "🧠"), ("learn", "📚"),
    ("learning", "📚"), ("study", "📚"), ("book", "📖"), ("question", "❓"), ("secret", "🤫"), ("truth", "💯"), ("fact", "📌"), ("focus", "🎯"),
    ("love", "❤️"), ("heart", "❤️"), ("amazing", "🤩"), ("incredible", "🤯"), ("insane", "🤯"), ("crazy", "🤯"), ("wow", "😮"), ("shocking", "😱"),
    ("scary", "😱"), ("fear", "😱"), ("happy", "😄"), ("sad", "😢"), ("angry", "😡"), ("fire", "🔥"), ("hot", "🔥"), ("cool", "😎"),
    ("perfect", "👌"), ("beautiful", "😍"), ("epic", "🤩"), ("magic", "✨"), ("powerful", "💪"), ("power", "⚡"), ("strong", "💪"), ("energy", "⚡"),
    ("stop", "✋"), ("warning", "⚠️"), ("danger", "⚠️"), ("boom", "💥"), ("time", "⏰"), ("today", "📅"), ("fast", "⚡"), ("quick", "⚡"),
    ("minutes", "⏱️"), ("future", "🔮"), ("forever", "♾️"), ("people", "👥"), ("team", "🤝"), ("family", "👨‍👩‍👧"), ("friend", "🫂"), ("friends", "🫂"),
    ("everyone", "🙌"), ("subscribe", "🔔"), ("viral", "📈"), ("famous", "🌟"), ("customer", "🛍️"), ("boss", "💼"), ("work", "💼"), ("hustle", "💪"),
    ("grind", "💪"), ("build", "🛠️"), ("create", "🎨"), ("health", "🏥"), ("food", "🍽️"), ("eat", "🍴"), ("gym", "🏋️"), ("workout", "🏋️"),
    ("sleep", "😴"), ("water", "💧"), ("run", "🏃"), ("ai", "🤖"), ("robot", "🤖"), ("tech", "💻"), ("computer", "💻"), ("phone", "📱"),
    ("internet", "🌐"), ("online", "🌐"), ("data", "📊"), ("code", "👨‍💻"), ("world", "🌍"), ("space", "🚀"), ("science", "🔬"), ("game", "🎮"),
    ("music", "🎵"), ("video", "🎬"), ("movie", "🎬"), ("car", "🚗"), ("house", "🏠"), ("home", "🏠"), ("travel", "✈️"), ("key", "🔑"),
];

fn emoji_lookup(token: &str) -> Option<&'static str> {
    let find = |t: &str| EMOJI_MAP.iter().find(|(k, _)| *k == t).map(|(_, e)| *e);
    if let Some(e) = find(token) { return Some(e); }
    if token.len() > 3 && token.ends_with('s') && !token.ends_with("ss") { return find(&token[..token.len() - 1]); }
    None
}

/// Rate-limited emoji per word index (max 8, 3 words apart, no repeat within 8 words).
fn annotate_emoji(words: &[Word]) -> std::collections::HashMap<usize, &'static str> {
    let mut out = std::collections::HashMap::new();
    let mut last: i64 = -4;
    let mut recent: std::collections::HashMap<&str, i64> = Default::default();
    for (i, w) in words.iter().enumerate() {
        if out.len() >= 8 { break; }
        let token = normalize_token(&w.text);
        let Some(e) = emoji_lookup(&token) else { continue };
        if (i as i64) - last < 3 { continue; }
        if (i as i64) - recent.get(e).copied().unwrap_or(-9) < 8 { continue; }
        out.insert(i, e);
        last = i as i64;
        recent.insert(e, i as i64);
    }
    out
}

const HOOK_TITLE_SECONDS: f64 = 4.0;
const HOOK_TITLE_MIN_SECONDS: f64 = 1.5;
const HOOK_TITLE_SIDE_MARGIN_FRAC: f64 = 0.07;
const HOOK_TITLE_TOP_MARGIN_FRAC: f64 = 0.12;

fn normalize_token(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '%').collect()
}

fn is_emphasis(text: &str) -> bool {
    let t = normalize_token(text);
    !t.is_empty() && (POWER_WORDS.contains(&t.as_str()) || t.chars().any(|c| c.is_ascii_digit()))
}

// ---------- fonts ----------

#[derive(Serialize, Clone, Debug)]
pub struct FontInfo {
    /// File stem; this is what templates and the UI refer to.
    pub name: String,
    /// Internal family name libass matches on.
    pub family: String,
    pub path: PathBuf,
}

pub fn list_fonts(dir: &Path) -> Vec<FontInfo> {
    let mut fonts = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if ext != "ttf" && ext != "otf" {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let family = std::fs::read(&path).ok().and_then(|d| family_name(&d)).unwrap_or_else(|| stem.clone());
        fonts.push(FontInfo { name: stem, family, path });
    }
    fonts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    fonts
}

fn family_name(data: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    // libass matches a font by family or full name. ASS \fn override tags end
    // at "(", "{" or "\", so a family like "THE BOLD FONT (FREE VERSION)" is
    // unusable there; fall back to the full name, which libass accepts too.
    for id in [ttf_parser::name_id::FAMILY, ttf_parser::name_id::FULL_NAME] {
        let mut names: Vec<String> = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .filter_map(|n| n.to_string())
            .filter(|n| !n.is_empty() && !n.contains(['(', '{', '\\', ',']))
            .collect();
        names.sort_by_key(|n| n.len());
        if let Some(name) = names.into_iter().next() {
            return Some(name);
        }
    }
    None
}

struct Measurer {
    data: Option<Vec<u8>>,
    px: f64,
}

impl Measurer {
    fn new(font: Option<&FontInfo>, px: f64) -> Self {
        Self { data: font.and_then(|f| std::fs::read(&f.path).ok()), px }
    }
    fn width(&self, text: &str) -> f64 {
        let fallback = text.chars().count() as f64 * self.px * 0.62;
        let Some(data) = &self.data else { return fallback };
        let Ok(face) = ttf_parser::Face::parse(data, 0) else { return fallback };
        // libass sizes a font by its ascender-descender height, not its em square.
        let height = (face.ascender() as f64 - face.descender() as f64).max(1.0);
        let scale = self.px / height;
        let avg = face.units_per_em() as f64 * 0.55;
        text.chars()
            .map(|c| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).map(|a| a as f64).unwrap_or(avg))
            .sum::<f64>()
            * scale
    }
}

// ---------- ASS primitives ----------

fn ass_ts(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let h = (s / 3600.0).floor() as i64;
    let m = ((s % 3600.0) / 60.0).floor() as i64;
    let rest = s - (h * 3600 + m * 60) as f64;
    format!("{h}:{m:02}:{rest:05.2}")
}

fn ass_color(value: Option<&str>, fallback: &str) -> String {
    let parse = |v: &str| -> Option<(String, u8)> {
        let v = v.trim().trim_start_matches('#');
        if !(v.len() == 6 || v.len() == 8) || !v.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let alpha = if v.len() == 8 { 255 - u8::from_str_radix(&v[6..8], 16).ok()? } else { 0 };
        Some((v[..6].to_uppercase(), alpha))
    };
    let (rgb, alpha) = value.and_then(parse).or_else(|| parse(fallback)).unwrap_or(("FFFFFF".into(), 0));
    format!("&H{:02X}{}{}{}&", alpha, &rgb[4..6], &rgb[2..4], &rgb[0..2])
}

fn esc(value: &str) -> String {
    value.replace('\\', "\\\\").replace('{', "\\{").replace('}', "\\}").replace(['\n', '\r'], " ").trim().to_string()
}

fn scaled_font_size(base: u32, video_width: i64) -> i64 {
    ((base as f64 * (video_width as f64 / 560.0)).round() as i64).clamp(26, 132)
}

fn safe_vertical_position(video_height: i64, text_height: i64, position_y: f64) -> i64 {
    let min_top = 40.max((video_height as f64 * 0.05) as i64);
    let min_bottom = 120.max((video_height as f64 * 0.10) as i64);
    let desired = (video_height as f64 * position_y) as i64 - text_height / 2;
    let max_y = video_height - min_bottom - text_height;
    min_top.max(desired.min(max_y))
}

/// Split one caption chunk into runs that fit `max_width`, cutting where the wider half is narrowest.
fn split_chunk(widths: &[f64], space: f64, max_width: f64) -> Vec<(usize, usize)> {
    fn width(widths: &[f64], space: f64, a: usize, b: usize) -> f64 {
        widths[a..b].iter().sum::<f64>() + space * (b.saturating_sub(a + 1)) as f64
    }
    fn go(widths: &[f64], space: f64, max_width: f64, a: usize, b: usize, out: &mut Vec<(usize, usize)>) {
        if b - a <= 1 || width(widths, space, a, b) <= max_width {
            out.push((a, b));
            return;
        }
        let cut = (a + 1..b)
            .min_by(|x, y| {
                let wx = width(widths, space, a, *x).max(width(widths, space, *x, b));
                let wy = width(widths, space, a, *y).max(width(widths, space, *y, b));
                wx.total_cmp(&wy).then(y.cmp(x))
            })
            .unwrap();
        go(widths, space, max_width, a, cut, out);
        go(widths, space, max_width, cut, b, out);
    }
    let mut out = Vec::new();
    if !widths.is_empty() {
        go(widths, space, max_width, 0, widths.len(), &mut out);
    }
    out
}

fn title_lines(title: &str, max_chars: usize) -> Vec<String> {
    let words: Vec<&str> = title.split_whitespace().collect();
    if title.chars().count() <= max_chars || words.len() < 2 {
        return vec![words.join(" ")];
    }
    // Two balanced lines.
    let best = (1..words.len())
        .min_by_key(|i| {
            let a = words[..*i].join(" ").chars().count();
            let b = words[*i..].join(" ").chars().count();
            a.max(b)
        })
        .unwrap();
    vec![words[..best].join(" "), words[best..].join(" ")]
}

// ---------- clip word timeline ----------

pub struct ClipWords {
    /// Words on the clip's own (output) timeline.
    pub words: Vec<Word>,
    /// Word indices that open a new source range (captions never straddle a cut).
    pub breaks: Vec<usize>,
    /// Edit key (source start in ms) per word.
    pub keys: Vec<String>,
}

pub fn word_key(source_start: f64) -> String {
    format!("{}", (source_start * 1000.0).round() as i64)
}

/// Words of a clip mapped onto its output timeline, with caption edits applied.
pub fn clip_words(transcript: &Transcript, doc: &ClipDoc, include_hidden: bool) -> ClipWords {
    let mut out = ClipWords { words: vec![], breaks: vec![], keys: vec![] };
    let mut offset = 0.0;
    for (ri, r) in doc.ranges.iter().enumerate() {
        let (a, b) = (r[0], r[1]);
        if ri > 0 && !out.words.is_empty() {
            out.breaks.push(out.words.len());
        }
        for w in transcript.words.iter().filter(|w| w.start >= a - 0.06 && w.start < b - 0.02) {
            let key = word_key(w.start);
            let text = match doc.word_edits.get(&key) {
                Some(t) if t.trim().is_empty() && !include_hidden => continue,
                Some(t) => t.trim().to_string(),
                None => w.text.trim().to_string(),
            };
            let start = offset + (w.start - a).max(0.0);
            let end = (offset + (w.end - a)).min(offset + (b - a)).max(start + 0.04);
            out.words.push(Word { text, start, end, speaker: w.speaker.clone() });
            out.keys.push(key);
        }
        offset += (b - a).max(0.0);
    }
    out
}

// ---------- the subtitle file ----------

pub struct AssInput<'a> {
    pub words: &'a [Word],
    pub breaks: &'a [usize],
    pub hook_title: Option<&'a str>,
    pub template: &'a Template,
    pub font_family: Option<&'a str>,
    pub font_size: Option<u32>,
    pub font_color: Option<&'a str>,
    pub width: i64,
    pub height: i64,
    pub duration: f64,
    pub fonts: &'a [FontInfo],
    pub emoji: bool,
    /// Caption centre as a fraction of the height (overrides the template and the safe-area clamp).
    pub pos_y: Option<f64>,
    /// Distance of the hook title from the top edge, as a fraction of the height.
    pub title_top: Option<f64>,
    /// Keep the hook title on screen for the whole clip.
    pub title_always: bool,
}

fn hook_title_ass(i: &AssInput, title: &str, font_name: &str, caption_px: i64) -> (String, String) {
    let t = i.template;
    let title_text = if t.uppercase { title.to_uppercase() } else { title.to_string() };
    let primary = ass_color(Some(t.font_color), "#FFFFFF");
    let highlight = ass_color(t.emphasis_color.or(Some(t.highlight_color)), "#FFE000");
    let outline = ass_color(t.stroke_color.or(Some("#000000")), "#000000");
    let back = ass_color(t.background_color, "#00000080");

    let base_px = 96.min((caption_px + 8).max((caption_px as f64 * 1.35).round() as i64));
    let usable = i.width - 2 * 48.max((i.width as f64 * HOOK_TITLE_SIDE_MARGIN_FRAC) as i64);
    let max_chars = 10.max((usable as f64 / (base_px as f64 * 0.52)) as usize);
    let lines = title_lines(&title_text, max_chars);
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1);
    let mut hook_px = base_px;
    if longest > max_chars {
        hook_px = base_px.min(1.max((usable as f64 / (longest as f64 * 0.52)) as i64));
    }
    let has_outline = t.stroke_color.is_some() && t.stroke_width > 0;
    let border_style = if !has_outline && t.background_color.is_some() { 3 } else { 1 };
    let mut outline_px = if has_outline { t.stroke_width.max((hook_px as f64 * t.stroke_width as f64 / 26.0).round() as i64) } else { 0 };
    if border_style == 3 {
        outline_px = 4.max(hook_px / 6);
    } else if outline_px == 0 {
        outline_px = 2.max(hook_px / 16);
    }
    let shadow_px = if t.shadow { 2.max(hook_px / 20) } else { 0 };
    let margin_v = match i.title_top { Some(f) => (i.height as f64 * f) as i64, None => 48.max((i.height as f64 * HOOK_TITLE_TOP_MARGIN_FRAC) as i64) };
    let style = format!(
        "Style: Hook,{font_name},{hook_px},{primary},&H000000FF,{outline},{back},1,0,0,0,100,100,0,0,{border_style},{outline_px},{shadow_px},8,60,60,{margin_v},1"
    );
    let rendered: Vec<String> = lines
        .iter()
        .map(|line| {
            line.split_whitespace()
                .map(|w| format!("{{\\c{}}}{}", if is_emphasis(w) { &highlight } else { &primary }, esc(w)))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let (mut start, mut end) = (0.12, HOOK_TITLE_SECONDS.min(HOOK_TITLE_MIN_SECONDS.max(i.duration - 0.25)));
    if i.duration <= HOOK_TITLE_MIN_SECONDS || i.title_always {
        start = if i.title_always { 0.0 } else { start };
        end = i.duration.max(0.5);
    }
    let span = (end - start).max(0.001);
    let (fade_in, fade_out) = ((0.16f64).min(span / 3.0), (0.24f64).min(span / 3.0));
    let mut entrance = format!("\\fad({},{})", (fade_in * 1000.0).round() as i64, (fade_out * 1000.0).round() as i64);
    if t.word_pop {
        entrance.push_str(&format!("\\fscx90\\fscy90\\t(0,{},\\fscx100\\fscy100)", (fade_in * 1000.0).round() as i64));
    }
    let event = format!("Dialogue: 1,{},{},Hook,,0,0,0,,{{{entrance}}}{}", ass_ts(start), ass_ts(end), rendered.join("\\N"));
    (style, event)
}

pub fn build_ass(i: &AssInput) -> String {
    let t = i.template;
    let family_stem = i.font_family.filter(|f| !f.is_empty()).unwrap_or(t.font_family);
    let font = i.fonts.iter().find(|f| f.name.eq_ignore_ascii_case(family_stem));
    let font_name = font.map(|f| f.family.clone()).unwrap_or_else(|| family_stem.to_string());
    let font_px = scaled_font_size(i.font_size.unwrap_or(t.font_size), i.width);
    let animation = t.animation;

    let primary = ass_color(i.font_color.or(Some(t.font_color)), "#FFFFFF");
    let highlight = ass_color(Some(t.highlight_color), "#FFE000");
    let emphasis_color = ass_color(t.emphasis_color.or(Some(t.highlight_color)), "#FFE000");
    let outline = ass_color(t.stroke_color.or(Some("#000000")), "#000000");
    let back = ass_color(t.background_color, "#00000080");
    let box_color = ass_color(t.word_box_color.or(Some(t.highlight_color)), "#00BF49");

    let has_outline = t.stroke_color.is_some();
    let outline_px = if has_outline && t.stroke_width > 0 { t.stroke_width.max((font_px as f64 * t.stroke_width as f64 / 26.0).round() as i64) } else { 0 };
    let shadow_px = if t.shadow { 2.max(font_px / 20) } else { 0 };
    let box_bord = (outline_px + 2).max(font_px / 5);
    let est_text_height = (font_px as f64 * 1.5) as i64;
    let y_pos = match i.pos_y { Some(f) => (i.height as f64 * f) as i64, None => safe_vertical_position(i.height, est_text_height, t.position_y) };
    let x_pos = i.width / 2;
    let border_style = if t.background && t.background_color.is_some() { 3 } else { 1 };
    let (style_outline, style_bord) = if border_style == 3 { (back.clone(), outline_px.max(font_px / 6)) } else { (outline.clone(), outline_px) };

    let (hook_style, hook_event) = match i.hook_title.filter(|h| !h.trim().is_empty()) {
        Some(title) => {
            let (s, e) = hook_title_ass(i, title.trim(), &font_name, font_px);
            (format!("{s}\n"), Some(e))
        }
        None => (String::new(), None),
    };

    let emphasis_on = animation != "none";
    let display = |w: &Word| if t.uppercase { w.text.to_uppercase() } else { w.text.clone() };
    let measurer = Measurer::new(font, font_px as f64);
    let space_width = measurer.width(" ");
    let edge = style_bord.max(if t.word_box { box_bord } else { 0 }) + shadow_px;
    let peak = match animation { "pop" => 1.06, "bounce" => 1.12, _ => 1.0 };
    let max_text_width = 200.max(i.width - 2 * 40.max((i.width as f64 * 0.06) as i64));
    let available = (max_text_width - 2 * edge) as f64 / peak;
    let emoji_by_idx = if i.emoji { annotate_emoji(i.words) } else { Default::default() };
    let widths: Vec<f64> = i.words.iter().enumerate().map(|(idx, w)| measurer.width(&display(w)) + if emoji_by_idx.contains_key(&idx) { space_width + font_px as f64 * 1.2 } else { 0.0 }).collect();

    // Lines of N words that never straddle a cut, split again if too wide.
    let chunk_size = t.max_words_per_line.max(1);
    let mut spans: Vec<(usize, usize, f64)> = Vec::new();
    let mut limits: Vec<usize> = i.breaks.to_vec();
    limits.push(i.words.len());
    limits.sort();
    limits.dedup();
    let mut start = 0;
    for limit in limits {
        while start < limit.min(i.words.len()) {
            let stop = limit.min(i.words.len()).min(start + chunk_size);
            let w = &widths[start..stop];
            for (a, b) in split_chunk(w, space_width, available) {
                let line_width = w[a..b].iter().sum::<f64>() + space_width * (b - a - 1) as f64;
                let scale = if line_width > 0.0 { (available / line_width).min(1.0) } else { 1.0 };
                spans.push((start + a, start + b, scale));
            }
            start = stop;
        }
    }

    let header = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,{font_name},{font_px},{primary},&H000000FF,{style_outline},{back},1,0,0,0,100,100,0,0,{border_style},{style_bord},{shadow_px},5,60,60,60,1\n{hook_style}\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        w = i.width, h = i.height
    );

    let pct = |percent: f64, scale: f64| ((percent * scale).round() as i64).max(1);
    let font_tag = format!("\\fn{font_name}");
    let mut events: Vec<String> = Vec::new();
    if let Some(e) = hook_event {
        events.push(e);
    }
    for (cs, ce, scale) in spans {
        let chunk = &i.words[cs..ce];
        let chunk_end = chunk[chunk.len() - 1].end;
        let mut prefix_tags = format!("\\pos({x_pos},{y_pos})");
        if t.glow { prefix_tags.push_str("\\blur4"); }
        if scale < 1.0 { prefix_tags.push_str(&format!("\\fscx{0}\\fscy{0}", pct(100.0, scale))); }
        let prefix = format!("{{{prefix_tags}}}");
        if animation == "karaoke" {
            for (li, word) in chunk.iter().enumerate() {
                let start = word.start;
                let mut end = if li + 1 < chunk.len() { chunk[li + 1].start } else { chunk_end };
                if end <= start { end = start + 0.05; }
                let parts: Vec<String> = chunk.iter().enumerate().map(|(lj, other)| {
                    let mut disp = esc(&display(other));
                    if let Some(e) = emoji_by_idx.get(&(cs + lj)) { disp = format!("{disp} {{\\fnSegoe UI Emoji}}{e}"); }
                    if lj == li {
                        let mut tags = format!("{font_tag}\\c{highlight}");
                        if t.word_box { tags.push_str(&format!("\\3c{box_color}\\bord{box_bord}\\shad0")); }
                        format!("{{{tags}}}{disp}")
                    } else {
                        let color = if emphasis_on && is_emphasis(&other.text) { &emphasis_color } else { &primary };
                        let mut tags = format!("{font_tag}\\c{color}");
                        if t.word_box { tags.push_str(&format!("\\3c{outline}\\bord{outline_px}\\shad{shadow_px}")); }
                        format!("{{{tags}}}{disp}")
                    }
                }).collect();
                // One-shot line entrance on the first word only, so the line never vibrates.
                let entrance = if t.word_pop && li == 0 {
                    format!("{{\\fscx{0}\\fscy{0}\\t(0,140,\\fscx{1}\\fscy{1})}}", pct(92.0, scale), pct(100.0, scale))
                } else { String::new() };
                events.push(format!("Dialogue: 0,{},{},Default,,0,0,0,,{prefix}{entrance}{}", ass_ts(start), ass_ts(end), parts.join(" ")));
            }
        } else {
            let start = chunk[0].start;
            let end = if chunk_end <= start { start + 0.05 } else { chunk_end };
            let text: Vec<String> = chunk.iter().enumerate().map(|(lj, w)| {
                let color = if emphasis_on && is_emphasis(&w.text) { &emphasis_color } else { &primary };
                let mut disp = esc(&display(w));
                if let Some(e) = emoji_by_idx.get(&(cs + lj)) { disp = format!("{disp} {{\\fnSegoe UI Emoji}}{e}"); }
                format!("{{{font_tag}\\c{color}}}{disp}")
            }).collect();
            let effect = match animation {
                "fade" => "{\\fad(120,120)}".to_string(),
                "pop" | "bounce" => {
                    let (low, peak, rise, settle) = if animation == "pop" { (88.0, 106.0, 130, 250) } else { (70.0, 112.0, 120, 240) };
                    format!("{{\\fscx{0}\\fscy{0}\\t(0,{rise},\\fscx{1}\\fscy{1})\\t({rise},{settle},\\fscx{2}\\fscy{2})}}", pct(low, scale), pct(peak, scale), pct(100.0, scale))
                }
                _ => String::new(),
            };
            events.push(format!("Dialogue: 0,{},{},Default,,0,0,0,,{prefix}{effect}{}", ass_ts(start), ass_ts(end), text.join(" ")));
        }
    }
    format!("{header}{}\n", events.join("\n"))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_primitives() {
        assert_eq!(ass_ts(3725.5), "1:02:05.50");
        assert_eq!(ass_color(Some("#FF0000"), "#FFFFFF"), "&H000000FF&");
        assert_eq!(ass_color(Some("#00000080"), "#FFFFFF"), "&H7F000000&");
        assert_eq!(ass_color(Some("nonsense"), "#FFFFFF"), "&H00FFFFFF&");
        assert_eq!(esc("a{b}\\c\n"), "a\\{b\\}\\\\c");
    }

    #[test]
    fn long_captions_are_split_to_fit() {
        let parts = split_chunk(&[400.0, 400.0, 400.0, 400.0], 20.0, 900.0);
        assert!(parts.len() >= 2);
        assert_eq!(parts.first().unwrap().0, 0);
        assert_eq!(parts.last().unwrap().1, 4);
    }

    #[test]
    fn templates_resolve() {
        assert_eq!(template("nope").id, "default");
        assert_eq!(template("gaming").uppercase, true);
        assert!(TEMPLATES.len() >= 8);
    }

    #[test]
    fn subtitle_file_has_every_word_and_the_title() {
        let words: Vec<Word> = (0..8).map(|i| Word { text: format!("word{i}"), start: i as f64 * 0.5, end: i as f64 * 0.5 + 0.4, speaker: None }).collect();
        let ass = build_ass(&AssInput { words: &words, breaks: &[], hook_title: Some("Big title"), template: template("gaming"), font_family: None, font_size: None, font_color: None,
            width: 1080, height: 1920, duration: 4.0, fonts: &[], emoji: false, pos_y: Some(0.54), title_top: Some(0.035), title_always: true });
        assert!(ass.contains("Style: Hook"));
        assert!(ass.contains("WORD7"), "uppercase template");
        assert_eq!(ass.matches("Dialogue: 0,").count(), 8, "one karaoke event per word");
        assert!(ass.contains("\\pos(540,1036)"));
    }

    #[test]
    fn edits_hide_and_change_words() {
        let t = Transcript { words: vec![
            Word { text: "keep".into(), start: 1.0, end: 1.4, speaker: None },
            Word { text: "umm".into(), start: 1.5, end: 1.8, speaker: None },
            Word { text: "this".into(), start: 2.0, end: 2.4, speaker: None }], ..Default::default() };
        let mut doc = ClipDoc { ranges: vec![[0.5, 3.0]], ..Default::default() };
        doc.word_edits.insert(word_key(1.5), String::new());
        doc.word_edits.insert(word_key(2.0), "THAT".into());
        let cw = clip_words(&t, &doc, false);
        assert_eq!(cw.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), vec!["keep", "THAT"]);
        assert_eq!(clip_words(&t, &doc, true).words.len(), 3);
    }
}
