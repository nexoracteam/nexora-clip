//! Task pipeline (replaces the ARQ worker): download → transcribe → AI analysis → render,
//! with live progress events instead of Redis pub/sub + SSE.

use crate::captions::{self, FontInfo};
use crate::ffmpeg::{self, Probe};
use crate::models::*;
use crate::render::{self, RenderCtx};
use crate::state::{now, State};
use crate::{ai, broll, download, transcribe};
use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

pub fn emit_task(app: &AppHandle, state: &State, id: &str) {
    if let Ok(t) = state.get_task(id) {
        let _ = app.emit("task-update", &t);
    }
}

pub fn emit_clip(app: &AppHandle, state: &State, clip_id: &str) {
    if let Ok(c) = state.get_clip(clip_id) {
        let _ = app.emit("clip-update", &c);
    }
}

/// Progress reporter that only touches the DB / UI when something changed.
struct Reporter {
    app: AppHandle,
    state: State,
    id: String,
    last: Mutex<(i64, String)>,
}

impl Reporter {
    fn set(&self, pct: f64, message: &str) {
        let pct = pct.round().clamp(0.0, 100.0) as i64;
        let mut last = self.last.lock().unwrap();
        if last.0 == pct && last.1 == message {
            return;
        }
        *last = (pct, message.to_string());
        let _ = self.state.set_task_fields(&self.id, &[("progress", &pct), ("message", &message)]);
        let _ = self.app.emit("task-progress", serde_json::json!({"id": self.id, "progress": pct, "message": message}));
    }
}

pub fn fonts(state: &State) -> Vec<FontInfo> {
    captions::list_fonts(&state.data_dir.join("fonts"))
}

fn clip_file(state: &State, task_id: &str, clip_id: &str) -> PathBuf {
    let stamp = chrono::Utc::now().timestamp_millis();
    state.data_dir.join("clips").join(task_id).join(format!("{clip_id}_{stamp}.mp4"))
}

struct Shared<'a> {
    app: &'a AppHandle,
    state: &'a State,
    settings: &'a Settings,
    encoder: &'a str,
    source: &'a Path,
    probe: &'a Probe,
    transcript: &'a Transcript,
    fonts: &'a [FontInfo],
}

/// Render one clip row and store the result. Never returns Err for a render
/// failure: the clip row carries its own error so the rest of the task goes on.
async fn render_and_store(sh: &Shared<'_>, mut clip: Clip, want_broll: bool, cancel: &AtomicBool, on: impl FnMut(f64)) -> bool {
    clip.status = "rendering".into();
    clip.error.clear();
    let _ = sh.state.upsert_clip(&clip);
    emit_clip(sh.app, sh.state, &clip.id);

    if want_broll && clip.doc.broll.is_empty() {
        let cw = captions::clip_words(sh.transcript, &clip.doc, false);
        clip.doc.broll = broll::plan(sh.state, sh.settings, &cw.words, clip.doc.duration()).await;
    }
    let out = clip_file(sh.state, &clip.task_id, &clip.id);
    let ctx = RenderCtx { state: sh.state, settings: sh.settings, encoder: sh.encoder, source: sh.source, probe: sh.probe, transcript: sh.transcript, fonts: sh.fonts };
    let old_file = clip.file_path.clone();
    let result = render::render_clip(&ctx, &clip.id, &mut clip.doc, &out, cancel, on).await;
    let ok = match result {
        Ok(duration) => {
            let thumb = out.with_extension("jpg");
            let _ = ffmpeg::thumbnail(sh.state, &out, &thumb, (duration * 0.25).min(2.0)).await;
            if !old_file.is_empty() && old_file != out.to_string_lossy() {
                let _ = std::fs::remove_file(&old_file);
                let _ = std::fs::remove_file(Path::new(&old_file).with_extension("jpg"));
            }
            clip.file_path = out.to_string_lossy().into();
            clip.thumb_path = if thumb.is_file() { thumb.to_string_lossy().into() } else { String::new() };
            clip.duration = duration;
            clip.status = "ready".into();
            true
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            clip.status = "error".into();
            clip.error = e.to_string();
            false
        }
    };
    let _ = sh.state.upsert_clip(&clip);
    emit_clip(sh.app, sh.state, &clip.id);
    ok
}

