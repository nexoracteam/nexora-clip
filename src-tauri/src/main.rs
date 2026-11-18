#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod applog;
mod secret;
mod gemini;
mod vision;
mod quota;
mod remote;
mod knowledge;
mod roman;
mod subs;
mod kit;
mod ai_edit;
mod broll;
mod captions;
mod chatgpt;
mod download;
mod editor;
mod face;
mod ffmpeg;
mod llm;
mod models;
mod outro;
mod pipeline;
mod publish;
mod render;
mod state;
mod tools;
mod transcribe;
mod tray;
mod visual;

use models::*;
use serde_json::{json, Value};
use state::{now, AppState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

type S<'a> = tauri::State<'a, state::State>;
type R<T> = Result<T, String>;

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

// ---------- settings & tools ----------

#[tauri::command]
fn get_settings(state: S) -> Settings {
    state.settings()
}

#[tauri::command]
fn save_settings(state: S, app: AppHandle, settings: Settings) -> R<()> {
    *state.encoder_cache.lock().unwrap() = None;
    let before = state.settings();
    state.save_settings(&settings).map_err(e)?;
    tray::apply(&app, &settings);
    if before.remote_enabled != settings.remote_enabled || before.remote_lan != settings.remote_lan || before.remote_port != settings.remote_port {
        remote::reconfigure(&app);
    }
    Ok(())
}

#[tauri::command]
fn remote_status(state: S) -> Value {
    remote::status(&state)
}

#[tauri::command]
fn remote_reset_pairing(state: S, app: AppHandle) -> R<()> {
    remote::reset_token(&state).map_err(e)?;
    remote::reconfigure(&app);
    Ok(())
}

#[tauri::command]
async fn remote_tailscale_setup(state: S<'_>) -> R<String> {
    remote::tailscale_setup(&state).await.map_err(e)
}

#[tauri::command]
fn remote_forget_tailscale(state: S) {
    remote::forget_tailscale(&state)
}

#[tauri::command]
async fn tools_status(state: S<'_>) -> R<Vec<tools::ToolStatus>> {
    Ok(tools::status(&state).await)
}

#[tauri::command]
async fn tool_updates(state: S<'_>, force: Option<bool>) -> R<Value> {
    Ok(tools::updates(&state, force.unwrap_or(false)).await)
}

#[tauri::command]
async fn install_tool(state: S<'_>, app: AppHandle, name: String) -> R<()> {
    tools::install(&state, &app, &name).await.map_err(e)
}

#[tauri::command]
async fn app_info(state: S<'_>) -> R<Value> {
    let settings = state.settings();
    let encoder = ffmpeg::resolve_encoder(&state, &settings).await;
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": state.data_dir.to_string_lossy(),
        "encoder": encoder,
    }))
}

#[tauri::command]
async fn test_llm(state: S<'_>) -> R<Value> {
    let settings = state.settings();
    llm::test_model(&state, &settings, &settings.llm).await.map_err(e)
}

#[tauri::command]
fn chatgpt_status(state: S) -> Value {
    chatgpt::status(&state)
}

#[tauri::command]
async fn chatgpt_login(state: S<'_>, app: AppHandle) -> R<()> {
    chatgpt::login(app, Arc::clone(&state)).await.map_err(e)
}

#[tauri::command]
async fn chatgpt_disconnect(state: S<'_>) -> R<()> {
    chatgpt::disconnect(&state).await.map_err(e)
}

#[tauri::command]
async fn chatgpt_models(state: S<'_>) -> R<Vec<Value>> {
    chatgpt::models(&state).await.map_err(e)
}

#[tauri::command]
fn chatgpt_set_model(state: S, model: String) -> R<()> {
    chatgpt::set_model(&state, &model).map_err(e)
}

#[tauri::command]
async fn list_ollama(state: S<'_>) -> R<Vec<String>> {
    Ok(llm::ollama_models(&state, &state.settings()).await)
}

#[tauri::command]
fn ai_usage(state: S) -> Value {
    quota::usage(&state)
}

