//! Shared data types: settings, tasks, clips, transcript.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub llm: String,
    pub rerank_llm: String,
    pub google_api_key: String,
    pub openai_api_key: String,
    pub anthropic_api_key: String,
    pub openrouter_api_key: String,
    pub groq_api_key: String,
    pub ollama_base_url: String,
    pub ollama_api_key: String,
    pub custom_base_url: String,
    pub custom_api_key: String,
    /// assemblyai | whisper_local | groq | openai
    pub transcription_provider: String,
    pub assemblyai_api_key: String,
    pub transcription_language: String,
    pub whisper_model: String,
    pub pexels_api_key: String,
    pub max_clips: u32,
    /// auto | libx264 | h264_nvenc | h264_qsv | h264_amf
    pub encoder: String,
    pub x264_preset: String,
    pub output_height: u32,
    pub loudnorm: bool,
    pub ken_burns: bool,
    pub youtube_max_height: u32,
    pub parallel_renders: u32,
    pub cookies_from_browser: String,
    pub groq_enabled: bool,
    pub groq_model: String,
    pub outro_enabled: bool,
    pub outro_line1: String,
    pub outro_line2: String,
    pub outro_seconds: f64,
    pub fps_30: bool,
    pub notify_done: bool,
    /// Use the video's own subtitles (YouTube etc.) instead of transcribing, when they exist.
    pub yt_subtitles: bool,
    /// Let Gemini look at frames or a small copy of the video (hooks, quiet videos, burned-in subtitles).
    pub gemini_vision: bool,
    /// auto (noticed from the video) | gta6 | off
    pub knowledge_profile: String,
    /// Show Hindi and Urdu captions in Roman letters.
    pub roman_captions: bool,
    /// Words for the speech-to-text to expect (set per task, never saved).
    #[serde(skip)]
    pub vocab: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llm: "google-gla:gemini-3.1-flash-lite".into(),
            rerank_llm: String::new(),
            google_api_key: String::new(),
            openai_api_key: String::new(),
            anthropic_api_key: String::new(),
            openrouter_api_key: String::new(),
            groq_api_key: String::new(),
            ollama_base_url: "http://localhost:11434/v1".into(),
            ollama_api_key: String::new(),
            custom_base_url: String::new(),
            custom_api_key: String::new(),
            transcription_provider: "whisper_local".into(),
            assemblyai_api_key: String::new(),
            transcription_language: String::new(),
            whisper_model: "base".into(),
            pexels_api_key: String::new(),
            max_clips: 10,
            encoder: "auto".into(),
            x264_preset: "veryfast".into(),
            output_height: 1920,
            loudnorm: true,
            ken_burns: false,
            youtube_max_height: 1080,
            parallel_renders: 2,
            cookies_from_browser: String::new(),
            groq_enabled: true,
            groq_model: "openai/gpt-oss-20b".into(),
            outro_enabled: false,
            outro_line1: "FOLLOW FOR MORE".into(),
            outro_line2: String::new(),
            outro_seconds: 2.5,
            fps_30: false,
            notify_done: true,
            yt_subtitles: true,
            gemini_vision: true,
            knowledge_profile: "auto".into(),
            roman_captions: true,
            vocab: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Transcript {
    pub words: Vec<Word>,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub provider: String,
}

/// One timestamped line shown to the LLM.
#[derive(Clone, Debug)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Virality {
    pub hook_score: i64,
    pub engagement_score: i64,
    pub value_score: i64,
    pub shareability_score: i64,
    pub total_score: i64,
    pub hook_type: String,
    pub virality_reasoning: String,
}