async fn run_inner(app: &AppHandle, state: &State, task_id: &str, cancel: &AtomicBool) -> Result<()> {
    let rep = Reporter { app: app.clone(), state: state.clone(), id: task_id.to_string(), last: Mutex::new((-1, String::new())) };
    let check = || if cancel.load(Ordering::Relaxed) { Err(anyhow!("cancelled")) } else { Ok(()) };
    let _permit = state.task_sem.clone().acquire_owned().await?;
    check()?;
    state.set_task_fields(task_id, &[("status", &"processing"), ("error", &"")])?;
    emit_task(app, state, task_id);
    let mut settings = state.settings();
    let task = state.get_task(task_id)?;

    // 1. source
    let mut source = PathBuf::from(&task.source_path);
    if task.source_path.is_empty() || !source.is_file() {
        if task.source_url.is_empty() {
            bail!("The source video file is missing: {}", task.source_path);
        }
        rep.set(1.0, "Setting up the downloader (first time only)");
        crate::tools::ensure_downloader(state, app).await?;
        rep.set(2.0, "Starting download");
        let section = match (task.options.section_start, task.options.section_end) {
            (None, None) => None,
            (a, b) => Some((a.unwrap_or(0.0), b.unwrap_or(f64::MAX))),
        };
        let d = download::download(state, &settings, &task.source_url, task_id, &task.options.download_quality, section, cancel, app, |f, m| rep.set(2.0 + f * 16.0, m)).await?;
        source = d.path;
        let title = if d.title.is_empty() { task.title.clone() } else { d.title };
        state.set_task_fields(task_id, &[("source_path", &source.to_string_lossy().to_string()), ("title", &title)])?;
        emit_task(app, state, task_id);
    }
    check()?;

    // 2. probe
    rep.set(19.0, "Reading video");
    let probe = ffmpeg::probe(state, &source).await?;
    if !probe.has_video {
        bail!("This file has no video track.");
    }
    if !probe.has_audio {
        bail!("This video has no audio track, so there is nothing to transcribe.");
    }
    state.set_task_fields(task_id, &[("duration", &probe.duration)])?;

    // Subtitles already burned into the picture would show twice: find them once per video.
    let vision_key = crate::gemini::key(&settings);
    let sub_crop: Option<f64> = match (&vision_key, state.kv_get(&format!("subband:{task_id}"))) {
        (_, Some(v)) => v.parse::<f64>().ok().filter(|k| *k > 0.0),
        (Some(key), None) => {
            rep.set(19.5, "Checking for subtitles burned into the picture");
            let k = crate::vision::subtitle_keep(state, &settings, key, &source, &probe).await;
            let _ = state.kv_set(&format!("subband:{task_id}"), &k.unwrap_or(0.0).to_string());
            k
        }
        _ => None,
    };

    // Game knowledge: if the video is about a game we know, the speech-to-text is told its names.
    let title_now = state.get_task(task_id).map(|t| t.title).unwrap_or_default();
    if let Some(pack) = crate::knowledge::pack_for(&settings, &title_now, &[]) {
        settings.vocab = crate::knowledge::terms(pack, &state.data_dir);
    }

    // 3. transcript: the video's own subtitles when it has good ones, otherwise transcribe
    let no_section = task.options.section_start.is_none() && task.options.section_end.is_none();
    if settings.yt_subtitles && task.source_type == "url" && no_section && transcribe::load_cached(state, task_id).is_none() {
        rep.set(20.0, "Looking for the video's own subtitles");
        if crate::subs::from_download(state, &settings, task_id, probe.duration).await.is_some() {
            rep.set(50.0, "Using the video's subtitles (no transcription needed)");
        }
    }
    let mut transcript = transcribe::transcribe(state, &settings, &source, task_id, probe.duration, cancel, |f, m| rep.set(20.0 + f * 30.0, m)).await?;
    check()?;

    // Put names right and show Hindi / Urdu in Roman letters (the saved transcript is updated, so every later render agrees).
    let pack = crate::knowledge::pack_for(&settings, &title_now, &transcript.words);
    let mut changed = 0;
    if let Some(p) = pack {
        changed += crate::knowledge::fix_words(p, &mut transcript.words);
    }
    if settings.roman_captions {
        for w in transcript.words.iter_mut() {
            if crate::roman::needs_roman(&w.text) {
                w.text = crate::roman::to_roman(&w.text);
                changed += 1;
            }
        }
    }
    if changed > 0 {
        let _ = transcribe::save(state, task_id, &transcript);
    }
    // The AI gets the game's facts together with the creator's own instructions.
    let mut ai_opts = task.options.clone();
    if let Some(p) = pack {
        let block = crate::knowledge::block(p, &state.data_dir);
        ai_opts.instructions = if ai_opts.instructions.trim().is_empty() { block } else { format!("{}

{}", ai_opts.instructions.trim(), block) };
    }

    // 4. analysis (kept on the task so a resume never pays for it twice)
    let analysis = match state.get_analysis(task_id) {
        Some(a) if !a.segments.is_empty() => a,
        _ => {
            let low_speech = crate::visual::is_low_speech(transcript.words.len(), probe.duration);
            let a = if low_speech && task.workflow == "highlights" {
                let mut seen: Option<Analysis> = None;
                if let Some(key) = &vision_key {
                    rep.set(50.0, "Little speech here: Gemini is watching the video");
                    match crate::vision::analyze(state, &settings, key, &source, &probe, &task.title, &ai_opts, cancel, |f, m| rep.set(50.0 + f * 12.0, m)).await {
                        Ok(a) => seen = Some(a),
                        Err(e) if e.to_string() == "cancelled" => bail!("cancelled"),
                        Err(e) => crate::applog::log(&format!("gemini vision failed, using scene analysis: {e}")),
                    }
                }
                if let Some(a) = seen { a } else {
                rep.set(50.0, "Little speech here: picking moments by action and sound");
                crate::visual::analyze(state, &settings, &source, probe.duration, &task.title, &ai_opts, cancel, |f, m| rep.set(50.0 + f * 12.0, m)).await?
                }
            } else if transcript.words.is_empty() {
                bail!("No speech was found in this video. This workflow needs speech: use Highlights, which can also pick clips from action and sound.");
            } else {
                match ai::analyze(state, &settings, &transcript, &task.workflow, &ai_opts, probe.duration, cancel, |f, m| rep.set(50.0 + f * 12.0, m)).await {
                    Ok(a) => a,
                    Err(e) if task.workflow == "highlights" && (e.to_string().starts_with("[ai_busy]") || e.to_string().starts_with("[ai_setup]")) => {
                        // No AI could answer: still make clips, chosen by sound and cuts, titled from the speech.
                        crate::applog::log(&format!("AI unavailable, picking by sound: {e}"));
                        rep.set(52.0, "The AI is unavailable: picking clips by sound and speech instead");
                        let mut a = crate::visual::analyze(state, &settings, &source, probe.duration, &task.title, &ai_opts, cancel, |f, m| rep.set(52.0 + f * 10.0, m)).await?;
                        offline_fill(&mut a, &transcript);
                        a
                    }
                    Err(e) => return Err(e),
                }
            };
            state.set_task_fields(task_id, &[
                ("analysis", &serde_json::to_string(&a)?),
                ("summary", &a.summary),
                ("key_topics", &serde_json::to_string(&a.key_topics)?),
            ])?;
            a
        }
    };
    check()?;

    // 5. clip rows
    let mut clips = state.list_clips(task_id)?;
    if clips.is_empty() {
        // Clips with hardly any speech get their hook from looking at the picture, not from a thin transcript.
        let mut hooks: Vec<Option<String>> = vec![None; analysis.segments.len()];
        if let (Some(key), "highlights") = (&vision_key, task.workflow.as_str()) {
            let todo: Vec<usize> = (0..analysis.segments.len()).filter(|&i| !analysis.segments[i].grounded && crate::vision::speech_density(&transcript.words, &analysis.segments[i].ranges) < 1.0).collect();
            if !todo.is_empty() {
                rep.set(61.0, "Looking at the clips to write their hooks");
                let found: Vec<(usize, Option<String>)> = futures_util::stream::iter(todo.into_iter().map(|i| {
                    let title_with_facts = match pack { Some(p) => format!("{}

{}", task.title, crate::knowledge::block(p, &state.data_dir)), None => task.title.clone() };
                    let (settings, source, title, key) = (&settings, &source, title_with_facts, key.clone());
                    let seg = &analysis.segments[i];
                    let said = captions::clip_words(&transcript, &ClipDoc { ranges: seg.ranges.clone(), ..Default::default() }, false).words.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" ");
                    async move { (i, crate::vision::ground_hook(state, settings, &key, source, &seg.ranges, &said, seg.hook_title.as_deref().unwrap_or(""), &title).await) }
                })).buffer_unordered(3).collect().await;
                for (i, h) in found { hooks[i] = h; }
            }
        }
        // With "render top N" only the best clips are rendered now; the rest wait as candidates.
        let mut ranked: Vec<usize> = (0..analysis.segments.len()).collect();
        ranked.sort_by_key(|&i| (std::cmp::Reverse(analysis.segments[i].virality.total_score), i));
        let top: std::collections::HashSet<usize> = if task.options.render_top == 0 { ranked.iter().copied().collect() } else { ranked.iter().copied().take(task.options.render_top as usize).collect() };
        for (i, seg) in analysis.segments.iter().enumerate() {
            let o = &task.options;
            let clip = Clip {
                id: uuid::Uuid::new_v4().to_string(),
                task_id: task_id.to_string(),
                idx: i as i64,
                doc: ClipDoc {
                    hook_title: hooks[i].clone().or_else(|| seg.hook_title.clone()),
                    sub_crop,
                    text: seg.text.clone(),
                    reasoning: seg.reasoning.clone(),
                    ranges: seg.ranges.clone(),
                    virality: seg.virality.clone(),
                    relevance: seg.relevance,
                    role: seg.role.clone(),
                    part_number: seg.part_number,
                    template: o.template.clone(),
                    font_family: o.font_family.clone(),
                    font_size: o.font_size,
                    font_color: o.font_color.clone(),
                    captions: o.captions,
                    show_hook: o.hook_titles,
                    transition: o.transition.clone(),
                    emoji: o.emoji,
                    layout: o.layout.clone(),
                    ..Default::default()
                },
                status: if top.contains(&i) { "queued".into() } else { "candidate".into() },
                created_at: now(),
                ..Default::default()
            };
            state.upsert_clip(&clip)?;
            clips.push(clip);
        }
        emit_task(app, state, task_id);
    }

    // 6. render what is not ready yet
    let pending: Vec<Clip> = clips.into_iter().filter(|c| c.status != "candidate" && (c.status != "ready" || !Path::new(&c.file_path).is_file())).collect();
    let total = pending.len();
    if total > 0 {
        rep.set(62.0, "Rendering clips");
        let encoder = ffmpeg::resolve_encoder(state, &settings).await;
        let font_list = fonts(state);
        let sh = Shared { app, state, settings: &settings, encoder: &encoder, source: &source, probe: &probe, transcript: &transcript, fonts: &font_list };
        let fractions = Arc::new(Mutex::new(vec![0.0f64; total]));
        let parallel = settings.parallel_renders.clamp(1, 4) as usize;
        let want_broll = task.options.broll;
        let (sh_ref, rep_ref) = (&sh, &rep);
        let results: Vec<bool> = futures_util::stream::iter(pending.into_iter().enumerate().map(|(i, clip)| {
            let fractions = fractions.clone();
            async move {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                let f2 = fractions.clone();
                let ok = render_and_store(sh_ref, clip, want_broll, cancel, move |f| {
                    let mut all = f2.lock().unwrap();
                    all[i] = f;
                    let avg = all.iter().sum::<f64>() / total as f64;
                    let finished = all.iter().filter(|x| **x >= 0.999).count();
                    drop(all);
                    rep_ref.set(62.0 + avg * 37.0, &format!("Rendering clips ({finished}/{total} done)"));
                }).await;
                fractions.lock().unwrap()[i] = 1.0;
                ok
            }
        })).buffer_unordered(parallel).collect().await;
        check()?;
        if !results.iter().any(|ok| *ok) && state.list_clips(task_id)?.iter().all(|c| c.status != "ready") {
            let first = state.list_clips(task_id)?.into_iter().find(|c| !c.error.is_empty()).map(|c| c.error).unwrap_or_default();
            bail!("No clip could be rendered. {first}");
        }
    }
    rep.set(100.0, "Done");
    Ok(())
}

/// Without an AI: give each sound-picked clip its spoken words and a hook made from the first of them.
fn offline_fill(a: &mut Analysis, t: &Transcript) {
    for seg in a.segments.iter_mut() {
        let words: Vec<&str> = t.words.iter().filter(|w| seg.ranges.iter().any(|r| w.start >= r[0] && w.start < r[1])).map(|w| w.text.as_str()).collect();
        if words.is_empty() {
            continue;
        }
        seg.text = words.join(" ");
        let hook: String = words.iter().take(7).cloned().collect::<Vec<_>>().join(" ");
        if let Some(h) = ai::sanitize_hook_title(Some(hook.trim_end_matches(['.', ',', '!', '?']))) {
            seg.hook_title = Some(h);
        }
    }
    a.summary = format!("Picked by sound and speech, because the AI was not available. {}", a.summary);
}

pub async fn run_task(app: AppHandle, state: State, task_id: String) {
    let cancel = {
        let mut map = state.cancels.lock().unwrap();
        map.entry(task_id.clone()).or_insert_with(|| Arc::new(AtomicBool::new(false))).clone()
    };
    let result = run_inner(&app, &state, &task_id, &cancel).await;
    let _ = match result {
        Ok(()) => state.set_task_fields(&task_id, &[("status", &"completed"), ("progress", &100i64), ("message", &"Done"), ("error", &"")]),
        Err(e) if cancel.load(Ordering::Relaxed) || e.to_string() == "cancelled" => {
            // Clips caught mid-render go back to "queued" so Resume picks them up.
            if let Ok(clips) = state.list_clips(&task_id) {
                for mut c in clips.into_iter().filter(|c| c.status == "rendering" || c.status == "error") {
                    c.status = "queued".into();
                    c.error.clear();
                    let _ = state.upsert_clip(&c);
                }
            }
            state.set_task_fields(&task_id, &[("status", &"cancelled"), ("message", &"Cancelled")])
        }
        Err(e) => state.set_task_fields(&task_id, &[("status", &"error"), ("error", &e.to_string()), ("message", &"Failed")]),
    };
    state.cancels.lock().unwrap().remove(&task_id);
    emit_task(&app, &state, &task_id);
    if let Ok(t) = state.get_task(&task_id) {
        crate::applog::log(&format!("task {} finished: {} {}", &task_id[..8.min(task_id.len())], t.status, t.error));
        if t.status == "completed" && state.settings().notify_done {
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title("Nexora Clip").body(format!("{} clip{} ready: {}", t.clip_count, if t.clip_count == 1 { "" } else { "s" }, t.title)).show();
        }
    }
}

/// Re-render a single clip after an edit.
pub async fn render_clip_job(app: AppHandle, state: State, clip_id: String) {
    let run = async {
        let clip = state.get_clip(&clip_id)?;
        let task = state.get_task(&clip.task_id)?;
        let source = PathBuf::from(&task.source_path);
        if !source.is_file() {
            bail!("The source video is no longer on disk, so this clip cannot be re-rendered.");
        }
        let settings = state.settings();
        let probe = ffmpeg::probe(&state, &source).await?;
        let transcript = transcribe::load_cached(&state, &task.id).unwrap_or_default();
        let encoder = ffmpeg::resolve_encoder(&state, &settings).await;
        let font_list = fonts(&state);
        let sh = Shared { app: &app, state: &state, settings: &settings, encoder: &encoder, source: &source, probe: &probe, transcript: &transcript, fonts: &font_list };
        let never = AtomicBool::new(false);
        let (app2, id2) = (app.clone(), clip_id.clone());
        let mut last = -1i64;
        render_and_store(&sh, clip, task.options.broll, &never, move |f| {
            let pct = (f * 100.0) as i64;
            if pct != last {
                last = pct;
                let _ = app2.emit("clip-progress", serde_json::json!({"id": id2, "progress": pct}));
            }
        }).await;
        Ok::<(), anyhow::Error>(())
    };
    if let Err(e) = run.await {
        if let Ok(mut clip) = state.get_clip(&clip_id) {
            clip.status = "error".into();
            clip.error = e.to_string();
            let _ = state.upsert_clip(&clip);
        }
        emit_clip(&app, &state, &clip_id);
    }
}