#[tauri::command]
fn ai_reset_rest() {
    quota::reset();
}

#[tauri::command]
async fn probe_url(state: S<'_>, app: AppHandle, url: String) -> R<download::Probe> {
    tools::ensure_downloader(&state, &app).await.map_err(e)?;
    let settings = state.settings();
    download::probe(&state, &settings, &url).await.map_err(e)
}

#[tauri::command]
fn groq_status(state: S) -> Value {
    llm::groq_status(&state.settings())
}

#[tauri::command]
async fn test_groq(state: S<'_>) -> R<Value> {
    let settings = state.settings();
    llm::test_groq(&state, &settings).await.map_err(e)
}

#[tauri::command]
async fn outro_preview(state: S<'_>) -> R<String> {
    let settings = state.settings();
    outro::preview(&state, &settings).await.map(|p| p.to_string_lossy().to_string()).map_err(e)
}

#[tauri::command]
async fn outro_set_logo(state: S<'_>, path: String) -> R<()> {
    outro::set_logo(&state, Path::new(&path)).await.map_err(e)
}

#[tauri::command]
fn list_templates() -> Vec<captions::Template> {
    captions::TEMPLATES.to_vec()
}

#[tauri::command]
fn list_fonts(state: S) -> Vec<captions::FontInfo> {
    pipeline::fonts(&state)
}

#[tauri::command]
fn add_font(state: S, path: String) -> R<()> {
    let src = PathBuf::from(&path);
    let ext = src.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
    if ext != "ttf" && ext != "otf" {
        return Err("Fonts must be .ttf or .otf files.".into());
    }
    let name = src.file_name().ok_or("Invalid font path")?;
    std::fs::copy(&src, state.data_dir.join("fonts").join(name)).map_err(e)?;
    Ok(())
}

// ---------- tasks ----------

fn spawn_task(app: &AppHandle, state: &state::State, id: &str) {
    state.cancels.lock().unwrap().insert(id.to_string(), Arc::new(AtomicBool::new(false)));
    let (app, state, id) = (app.clone(), state.inner_clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let worker = tauri::async_runtime::spawn(pipeline::run_task(app.clone(), state.clone(), id.clone()));
        if worker.await.is_err() {
            // A panic inside the task: never leave it "processing" forever.
            applog::log(&format!("task {id} crashed"));
            state.cancels.lock().unwrap().remove(&id);
            let _ = state.set_task_fields(&id, &[("status", &"error"), ("error", &"Something unexpected went wrong while processing. Press Resume to try again; the log has the details."), ("message", &"Failed")]);
            pipeline::emit_task(&app, &state, &id);
        }
    });
}

trait InnerClone {
    fn inner_clone(&self) -> state::State;
}
impl InnerClone for state::State {
    fn inner_clone(&self) -> state::State {
        Arc::clone(self)
    }
}

/// Catch the obvious problems before a long download and transcription are wasted on them.
fn preflight(state: &state::State, workflow: &str) -> Result<(), String> {
    let s = state.settings();
    if !ffmpeg::tool_works(state, "ffmpeg") || !ffmpeg::tool_works(state, "ffprobe") {
        return Err("FFmpeg is missing. Open Settings → Tools and press \"Install everything missing\".".into());
    }
    match s.transcription_provider.as_str() {
        "whisper_local" => {
            if tools::whisper_exe(state).is_none() || !tools::whisper_model_path(state, &s.whisper_model).is_file() {
                return Err("Local transcription is not set up. Open Settings → Tools and install whisper.cpp and its speech model, or pick another transcription provider.".into());
            }
        }
        "assemblyai" if s.assemblyai_api_key.trim().is_empty() => return Err("Add your AssemblyAI API key in Settings, or switch transcription to local Whisper.".into()),
        "groq" if s.groq_api_key.trim().is_empty() => return Err("Add your Groq API key in Settings to transcribe with Groq.".into()),
        "openai" if s.openai_api_key.trim().is_empty() => return Err("Add your OpenAI API key in Settings to transcribe with OpenAI.".into()),
        _ => {}
    }
    // Highlights still work without any AI (clips are picked by sound). The other workflows need one.
    let groq_ready = s.groq_enabled && !s.groq_api_key.trim().is_empty();
    if !groq_ready && workflow != "highlights" {
        if let Some(problem) = llm::config_error(&s, &s.llm) {
            return Err(format!("[ai_setup] {problem} (Settings → AI model)"));
        }
    }
    if ffmpeg::free_space(&state.data_dir) < 3_000_000_000 {
        return Err("Less than 3 GB of free disk space is left. Free some space first (Settings → Storage can clear leftovers).".into());
    }
    Ok(())
}