impl Default for Virality {
    fn default() -> Self {
        Self {
            hook_score: 15,
            engagement_score: 15,
            value_score: 15,
            shareability_score: 15,
            total_score: 60,
            hook_type: "none".into(),
            virality_reasoning: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BrollPlacement {
    pub start: f64,
    pub duration: f64,
    pub path: String,
    pub query: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FaceCam {
    /// Face centre and face height as fractions of the source frame.
    pub cx: f64,
    pub cy: f64,
    pub h: f64,
    pub found: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CropPlan {
    /// (output time the segment starts at, face centre as a fraction of source width)
    pub segments: Vec<(f64, f64)>,
    pub faces_found: bool,
}

/// Everything needed to (re)render one clip from the source video.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ClipDoc {
    pub hook_title: Option<String>,
    pub text: String,
    pub reasoning: String,
    /// Source ranges in seconds, played in order.
    pub ranges: Vec<[f64; 2]>,
    pub virality: Virality,
    pub relevance: f64,
    pub role: Option<String>,
    pub part_number: Option<u32>,
    pub template: String,
    pub font_family: Option<String>,
    pub font_size: Option<u32>,
    pub font_color: Option<String>,
    pub captions: bool,
    pub show_hook: bool,
    /// Caption text edits keyed by the word's source start in milliseconds. Empty text hides a word.
    pub word_edits: HashMap<String, String>,
    pub broll: Vec<BrollPlacement>,
    pub crop: Option<CropPlan>,
    pub transition: Option<String>,
    pub emoji: bool,
    pub outro: Option<bool>,
    /// auto | gaming (gameplay on top, face below, captions in between) | center
    pub layout: String,
    pub facecam: Option<FaceCam>,
    /// Letterbox bars to cut off: [w, h, x, y]; [0,0,0,0] means none were found.
    pub bars: Option<[i64; 4]>,
    /// Fraction of the picture height to keep from the top: cuts off subtitles burned into the source.
    pub sub_crop: Option<f64>,
}

impl Default for ClipDoc {
    fn default() -> Self {
        Self {
            hook_title: None,
            text: String::new(),
            reasoning: String::new(),
            ranges: vec![],
            virality: Virality::default(),
            relevance: 0.75,
            role: None,
            part_number: None,
            template: "default".into(),
            font_family: None,
            font_size: None,
            font_color: None,
            captions: true,
            show_hook: true,
            word_edits: HashMap::new(),
            broll: vec![],
            crop: None,
            transition: None,
            emoji: false,
            outro: None,
            layout: "auto".into(),
            facecam: None,
            bars: None,
            sub_crop: None,
        }
    }
}

impl ClipDoc {
    pub fn duration(&self) -> f64 {
        self.ranges.iter().map(|r| (r[1] - r[0]).max(0.0)).sum()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct TaskOptions {
    pub clip_length: String,
    pub template: String,
    pub font_family: Option<String>,
    pub font_size: Option<u32>,
    pub font_color: Option<String>,
    pub captions: bool,
    pub hook_titles: bool,
    pub broll: bool,
    pub transition: Option<String>,
    pub emoji: bool,
    pub layout: String,
    pub instructions: String,
    pub topic: String,
    pub part_count: u32,
    pub download_quality: String,
    pub section_start: Option<f64>,
    pub section_end: Option<f64>,
    /// Render only the best N clips now; the rest stay as candidates (0 = render all).
    pub render_top: u32,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            clip_length: "auto".into(),
            template: "default".into(),
            font_family: None,
            font_size: None,
            font_color: None,
            captions: true,
            hook_titles: true,
            broll: false,
            transition: None,
            emoji: false,
            layout: "auto".into(),
            instructions: String::new(),
            topic: String::new(),
            part_count: 3,
            download_quality: "1080p60".into(),
            section_start: None,
            section_end: None,
            render_top: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub source_type: String,
    pub source_url: String,
    pub source_path: String,
    pub status: String,
    pub progress: i64,
    pub message: String,
    pub error: String,
    pub workflow: String,
    pub options: TaskOptions,
    pub summary: String,
    pub key_topics: Vec<String>,
    pub duration: f64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub clip_count: i64,
    #[serde(default)]
    pub thumb: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Clip {
    pub id: String,
    pub task_id: String,
    pub idx: i64,
    pub doc: ClipDoc,
    pub file_path: String,
    pub thumb_path: String,
    pub duration: f64,
    /// ready | rendering | error | queued | candidate (planned, not rendered yet)
    pub status: String,
    pub error: String,
    pub created_at: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct NewTask {
    pub url: Option<String>,
    pub path: Option<String>,
    pub workflow: Option<String>,
    #[serde(default)]
    pub options: TaskOptions,
}

/// One planned clip coming out of AI analysis.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Segment {
    pub ranges: Vec<[f64; 2]>,
    pub text: String,
    pub reasoning: String,
    pub relevance: f64,
    pub virality: Virality,
    pub hook_title: Option<String>,
    pub role: Option<String>,
    pub part_number: Option<u32>,
    /// The hook already came from looking at the picture.
    #[serde(default)]
    pub grounded: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Analysis {
    pub segments: Vec<Segment>,
    pub summary: String,
    pub key_topics: Vec<String>,
}