#[tauri::command]
fn create_task(state: S, app: AppHandle, input: NewTask) -> R<Task> {
    let url = input.url.unwrap_or_default().trim().to_string();
    let path = input.path.unwrap_or_default().trim().to_string();
    if url.is_empty() && path.is_empty() {
        return Err("Paste a video link or choose a video file.".into());
    }
    let workflow = input.workflow.unwrap_or_else(|| "highlights".into());
    preflight(&state, &workflow)?;
    if !["highlights", "series", "topic", "recap", "trailer"].contains(&workflow.as_str()) {
        return Err(format!("Unknown workflow: {workflow}"));
    }
    if workflow == "topic" && input.options.topic.trim().is_empty() {
        return Err("Enter a topic for the Topic workflow.".into());
    }
    let (source_type, title) = if !path.is_empty() {
        if !Path::new(&path).is_file() {
            return Err(format!("File not found: {path}"));
        }
        ("file", Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
    } else {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("That does not look like a video link.".into());
        }
        ("url", url.clone())
    };
    let task = Task {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        source_type: source_type.into(),
        source_url: url,
        source_path: path,
        status: "queued".into(),
        message: "Waiting to start".into(),
        workflow,
        options: input.options,
        created_at: now(),
        updated_at: now(),
        ..Default::default()
    };
    state.insert_task(&task).map_err(e)?;
    spawn_task(&app, &state, &task.id);
    Ok(task)
}

#[tauri::command]
fn list_tasks(state: S) -> R<Vec<Task>> {
    state.list_tasks().map_err(e)
}

#[tauri::command]
fn get_task(state: S, id: String) -> R<Value> {
    let task = state.get_task(&id).map_err(e)?;
    let clips = state.list_clips(&id).map_err(e)?;
    Ok(json!({"task": task, "clips": clips}))
}

#[tauri::command]
fn cancel_task(state: S, app: AppHandle, id: String) -> R<()> {
    let flag = state.cancels.lock().unwrap().get(&id).cloned();
    match flag {
        Some(f) => f.store(true, Ordering::Relaxed),
        None => {
            state.set_task_fields(&id, &[("status", &"cancelled"), ("message", &"Cancelled")]).map_err(e)?;
            pipeline::emit_task(&app, &state, &id);
        }
    }
    Ok(())
}

#[tauri::command]
fn resume_task(state: S, app: AppHandle, id: String) -> R<()> {
    if state.cancels.lock().unwrap().contains_key(&id) {
        return Err("This task is already running.".into());
    }
    state.set_task_fields(&id, &[("status", &"queued"), ("error", &""), ("message", &"Waiting to start")]).map_err(e)?;
    spawn_task(&app, &state, &id);
    pipeline::emit_task(&app, &state, &id);
    Ok(())
}

#[tauri::command]
fn delete_task(state: S, id: String) -> R<()> {
    if let Some(f) = state.cancels.lock().unwrap().get(&id) {
        f.store(true, Ordering::Relaxed);
    }
    let _ = std::fs::remove_dir_all(state.data_dir.join("clips").join(&id));
    // Only files this app downloaded or derived live in <data>/sources; a user's own file is never touched.
    let _ = std::fs::remove_dir_all(state.data_dir.join("sources").join(&id));
    if let Ok(entries) = std::fs::read_dir(state.data_dir.join("sources")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&id) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    state.delete_task(&id).map_err(e)
}

// ---------- clip editing ----------

fn spawn_render(app: &AppHandle, state: &state::State, clip_id: &str) {
    let (app, state, id) = (app.clone(), Arc::clone(state), clip_id.to_string());
    tauri::async_runtime::spawn(async move {
        let worker = tauri::async_runtime::spawn(pipeline::render_clip_job(app.clone(), state.clone(), id.clone()));
        if worker.await.is_err() {
            applog::log(&format!("clip render {id} crashed"));
            if let Ok(mut clip) = state.get_clip(&id) {
                clip.status = "error".into();
                clip.error = "The render hit an unexpected problem. Try again.".into();
                let _ = state.upsert_clip(&clip);
                pipeline::emit_clip(&app, &state, &id);
            }
        }
    });
}

fn busy(clip: &Clip) -> R<()> {
    if clip.status == "rendering" {
        return Err("This clip is still rendering.".into());
    }
    Ok(())
}

#[tauri::command]
fn update_clip(state: S, app: AppHandle, clip_id: String, patch: Value) -> R<Clip> {
    let mut clip = state.get_clip(&clip_id).map_err(e)?;
    busy(&clip)?;
    let task = state.get_task(&clip.task_id).map_err(e)?;
    editor::apply_patch(&mut clip.doc, &patch, task.duration).map_err(e)?;
    clip.status = "rendering".into();
    state.upsert_clip(&clip).map_err(e)?;
    spawn_render(&app, &state, &clip.id);
    Ok(clip)
}

#[tauri::command]
async fn ai_edit(state: S<'_>, app: AppHandle, clip_id: String, prompt: String) -> R<Value> {
    let mut clip = state.get_clip(&clip_id).map_err(e)?;
    busy(&clip)?;
    let task = state.get_task(&clip.task_id).map_err(e)?;
    let settings = state.settings();
    let transcript = transcribe::load_cached(&state, &task.id).ok_or("The transcript for this video is missing, so the AI cannot edit it.")?;
    let fonts: Vec<String> = pipeline::fonts(&state).into_iter().map(|f| f.name).collect();
    let (doc, summary, notes) = ai_edit::run(&state, &settings, &clip, &transcript, task.duration, &fonts, &prompt).await.map_err(e)?;
    clip.doc = doc;
    clip.status = "rendering".into();
    state.upsert_clip(&clip).map_err(e)?;
    spawn_render(&app, &state, &clip.id);
    Ok(json!({"clip": clip, "summary": summary, "notes": notes}))
}

#[tauri::command]
fn split_clip(state: S, app: AppHandle, clip_id: String, at: f64) -> R<Vec<Clip>> {
    let mut clip = state.get_clip(&clip_id).map_err(e)?;
    busy(&clip)?;
    let (first, second) = editor::split_doc(&clip.doc, at).map_err(e)?;
    clip.doc = first;
    clip.status = "rendering".into();
    state.upsert_clip(&clip).map_err(e)?;
    let other = editor::new_clip(&clip.task_id, clip.idx, second);
    state.upsert_clip(&other).map_err(e)?;
    spawn_render(&app, &state, &clip.id);
    spawn_render(&app, &state, &other.id);
    Ok(vec![clip, other])
}

#[tauri::command]
fn merge_clips(state: S, app: AppHandle, clip_ids: Vec<String>) -> R<Clip> {
    let mut clips = Vec::new();
    for id in &clip_ids {
        clips.push(state.get_clip(id).map_err(e)?);
    }
    clips.sort_by_key(|c| c.idx);
    let task_id = clips.first().map(|c| c.task_id.clone()).ok_or("Select at least two clips to merge.")?;
    if clips.iter().any(|c| c.task_id != task_id) {
        return Err("Clips from different videos cannot be merged.".into());
    }
    let docs: Vec<ClipDoc> = clips.iter().map(|c| c.doc.clone()).collect();
    let merged = editor::merge_docs(&docs).map_err(e)?;
    let clip = editor::new_clip(&task_id, state.next_clip_idx(&task_id), merged);
    state.upsert_clip(&clip).map_err(e)?;
    spawn_render(&app, &state, &clip.id);
    Ok(clip)
}

/// Render a planned candidate clip now.
#[tauri::command]
fn render_candidate(state: S, app: AppHandle, clip_id: String) -> R<Clip> {
    let mut clip = state.get_clip(&clip_id).map_err(e)?;
    busy(&clip)?;
    if clip.status == "ready" && Path::new(&clip.file_path).is_file() {
        return Ok(clip);
    }
    clip.status = "rendering".into();
    clip.error.clear();
    state.upsert_clip(&clip).map_err(e)?;
    spawn_render(&app, &state, &clip.id);
    Ok(clip)
}

#[tauri::command]
fn get_kit(state: S, clip_id: String) -> R<Value> {
    Ok(kit::cached(&state, &clip_id).map(|k| json!({"kit": kit::to_json(&k)})).unwrap_or(Value::Null))
}

#[tauri::command]
async fn make_kit(state: S<'_>, clip_id: String) -> R<Value> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    let task = state.get_task(&clip.task_id).map_err(e)?;
    let k = kit::generate(&state, &state.settings(), &clip, &task).await.map_err(e)?;
    Ok(kit::to_json(&k))
}

#[tauri::command]
async fn save_kit(state: S<'_>, clip_id: String, dest: String) -> R<()> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    let k = match kit::cached(&state, &clip_id) {
        Some(k) => k,
        None => {
            let task = state.get_task(&clip.task_id).map_err(e)?;
            kit::generate(&state, &state.settings(), &clip, &task).await.map_err(e)?
        }
    };
    kit::write_zip(&state, &clip, &k, Path::new(&dest)).await.map_err(e)
}

#[tauri::command]
fn delete_clip(state: S, clip_id: String) -> R<()> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    busy(&clip)?;
    let _ = std::fs::remove_file(&clip.file_path);
    let _ = std::fs::remove_file(&clip.thumb_path);
    state.delete_clip(&clip_id).map_err(e)
}

#[tauri::command]
fn clip_words(state: S, clip_id: String) -> R<Value> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    let transcript = transcribe::load_cached(&state, &clip.task_id).unwrap_or_default();
    let cw = captions::clip_words(&transcript, &clip.doc, true);
    let plain = ClipDoc { word_edits: Default::default(), ..clip.doc.clone() };
    let originals = captions::clip_words(&transcript, &plain, true);
    let words: Vec<Value> = cw.words.iter().zip(cw.keys.iter()).zip(originals.words.iter())
        .map(|((w, k), o)| json!({"key": k, "text": w.text, "original": o.text, "start": w.start, "end": w.end}))
        .collect();
    Ok(json!({"words": words, "duration": clip.doc.duration()}))
}

// ---------- exports ----------

#[tauri::command]
async fn save_clip(state: S<'_>, clip_id: String, dest: String) -> R<()> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    tokio::fs::copy(&clip.file_path, &dest).await.map_err(e)?;
    Ok(())
}

#[tauri::command]
async fn export_clip(state: S<'_>, clip_id: String, preset: String, dest: String) -> R<()> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    editor::export_preset(&state, &clip, &preset, Path::new(&dest)).await.map_err(e)
}

#[tauri::command]
async fn export_zip(state: S<'_>, task_id: String, dest: String) -> R<usize> {
    let clips = state.list_clips(&task_id).map_err(e)?;
    tokio::task::spawn_blocking(move || editor::export_zip(&clips, Path::new(&dest))).await.map_err(e)?.map_err(e)
}

#[tauri::command]
fn suggested_name(state: S, clip_id: String) -> R<String> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    Ok(editor::clip_file_name(&clip, clip.idx as usize + 1))
}

#[tauri::command]
fn read_text_file(path: String) -> R<String> {
    let meta = std::fs::metadata(&path).map_err(e)?;
    if meta.len() > 1_000_000 {
        return Err("That file is too large to be a settings file.".into());
    }
    std::fs::read_to_string(&path).map_err(e)
}

// ---------- storage & diagnostics ----------

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.is_file() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| dir_size(&e.path())).sum()
}

#[tauri::command]
async fn storage_info(state: S<'_>) -> R<Value> {
    let d = state.data_dir.clone();
    let free = ffmpeg::free_space(&d);
    tokio::task::spawn_blocking(move || {
        json!({
            "sources": dir_size(&d.join("sources")), "clips": dir_size(&d.join("clips")),
            "cache": dir_size(&d.join("cache")) + dir_size(&d.join("work")),
            "tools": dir_size(&d.join("tools")) + dir_size(&d.join("models")),
            "free": if free == u64::MAX { Value::Null } else { json!(free) },
        })
    }).await.map_err(e)
}

/// Remove temporary files and anything that belongs to a task that no longer exists.
#[tauri::command]
async fn clear_leftovers(state: S<'_>) -> R<u64> {
    let st = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let ids: std::collections::HashSet<String> = st.list_tasks().unwrap_or_default().into_iter().map(|t| t.id).collect();
        let running = st.cancels.lock().unwrap().len() > 0;
        let mut freed = 0u64;
        let mut remove = |p: PathBuf| {
            freed += dir_size(&p);
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        };
        if !running {
            for sub in ["work", "cache"] {
                for entry in std::fs::read_dir(st.data_dir.join(sub)).into_iter().flatten().flatten() {
                    remove(entry.path());
                }
            }
        }
        for sub in ["sources", "clips"] {
            for entry in std::fs::read_dir(st.data_dir.join(sub)).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !ids.iter().any(|id| name.starts_with(id.as_str())) {
                    remove(entry.path());
                }
            }
        }
        for sub in ["work", "cache/broll"] {
            let _ = std::fs::create_dir_all(st.data_dir.join(sub));
        }
        freed
    }).await.map_err(e)
}

#[tauri::command]
fn open_folder(state: S, which: String) -> R<()> {
    let path = match which.as_str() {
        "logs" => applog::dir().unwrap_or_else(|| state.data_dir.clone()),
        "knowledge" => {
            // Your own GTA notes: created with a short guide the first time.
            let dir = state.data_dir.join("knowledge");
            let _ = std::fs::create_dir_all(&dir);
            let file = dir.join("gta6.txt");
            if !file.exists() {
                let _ = std::fs::write(&file, "Add your own notes about GTA VI here. The AI reads them together with the built-in facts.
One fact per line. Say when something is a rumour.

A line that starts with Terms: adds words for speech recognition, separated by commas. Example:
Terms: Lucia, Jason, Vice City Beach
");
            }
            return std::process::Command::new("explorer").arg(&file).spawn().map(|_| ()).map_err(e);
        }
        _ => state.data_dir.clone(),
    };
    let _ = std::fs::create_dir_all(&path);
    std::process::Command::new("explorer").arg(&path).spawn().map(|_| ()).map_err(e)
}

#[tauri::command]
fn log_client_error(message: String) {
    applog::log(&format!("UI: {}", message.chars().take(400).collect::<String>()));
}

// ---------- publishing ----------

#[tauri::command]
fn publish_status(state: S) -> Value {
    json!({"status": publish::status(&state), "history": publish::history(&state)})
}
#[tauri::command]
fn publish_history(state: S) -> Vec<publish::Receipt> {
    publish::history(&state)
}
#[tauri::command]
fn publish_clear_history(state: S) -> R<()> {
    state.kv_set("publish_history", "[]").map_err(e)
}
#[tauri::command]
fn publish_disconnect(state: S, provider: String) -> R<()> {
    publish::disconnect(&state, &provider).map_err(e)
}
#[tauri::command]
fn youtube_configure(state: S, document: String) -> R<()> {
    publish::youtube_configure(&state, &document).map_err(e)
}
#[tauri::command]
async fn youtube_login(state: S<'_>, app: AppHandle) -> R<()> {
    publish::youtube_login(app, Arc::clone(&state)).await.map_err(e)
}
#[tauri::command]
async fn facebook_connect(state: S<'_>, app_id: String, app_secret: String, token: String) -> R<()> {
    publish::facebook_connect(&state, &app_id, &app_secret, &token).await.map_err(e)
}
#[tauri::command]
fn facebook_select(state: S, id: String) -> R<()> {
    publish::facebook_select(&state, &id).map_err(e)
}
#[tauri::command]
fn tiktok_configure(state: S, key: String, secret: String) -> R<()> {
    publish::tiktok_configure(&state, &key, &secret).map_err(e)
}
#[tauri::command]
async fn tiktok_login(state: S<'_>, app: AppHandle) -> R<()> {
    publish::tiktok_login(app, Arc::clone(&state)).await.map_err(e)
}
#[tauri::command]
fn publish_clip(state: S, app: AppHandle, clip_id: String, platforms: Vec<String>, title: String, description: String, privacy: String, made_for_kids: bool) -> R<Vec<publish::Receipt>> {
    let clip = state.get_clip(&clip_id).map_err(e)?;
    publish::start(app, Arc::clone(&state), publish::Request { clip, platforms, title, description, privacy, made_for_kids }).map_err(e)
}

fn seed_fonts(res: &Path, data: &Path) {
    let Ok(entries) = std::fs::read_dir(res.join("fonts")) else { return };
    for entry in entries.flatten() {
        let dest = data.join("fonts").join(entry.file_name());
        if !dest.exists() {
            let _ = std::fs::copy(entry.path(), dest);
        }
    }
}

fn main() {
    applog::install_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }
                let Some(st) = window.app_handle().try_state::<state::State>() else { return };
                if tray::hide_on_close(&st.settings()) {
                    api.prevent_close();
                    let _ = window.hide();
                    if st.kv_get("tray_hint_shown").is_none() {
                        let _ = st.kv_set("tray_hint_shown", "1");
                        use tauri_plugin_notification::NotificationExt;
                        let _ = window.app_handle().notification().builder().title("Nexora Clip is still running").body("It stays in the tray so clips keep rendering. Click the tray icon to open it, or choose Quit.").show();
                    }
                }
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data = app.path().app_local_data_dir()?;
            let res = app.path().resource_dir()?;
            let st = AppState::open(data.clone(), res.clone())?;
            applog::init(&data);
            applog::log("Nexora Clip started");
            // Keys saved by an older version were plain text: store them encrypted now.
            let _ = st.save_settings(&st.settings());
            if let Some(v) = st.kv_get("publish_vault") {
                let _ = st.kv_set("publish_vault", &v);
            }
            seed_fonts(&res, &data);
            let settings = st.settings();
            app.manage(Arc::new(st));
            tray::setup(app.handle())?;
            remote::reconfigure(app.handle());
            tray::apply(app.handle(), &settings);
            // Started by Windows: stay out of the way in the tray.
            if settings.run_in_background && std::env::args().any(|a| a == "--minimized") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            storage_info, clear_leftovers, open_folder, log_client_error, get_settings, save_settings, groq_status, test_groq, outro_preview, outro_set_logo, probe_url, tools_status, tool_updates, install_tool, app_info, test_llm, remote_status, remote_reset_pairing, remote_tailscale_setup, remote_forget_tailscale, chatgpt_status, chatgpt_login, chatgpt_disconnect, chatgpt_models, chatgpt_set_model, list_ollama, ai_usage, ai_reset_rest, list_templates, list_fonts, add_font,
            create_task, list_tasks, get_task, cancel_task, resume_task, delete_task,
            update_clip, ai_edit, split_clip, merge_clips, delete_clip, clip_words, render_candidate, get_kit, make_kit, save_kit,
            save_clip, export_clip, export_zip, suggested_name,
            read_text_file, publish_status, publish_history, publish_clear_history, publish_disconnect, youtube_configure, youtube_login,
            facebook_connect, facebook_select, tiktok_configure, tiktok_login, publish_clip
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nexora Clip");
}
