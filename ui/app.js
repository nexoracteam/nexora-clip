"use strict";
const T = window.__TAURI__;
const invoke = T.core.invoke;
const fileSrc = (p) => T.core.convertFileSrc(p);
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const fmt = (s) => { s = Math.max(0, s || 0); const m = Math.floor(s / 60); return `${m}:${(s - m * 60).toFixed(1).padStart(4, "0")}`; };
const fmtDur = (s) => { s = Math.round(s || 0); const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60); return h ? `${h}h ${m}m` : `${m}m ${s % 60}s`; };

const WORKFLOWS = [
  ["highlights", "Highlights", "The most viral-worthy moments, scored and ranked."],
  ["series", "Series", "One story cut into Part 1..N; each part picks up where the last ended."],
  ["topic", "Topic", "Every strong moment about one topic you name."],
  ["recap", "Recap", "One clip that delivers the video's main points."],
  ["trailer", "Trailer", "One teaser that never gives away the payoff."],
];
// Only services with a free way to use them. A paid provider that is already saved still shows up, so nothing breaks.
const FREE_MODELS = {
  "google-gla": [
    ["gemini-3.1-flash-lite", "Gemini 3.1 Flash-Lite · best free daily limit"],
    ["gemini-2.5-flash", "Gemini 2.5 Flash"],
    ["gemini-flash-latest", "Gemini Flash (latest)"],
    ["gemini-3-flash-preview", "Gemini 3 Flash preview · about 20 a day"],
  ],
  groq: [
    ["openai/gpt-oss-20b", "GPT-OSS 20B"],
    ["openai/gpt-oss-120b", "GPT-OSS 120B"],
    ["llama-3.3-70b-versatile", "Llama 3.3 70B"],
    ["llama-3.1-8b-instant", "Llama 3.1 8B"],
  ],
  openrouter: [
    ["openai/gpt-oss-20b:free", "GPT-OSS 20B (free)"],
    ["meta-llama/llama-3.3-70b-instruct:free", "Llama 3.3 70B (free)"],
  ],
};
const PROVIDERS = [
  ["google-gla", "Google Gemini (free tier)", "gemini-3.1-flash-lite", "google_api_key"],
  ["groq", "Groq (free tier)", "openai/gpt-oss-20b", "groq_api_key"],
  ["openrouter", "OpenRouter (free models)", "openai/gpt-oss-20b:free", "openrouter_api_key"],
  ["ollama", "Ollama (on this PC, free)", "", null],
  ["custom", "Local server (OpenAI-compatible)", "", "custom_api_key"],
];
const CHATGPT = ["chatgpt", "ChatGPT (your plan)", "", null];
const PAID_PROVIDERS = [
  ["openai", "OpenAI (paid)", "gpt-5-mini", "openai_api_key"],
  ["anthropic", "Anthropic Claude (paid)", "claude-sonnet-5-5", "anthropic_api_key"],
];


const ICONS = {
  plus: '<svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>',
  library: '<svg viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/></svg>',
  settings: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M4.2 4.2l2.1 2.1M17.7 17.7l2.1 2.1M2 12h3M19 12h3M4.2 19.8l2.1-2.1M17.7 6.3l2.1-2.1"/></svg>',
  send: '<svg viewBox="0 0 24 24"><path d="M22 2 11 13M22 2l-7 20-4-9-9-4z"/></svg>',
  sun: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>',
  moon: '<svg viewBox="0 0 24 24"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>',
};
function paintIcons() {
  $$("[data-icon]").forEach((el) => (el.innerHTML = ICONS[el.dataset.icon]));
  const dark = document.documentElement.classList.contains("dark");
  $("#theme-toggle").innerHTML = `<span class="ico">${dark ? ICONS.sun : ICONS.moon}</span>`;
}
function toggleTheme() {
  const dark = document.documentElement.classList.toggle("dark");
  try { localStorage.setItem("theme", dark ? "dark" : "light"); } catch (e) {}
  paintIcons();
}

window.addEventListener("error", (ev) => { try { invoke("log_client_error", { message: `${ev.message} @ ${ev.filename}:${ev.lineno}` }); } catch (e) {} });
window.addEventListener("unhandledrejection", (ev) => { try { invoke("log_client_error", { message: "unhandled: " + String(ev.reason) }); } catch (e) {} });
const fmtBytes = (n) => (n == null ? "?" : n > 1e9 ? (n / 1e9).toFixed(1) + " GB" : n > 1e6 ? Math.round(n / 1e6) + " MB" : Math.round(n / 1e3) + " KB");

const S = {
  view: "create", taskId: null, settings: null, templates: [], fonts: [], tools: [], info: {}, tasks: [],
  task: null, clips: [], selected: new Set(), probe: null, pub: { status: null, history: [] }, publishClip: null, openGuide: {}, toolProgress: {}, installing: false, editor: null,
  form: { url: "", path: "", workflow: "highlights", options: { layout: "gaming", clip_length: "auto", template: "gaming", font_family: null, font_size: null, font_color: null, captions: true, hook_titles: true, broll: false, instructions: "", topic: "", part_count: 3, transition: null, emoji: false, download_quality: "1080p60", section_start: null, section_end: null, render_top: 5 } },
};

const ERRORS = {
  ai_busy: ["No free AI is available right now", "Highlights still work: clips are then picked by sound and speech. To get AI picks again, add a free Groq key, start Ollama on this PC, or try tomorrow when the Gemini limit resets.", "settings"],
  ai_quota: ["The free limit is used up", "Another free model takes over by itself while this one rests.", "settings"],
  ai_key: ["The AI key was not accepted", "Open Settings and paste the key again.", "settings"],
  ai_model: ["This model is not available", "Pick another one in the list.", "settings"],
  ai_down: ["The AI did not answer", "Try again in a moment, or pick another model.", ""],
  ai_bad_answer: ["The AI answered, but not as expected", "Pick another model.", "settings"],
  ai_setup: ["The AI is not set up yet", "Choose a free model and paste its key in Settings.", "settings"],
};
function errInfo(err) {
  let raw = String((err && err.message) || err || "").trim().replace(/^Error:\s*/i, "");
  const m = raw.match(/^\[([a-z_]+)\]\s*([\s\S]*)$/);
  if (m && ERRORS[m[1]]) return { title: ERRORS[m[1]][0], body: m[2], hint: ERRORS[m[1]][1], action: ERRORS[m[1]][2], raw };
  const t = raw.toLowerCase();
  if (/ffmpeg is missing|not installed|tools/.test(t) && /install|missing/.test(t)) return { title: "A tool is missing", body: raw, hint: "Open Settings, then Tools, and press Install everything missing.", action: "settings", raw };
  if (/human check|signed in|cookies/.test(t)) return { title: "YouTube wants a signed-in browser", body: raw, hint: "", action: "settings", raw };
  if (/connection|network|getaddrinfo|timed out/.test(t)) return { title: "No internet connection", body: "Check your connection and try again.", hint: "", action: "", raw };
  if (/disk|free space/.test(t)) return { title: "Not enough disk space", body: raw, hint: "", action: "settings", raw };
  return { title: "Something went wrong", body: raw.length > 260 ? raw.slice(0, 260) + "…" : raw, hint: "", action: "", raw };
}
function errorCard(i) {
  return `<div class="errcard"><div class="errcard-ic">!</div><div class="grow"><b>${esc(i.title)}</b>${i.body ? `<p>${esc(i.body)}</p>` : ""}${i.hint ? `<p class="muted small">${esc(i.hint)}</p>` : ""}${i.action && S.view !== "settings" ? `<div class="row" style="margin-top:8px"><button class="btn sm" data-act="err-settings">Open Settings</button></div>` : ""}</div></div>`;
}
function okCard(title, body) {
  return `<div class="errcard ok"><div class="errcard-ic">✓</div><div class="grow"><b>${esc(title)}</b>${body ? `<p>${esc(body)}</p>` : ""}</div></div>`;
}
function toastError(err) {
  const i = errInfo(err);
  const el = document.createElement("div");
  el.className = "toast err";
  el.innerHTML = `<b>${esc(i.title)}</b>${i.body ? `<span>${esc(i.body.length > 180 ? i.body.slice(0, 180) + "…" : i.body)}</span>` : ""}`;
  $("#toasts").appendChild(el);
  setTimeout(() => el.remove(), 9000);
}
// An app-styled question instead of the Windows box. Resolves true on the confirm button.
function askConfirm({ title, text, label = "Delete", danger = true }) {
  return new Promise((resolve) => {
    const root = document.createElement("div");
    root.className = "modal-back confirm-back";
    root.innerHTML = `<div class="modal confirm" role="alertdialog" aria-modal="true" aria-label="${esc(title)}"><div class="confirm-body"><h2>${esc(title)}</h2><p class="muted">${esc(text || "")}</p></div>
      <div class="modal-foot"><button class="btn" data-c="no">Cancel</button><button class="btn ${danger ? "danger-fill" : "primary"}" data-c="yes">${esc(label)}</button></div></div>`;
    const done = (v) => { document.removeEventListener("keydown", key, true); root.remove(); resolve(v); };
    const key = (e) => { if (e.key === "Escape") { e.preventDefault(); done(false); } else if (e.key === "Enter") { e.preventDefault(); done(true); } };
    root.addEventListener("click", (e) => { if (e.target === root) done(false); const b = e.target.closest("[data-c]"); if (b) done(b.dataset.c === "yes"); });
    document.addEventListener("keydown", key, true);
    document.body.appendChild(root);
    root.querySelector('[data-c="no"]').focus();
  });
}

function toast(message, isError) {
  const el = document.createElement("div");
  el.className = "toast" + (isError ? " err" : "");
  el.textContent = message;
  $("#toasts").appendChild(el);
  setTimeout(() => el.remove(), isError ? 9000 : 4000);
}
async function call(cmd, args) {
  try { return await invoke(cmd, args); } catch (err) { toastError(err); throw err; }
}

// ---------- navigation ----------
function go(view, taskId) {
  S.view = view; S.taskId = taskId || null; S.selected.clear();
  $$(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.nav === (view === "task" ? "library" : view)));
  render();
}
// Tabs open at once from what is already known; fresh data is fetched behind and only
// replaces the page when it changed (and nobody is typing in it).
let renderSeq = 0;
function skeletonPage(title) {
  const row = `<div class="task-row"><div class="sk" style="width:44px;height:78px"></div><div class="grow stack" style="gap:8px"><div class="sk sk-line" style="width:55%"></div><div class="sk sk-line" style="width:30%"></div></div></div>`;
  return `<div class="page"><div class="page-head"><div><h1>${title}</h1></div></div><div class="task-list">${row}${row}${row}</div></div>`;
}
function typingInView() { const a = document.activeElement; return !!(a && $("#view").contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName)); }
async function render() {
  const v = $("#view"); v.classList.remove("enter"); void v.offsetWidth; v.classList.add("enter");
  const seq = ++renderSeq, view = S.view, tid = S.taskId;
  const live = () => seq === renderSeq && S.view === view && S.taskId === tid;
  const quiet = (fn) => { if (!live() || typingInView()) return; v.classList.remove("enter"); fn(); };
  if (view === "create") return renderCreate();
  if (view === "library") {
    S.tasksLoaded ? renderLibrary() : (v.innerHTML = skeletonPage("Library"));
    const t = await call("list_tasks");
    const changed = JSON.stringify(t) !== JSON.stringify(S.tasks) || !S.tasksLoaded;
    S.tasks = t; S.tasksLoaded = true;
    if (changed) quiet(renderLibrary);
    return;
  }
  if (view === "settings") {
    if (S.settings && S.tools.length) { renderSettings(); loadOutroPreview(); loadStorage(); loadAiUsage(); loadUpdates(false); loadOllama(); loadChatgpt(false); } else v.innerHTML = skeletonPage("Settings");
    const before = JSON.stringify([S.tools, S.groq]);
    await refreshTools();
    try { S.groq = await invoke("groq_status"); } catch (e) {}
    if (!live()) return;
    if (before !== JSON.stringify([S.tools, S.groq]) || !$("#storage-box")) { quiet(() => { renderSettings(); loadOutroPreview(); loadStorage(); loadAiUsage(); }); }
    else loadStorage();
    return;
  }
  if (view === "publish") {
    S.pub.status ? renderPublish() : (v.innerHTML = skeletonPage("Publishing"));
    const before = JSON.stringify([S.pub.status, S.pub.history]);
    await loadPublish();
    if (before !== JSON.stringify([S.pub.status, S.pub.history])) quiet(renderPublish);
    return;
  }
  if (view === "task") {
    if (S.task && S.task.id === tid) renderTask(); else v.innerHTML = skeletonPage("Loading clips…");
    const before = JSON.stringify([S.task, S.clips]);
    await loadTask();
    if (before !== JSON.stringify([S.task, S.clips])) quiet(renderTask);
    return;
  }
}
function missingTools() {
  const need = ["ffmpeg", "ffprobe"];
  if (S.settings?.transcription_provider === "whisper_local") need.push("whisper", "whisper-model");
  return S.tools.filter((t) => need.includes(t.name) && !t.installed);
}
function renderFoot() {
  const miss = missingTools();
  const enc = { libx264: "CPU", h264_nvenc: "NVIDIA GPU", h264_qsv: "Intel GPU", h264_amf: "AMD GPU" }[S.info.encoder] || S.info.encoder || "";
  $("#sidebar-foot").innerHTML = miss.length
    ? `<b><span class="dot warn"></span>Setup needed</b><div class="muted" style="margin-top:6px">${miss.map((t) => esc(t.label)).join(", ")} missing</div>`
    : `<b><span class="dot"></span>Ready</b><div class="muted" style="margin-top:6px">Rendering on ${esc(enc)}</div>`;
}

// ---------- create ----------
function tplCard(t, selected) {
  const stroke = t.stroke_color ? `-webkit-text-stroke:1px ${t.stroke_color};paint-order:stroke fill;` : "";
  const bg = t.background && t.background_color ? `background:${t.background_color};padding:2px 6px;border-radius:4px;` : "";
  const box = t.word_box ? `background:${t.word_box_color};padding:0 4px;border-radius:4px;` : "";
  const up = (w) => (t.uppercase ? w.toUpperCase() : w);
  return `<button class="tpl ${selected ? "on" : ""}" data-tpl="${t.id}" title="${esc(t.description)}">
    <div class="tpl-sample" style="${bg}"><span style="color:${t.font_color};${stroke}">${up("Make")}</span><span style="color:${t.highlight_color};${stroke}${box}">${up("clips")}</span></div>
    <div class="tpl-name">${esc(t.name)}</div></button>`;
}
function fontOptions(current) {
  return `<option value="">Template default</option>` + S.fonts.map((f) => `<option value="${esc(f.name)}" ${current === f.name ? "selected" : ""}>${esc(f.family)}</option>`).join("");
}
function renderCreate() {
  const f = S.form, o = f.options, miss = missingTools();
  const wf = WORKFLOWS.find((w) => w[0] === f.workflow);
  $("#view").innerHTML = `<div class="page stack">
    <div><h1>What are we clipping today?</h1><p class="muted">Paste a YouTube, Twitch or Kick link, or pick a file from this PC. Everything runs on your machine.</p></div>
    ${miss.length ? `<div class="alert"><div><b>Finish setup:</b> ${miss.map((t) => esc(t.label)).join(", ")} ${miss.length > 1 ? "are" : "is"} not installed yet.</div><button class="btn primary" data-act="goto-settings">Open setup</button></div>` : ""}
    <div class="card stack">
      <div class="source-box"><input type="text" id="f-url" placeholder="https://www.youtube.com/watch?v=…" value="${esc(f.url)}" ${f.path ? "disabled" : ""} />
        <button class="btn big brand" data-act="create" id="create-btn">Generate clips</button></div>
      <div id="probe-card">${probeHtml()}</div>
      <div class="drop ${f.path ? "has-file" : ""}"><span>${f.path ? "📁 " + esc(f.path) : "…or drop a video file here"}</span>
        <span class="row">${f.path ? `<button class="btn sm" data-act="clear-file">Remove</button>` : ""}<button class="btn sm" data-act="pick-file">Choose file</button></span></div>
    </div>
    <div class="card stack">
      <h2>What to make</h2>
      <div class="seg">${WORKFLOWS.map((w) => `<button data-wf="${w[0]}" class="${f.workflow === w[0] ? "on" : ""}">${w[1]}</button>`).join("")}</div>
      <p class="muted small">${wf[2]}</p>
      ${f.workflow === "topic" ? `<label class="field">Topic<input type="text" id="f-topic" placeholder="e.g. fundraising" value="${esc(o.topic)}" /></label>` : ""}
      ${f.workflow === "series" ? `<label class="field" style="max-width:160px">Parts<input type="number" id="f-parts" min="2" max="8" value="${o.part_count}" /></label>` : ""}
      <div class="grid-2">
        <div class="stack"><span class="muted small">Clip length</span>
          <div class="seg">${[["auto", "Auto 15–60s"], ["short", "Short 15–30s"], ["medium", "Medium 30–60s"], ["long", "Long 60–120s"]].map((l) => `<button data-len="${l[0]}" class="${o.clip_length === l[0] ? "on" : ""}">${l[1]}</button>`).join("")}</div></div>
        <label class="field">Instructions for the AI (optional)<textarea id="f-instr" placeholder="e.g. focus on the pricing discussion, skip the intro">${esc(o.instructions)}</textarea></label>
      </div>
    </div>
    <div class="card stack">
      <h2>Layout</h2>
      <div class="layouts">${[["gaming", "Gaming split", "Gameplay on top, the streamer's face below, big captions in between. Uses Cinematic when no facecam is found."], ["cinema", "Cinematic (no facecam)", "For trailers and plain gameplay: the whole picture in a band with a blurred backdrop, title on top, big captions below. Black bars are trimmed."], ["auto", "Follow the speaker", "One vertical crop that tracks the main face."], ["center", "Centre crop", "Plain centre crop, no tracking."]].map((l) => `<button class="layout-opt ${o.layout === l[0] ? "on" : ""}" data-layout="${l[0]}"><span class="layout-art layout-${l[0]}"><i></i><i></i></span><b>${l[1]}</b><span class="muted small">${l[2]}</span></button>`).join("")}</div>
    </div>
    <div class="card stack">
      <h2>Captions</h2>
      <div class="templates">${S.templates.map((t) => tplCard(t, o.template === t.id)).join("")}</div>
      <div class="grid-3">
        <label class="field">Font<select id="f-font">${fontOptions(o.font_family)}</select></label>
        <label class="field">Font size (12–72, blank = template)<input type="number" id="f-size" min="12" max="72" value="${o.font_size ?? ""}" /></label>
        <label class="field">Text colour<span class="row"><input type="color" id="f-color" value="${o.font_color || "#ffffff"}" /><button class="btn sm" data-act="reset-color">Template default</button></span></label>
      </div>
      <div class="row" style="gap:22px">
        <label class="field" style="min-width:210px">Cut style between parts<select id="f-transition">${[["", "None (hard cut)"], ["dip", "Dip to black"], ["flash", "Flash to white"]].map((x) => `<option value="${x[0]}" ${(o.transition || "") === x[0] ? "selected" : ""}>${x[1]}</option>`).join("")}</select></label>
      </div>
      <div class="row" style="gap:12px"><span class="muted small">Render now</span>
        <div class="seg">${[[3, "Best 3"], [5, "Best 5"], [8, "Best 8"], [0, "All"]].map((q) => `<button data-top="${q[0]}" class="${o.render_top === q[0] ? "on" : ""}">${q[1]}</button>`).join("")}</div>
        <span class="muted small">The rest stay as ranked candidates you can generate with one click.</span></div>
      <div class="row" style="gap:22px">
        <label class="check"><input type="checkbox" id="f-captions" ${o.captions ? "checked" : ""} />Word-synced captions</label>
        <label class="check"><input type="checkbox" id="f-hook" ${o.hook_titles ? "checked" : ""} />AI hook title on top</label>
        <label class="check" title="Needs a Pexels API key in Settings"><input type="checkbox" id="f-broll" ${o.broll ? "checked" : ""} />B-roll cutaways (Pexels)</label>
      </div>
    </div></div>`;
  renderFoot();
}


// ---------- link probe (video card before download) ----------
let probeTimer = null, probeSeq = 0;
function looksLikeVideoUrl(u) { return /^https?:\/\/\S+\.\S+/.test(u.trim()); }
function probeHtml() {
  const pr = S.probe, o = S.form.options;
  if (!pr) return "";
  if (pr.state === "loading") return `<div class="probe skeleton-row"><div class="sk sk-thumb"></div><div class="grow stack" style="gap:8px"><div class="sk sk-line" style="width:60%"></div><div class="sk sk-line" style="width:35%"></div></div><span class="muted small">Reading video details…</span></div>`;
  if (pr.state === "error") return `<div class="alert err"><div>${esc(pr.error)}</div></div>`;
  const d = pr.data, tallest = Math.max(0, ...d.heights);
  return `<div class="probe"><img class="probe-thumb" src="${esc(d.thumbnail)}" alt="" onerror="this.style.visibility='hidden'" />
    <div class="grow stack" style="gap:6px"><div class="clip-title">${esc(d.title)}</div>
      <div class="muted small">${esc(d.channel)}${d.channel ? " · " : ""}${fmtDur(d.duration)}${tallest ? " · up to " + tallest + "p" : ""}</div>
      <div class="row"><div class="seg">${[["1080p60", "1080p"], ["720p", "720p (faster)"]].map((q) => `<button data-q="${q[0]}" class="${o.download_quality === q[0] ? "on" : ""}">${q[1]}</button>`).join("")}</div>
        <label class="check" title="Download only part of a long video"><input type="checkbox" id="f-section" ${o.section_start !== null || o.section_end !== null ? "checked" : ""} />Only a section</label></div>
      ${o.section_start !== null || o.section_end !== null ? `<div class="row"><label class="field" style="width:130px">Start (sec)<input type="number" min="0" id="f-sec-start" value="${o.section_start ?? 0}" /></label><label class="field" style="width:130px">End (sec)<input type="number" min="1" id="f-sec-end" value="${o.section_end ?? Math.floor(d.duration)}" /></label></div>` : ""}
    </div></div>`;
}
function refreshProbe() { const el = $("#probe-card"); if (el) el.innerHTML = probeHtml(); }
function scheduleProbe() {
  clearTimeout(probeTimer);
  const url = S.form.url.trim();
  if (!looksLikeVideoUrl(url)) { S.probe = null; refreshProbe(); return; }
  S.probe = { state: "loading" }; refreshProbe();
  const seq = ++probeSeq;
  probeTimer = setTimeout(async () => {
    try { const data = await invoke("probe_url", { url }); if (seq === probeSeq) S.probe = { state: "ok", data }; }
    catch (err) { if (seq === probeSeq) S.probe = { state: "error", error: String(err) }; }
    refreshProbe();
  }, 600);
}

// ---------- library ----------
function taskRow(t) {
  const thumb = t.thumb ? `style="background-image:url('${fileSrc(t.thumb)}')"` : "";
  return `<div class="task-row" data-task="${t.id}"><div class="task-thumb" ${thumb}></div>
    <div class="grow"><div class="task-title">${esc(t.title || t.source_url)}</div>
      <div class="muted small">${esc(t.workflow)} · ${t.clip_count} clip${t.clip_count === 1 ? "" : "s"}${t.duration ? " · " + fmtDur(t.duration) : ""} · ${new Date(t.created_at).toLocaleString()}</div>
      ${["processing", "queued"].includes(t.status) ? `<div class="bar" style="margin-top:8px"><i style="width:${t.progress}%"></i></div>` : ""}</div>
    <span class="badge ${t.status}">${esc(t.status)}</span></div>`;
}
function renderLibrary() {
  $("#view").innerHTML = `<div class="page"><div class="page-head"><div><h1>Library</h1><p class="muted">Every video you have clipped.</p></div><button class="btn primary" data-nav="create">New clips</button></div>
    <div class="task-list">${S.tasks.length ? S.tasks.map(taskRow).join("") : `<div class="empty">Nothing here yet. Start with a link or a file.</div>`}</div></div>`;
}

// ---------- task ----------
async function loadTask() {
  const r = await call("get_task", { id: S.taskId });
  S.task = r.task; S.clips = r.clips;
}
function clipCard(c) {
  const d = c.doc, v = d.virality, ready = c.status === "ready" && c.file_path, cand = c.status === "candidate";
  const media = cand
    ? `<div class="clip-wait cand"><div><b>Not rendered yet</b><div class="small">Score ${v.total_score}. Generate it when you want it.</div><button class="btn sm brand" style="margin-top:10px" data-act="gen" data-id="${c.id}">Generate this clip</button></div></div>`
    : ready
    ? `<video src="${fileSrc(c.file_path)}" poster="${c.thumb_path ? fileSrc(c.thumb_path) : ""}" controls preload="none"></video>`
    : c.status === "error"
      ? `<div class="clip-wait"><div><b style="color:var(--danger)">Render failed</b><div class="small">${esc(c.error)}</div></div></div>`
      : `<div class="clip-wait"><div><div class="spin"></div><span data-clip-pct="${c.id}">${c.status === "queued" ? "Waiting" : "Rendering"}</span></div></div>`;
  const label = d.role === "series_part" ? `Part ${d.part_number}` : d.role ? d.role : `${v.total_score}`;
  return `<div class="clip ${S.selected.has(c.id) ? "sel" : ""}" data-clip="${c.id}">
    <div class="clip-media">${media}<div class="clip-score" title="Virality score">${esc(label)}</div>
      ${ready ? `<label class="clip-pick" title="Select to merge"><input type="checkbox" data-pick="${c.id}" ${S.selected.has(c.id) ? "checked" : ""} /></label>` : ""}</div>
    <div class="clip-body">
      <div class="clip-title">${esc(d.hook_title || "Untitled clip")}</div>
      <div class="muted small">${fmt(c.duration || d.ranges.reduce((a, r) => a + r[1] - r[0], 0))} · from ${d.ranges.map((r) => fmt(r[0])).join(", ")}${v.hook_type && v.hook_type !== "none" ? " · " + esc(v.hook_type) + " hook" : ""}</div>
      ${d.role ? "" : `<div class="scores"><div><b>${v.hook_score}</b>Hook</div><div><b>${v.engagement_score}</b>Engage</div><div><b>${v.value_score}</b>Value</div><div><b>${v.shareability_score}</b>Share</div></div>`}
      <div class="clip-text" title="${esc(d.reasoning)}">${esc(d.text)}</div>
      <div class="clip-actions">
        ${cand ? `<button class="btn sm brand" data-act="gen" data-id="${c.id}">Generate</button>` : `<button class="btn sm" data-act="edit" data-id="${c.id}" ${c.status === "rendering" ? "disabled" : ""}>Edit</button>`}
        ${ready ? `<button class="btn sm brand" data-act="publish" data-id="${c.id}">Publish</button><button class="btn sm" data-act="kit" data-id="${c.id}" title="AI title, description, hashtags and cover in a ZIP">Publish kit</button>` : ""}
        ${ready ? `<button class="btn sm" data-act="save" data-id="${c.id}">Save</button>
        <select class="btn sm" data-export="${c.id}" style="width:auto"><option value="">Export for…</option><option value="tiktok">TikTok</option><option value="reels">Reels</option><option value="shorts">Shorts</option></select>` : ""}
        <button class="btn sm danger" data-act="del-clip" data-id="${c.id}" ${c.status === "rendering" ? "disabled" : ""}>Delete</button>
      </div></div></div>`;
}

const STAGES = [["Download", 0], ["Transcribe", 19], ["Pick moments", 50], ["Render", 62]];
function stageIndex(p) { let i = 0; STAGES.forEach((s, k) => { if (p >= s[1]) i = k; }); return i; }
function progressCard(t) {
  const cur = stageIndex(t.progress);
  return `<div class="card progress-card" id="progress-card"><div class="steps">${STAGES.map((s, k) => `<div class="step ${k < cur ? "done" : k === cur ? "active" : ""}"><span class="step-dot">${k < cur ? "✓" : k + 1}</span>${s[0]}</div>`).join('<i class="step-line"></i>')}</div>
    <div class="row" style="justify-content:space-between;margin:14px 0 8px"><span id="t-msg">${esc(t.message)}</span><b id="t-pct" class="tabnum">${t.progress}%</b></div>
    <div class="bar big"><i id="t-bar" class="shimmer" style="width:${t.progress}%"></i></div></div>`;
}

function taskHead() {
  const t = S.task, running = ["processing", "queued"].includes(t.status);
  return `<div class="page-head"><div class="grow"><div class="row"><button class="btn sm" data-nav="library">← Library</button><span class="badge ${t.status}">${esc(t.status)}</span><span class="muted small">${esc(t.workflow)}${t.duration ? " · " + fmtDur(t.duration) : ""}</span>${S.clips.some((c) => c.status === "candidate") ? `<span class="muted small">· ${S.clips.filter((c) => c.status !== "candidate").length} rendered, ${S.clips.filter((c) => c.status === "candidate").length} candidates</span>` : ""}</div>
      <h1 style="margin-top:10px">${esc(t.title || t.source_url)}</h1></div>
    <div class="row">
      ${running ? `<button class="btn" data-act="cancel">Cancel</button>` : ""}
      ${["error", "cancelled"].includes(t.status) ? `<button class="btn primary" data-act="resume">Resume</button>` : ""}
      ${!running && S.clips.some((c) => c.status === "candidate") ? `<button class="btn" data-act="gen-all">Generate remaining (${S.clips.filter((c) => c.status === "candidate").length})</button>` : ""}
      ${S.selected.size >= 2 ? `<button class="btn" data-act="merge">Merge ${S.selected.size} clips</button>` : ""}
      ${S.clips.some((c) => c.status === "ready") ? `<button class="btn" data-act="zip">Download all (ZIP)</button>` : ""}
      <button class="btn danger" data-act="del-task">Delete</button></div></div>
    ${running ? progressCard(t) : ""}
    ${t.status === "error" ? `<div style="margin-bottom:16px">${errorCard(errInfo(t.error))}</div>` : ""}
    ${t.summary ? `<div class="card stack" style="margin-bottom:16px"><p>${esc(t.summary)}</p>${t.key_topics.length ? `<div class="chips">${t.key_topics.map((k) => `<span class="chip">${esc(k)}</span>`).join("")}</div>` : ""}</div>` : ""}`;
}
function renderTask() {
  $("#view").innerHTML = `<div class="page"><div id="task-head">${taskHead()}</div>
    <div class="clips" id="clips">${S.clips.map(clipCard).join("")}</div>
    ${!S.clips.length && S.task.status === "completed" ? `<div class="empty">No clips.</div>` : ""}</div>`;
}
function refreshHead() { const h = $("#task-head"); if (h) h.innerHTML = taskHead(); }


// ---------- publishing ----------
const PLATFORMS = {
  youtube: { name: "YouTube Shorts", color: "#ff0033" },
  facebook: { name: "Facebook Reels", color: "#1877f2" },
  tiktok: { name: "TikTok", color: "#111" },
};
async function loadPublish() {
  const r = await call("publish_status");
  S.pub.status = r.status; S.pub.history = r.history.slice().reverse();
}
function guideBlock(id, steps, notes) {
  const open = S.openGuide[id];
  return `<div class="guide"><button class="guide-toggle" data-guide="${id}">${open ? "▾" : "▸"} Step-by-step setup guide</button>
    ${open ? `<ol class="guide-steps">${steps.map((x) => `<li>${x}</li>`).join("")}</ol><div class="guide-notes">${notes.map((n) => `<p>⚠ ${n}</p>`).join("")}</div>` : ""}</div>`;
}
const GUIDES = {
  youtube: [[
    'Open <b>console.cloud.google.com</b> and create a project (any name).',
    'Go to <b>APIs &amp; Services → Library</b>, search <b>YouTube Data API v3</b> and press <b>Enable</b>.',
    'Go to <b>OAuth consent screen</b> (Google Auth Platform): choose <b>External</b>, fill in app name and your email.',
    'Under <b>Audience / Test users</b> add your own Google account (the one that owns the YouTube channel).',
    'Go to <b>Credentials → Create credentials → OAuth client ID</b>, type <b>Desktop app</b>, then <b>Download JSON</b>.',
    'Press <b>Import OAuth JSON</b> below, pick that file, then press <b>Connect YouTube</b> and allow both permissions.',
  ], [
    'While the Google project is unverified, videos uploaded through the API are forced to <b>Private</b>. Submit the project for the YouTube API compliance audit to publish publicly, or switch each video to Public in YouTube Studio.',
    'In <b>Testing</b> mode Google expires the sign-in after 7 days: press Connect again. Publishing the OAuth consent screen to <b>In production</b> removes that limit.',
    'The free API quota is 10,000 units a day: about 6 uploads per day.',
    'Vertical videos up to 3 minutes become Shorts automatically.',
  ]],
  facebook: [[
    'Open <b>developers.facebook.com → My Apps → Create App</b> (type <b>Business</b>).',
    'Open <b>Tools → Graph API Explorer</b>, select your app and press <b>Generate Access Token</b>.',
    'Tick these permissions: <b>pages_show_list</b>, <b>pages_read_engagement</b>, <b>pages_manage_posts</b>. Approve and select your Page.',
    'Copy the token. Copy <b>App ID</b> and <b>App Secret</b> from <b>App settings → Basic</b>.',
    'Paste all three below and press <b>Connect Facebook</b>. The app swaps the token for a long-lived one and finds your Pages.',
  ], [
    'You must be an <b>admin</b> of the Page. In Development mode the app works for the app owner and added testers; other people need Meta App Review.',
    'Reels must be vertical (9:16), 3–90 seconds, at least 540×960 and under 1 GB.',
    'Page posting only: this does not post to personal profiles.',
  ]],
  tiktok: [[
    'Open <b>developers.tiktok.com → Manage apps → Connect an app</b> and create an app.',
    'Add the products <b>Login Kit</b> and <b>Content Posting API</b>, and switch on <b>Direct Post</b>.',
    'In Login Kit add this exact <b>Redirect URI</b>: <b>__REDIRECT__</b>',
    'In <b>Sandbox / Target users</b> add your TikTok account so you can sign in while the app is in development.',
    'Copy <b>Client key</b> and <b>Client secret</b>, paste them below, press <b>Save keys</b>, then <b>Connect TikTok</b>.',
  ], [
    'Until TikTok audits your app, every post is forced to <b>Only me (private)</b>, and the TikTok account must be private. Submit the app for audit to post publicly.',
    'The redirect URI has to match character for character, including the trailing slash.',
    'TikTok limits how many posts an account can make per day.',
  ]],
};
function pubStatus(p) {
  const st = S.pub.status[p];
  return st.connected ? `<span class="badge completed">Connected${st.name ? " · " + esc(st.name) : ""}</span>` : `<span class="badge">Not connected</span>`;
}
function ytCard() {
  const st = S.pub.status.youtube;
  return `<div class="card stack" data-pcard="youtube"><div class="row" style="justify-content:space-between"><h2 style="margin:0">YouTube Shorts</h2>${pubStatus("youtube")}</div>
    ${guideBlock("youtube", ...GUIDES.youtube)}
    <div class="row"><button class="btn" data-act="yt-import">${st.configured ? "Replace OAuth JSON" : "Import OAuth JSON"}</button>
      <button class="btn primary" data-act="yt-login" ${st.configured ? "" : "disabled"}>${st.connected ? "Reconnect" : "Connect YouTube"}</button>
      ${st.connected ? `<button class="btn danger" data-disc="youtube">Disconnect</button>` : ""}</div>
    ${st.message ? `<p class="small muted" id="auth-youtube">${esc(st.message)}</p>` : ""}</div>`;
}
function fbCard() {
  const st = S.pub.status.facebook;
  return `<div class="card stack" data-pcard="facebook"><div class="row" style="justify-content:space-between"><h2 style="margin:0">Facebook Reels</h2>${st.connected ? `<span class="badge completed">Connected · ${st.pages.length} Page${st.pages.length === 1 ? "" : "s"}</span>` : `<span class="badge">Not connected</span>`}</div>
    ${guideBlock("facebook", ...GUIDES.facebook)}
    ${st.connected ? `<label class="field" style="max-width:360px">Post to this Page<select id="fb-page">${st.pages.map((g) => `<option value="${esc(g.id)}" ${g.id === st.selected ? "selected" : ""}>${esc(g.name)}</option>`).join("")}</select></label>` : ""}
    <div class="grid-3"><label class="field">App ID<input type="text" id="fb-id" autocomplete="off" /></label><label class="field">App secret<input type="password" id="fb-secret" autocomplete="off" /></label><label class="field">Access token<input type="password" id="fb-token" autocomplete="off" /></label></div>
    <div class="row"><button class="btn primary" data-act="fb-connect">${st.connected ? "Reconnect Facebook" : "Connect Facebook"}</button>${st.connected ? `<button class="btn danger" data-disc="facebook">Disconnect</button>` : ""}</div></div>`;
}
function ttCard() {
  const st = S.pub.status.tiktok;
  const guide = GUIDES.tiktok;
  return `<div class="card stack" data-pcard="tiktok"><div class="row" style="justify-content:space-between"><h2 style="margin:0">TikTok</h2>${pubStatus("tiktok")}</div>
    ${guideBlock("tiktok", guide[0].map((x) => x.replace("__REDIRECT__", esc(st.redirect))), guide[1])}
    <div class="row"><span class="muted small">Redirect URI to register:</span><code class="code">${esc(st.redirect)}</code><button class="btn sm" data-copy="${esc(st.redirect)}">Copy</button></div>
    <div class="grid-2"><label class="field">Client key<input type="text" id="tt-key" autocomplete="off" /></label><label class="field">Client secret<input type="password" id="tt-secret" autocomplete="off" /></label></div>
    <div class="row"><button class="btn" data-act="tt-save">Save keys</button><button class="btn primary" data-act="tt-login" ${st.configured ? "" : "disabled"}>${st.connected ? "Reconnect" : "Connect TikTok"}</button>
      ${st.connected ? `<button class="btn danger" data-disc="tiktok">Disconnect</button>` : ""}</div>
    ${st.message ? `<p class="small muted" id="auth-tiktok">${esc(st.message)}</p>` : ""}</div>`;
}
function receiptRow(r) {
  const live = ["queued", "uploading", "processing"].includes(r.status);
  return `<div class="receipt" data-rid="${r.id}"><span class="plat-dot" style="background:${PLATFORMS[r.provider]?.color || "#999"}"></span>
    <div class="grow"><div class="task-title">${esc(r.title)}</div><div class="muted small">${esc(PLATFORMS[r.provider]?.name || r.provider)} · ${esc(r.account_name)} · ${new Date(r.created).toLocaleString()}</div>
      ${live ? `<div class="bar" style="margin-top:6px"><i class="shimmer" style="width:${r.progress}%"></i></div>` : ""}<div class="small ${["failed", "uncertain"].includes(r.status) ? "err-text" : "muted"}">${esc(r.message)}</div></div>
    <span class="badge ${r.status === "published" || r.status === "uploaded" ? "completed" : r.status === "failed" || r.status === "uncertain" ? "error" : "processing"}">${esc(r.status)}</span>
    ${r.url ? `<button class="btn sm" data-open="${esc(r.url)}">Open</button>` : ""}</div>`;
}
function renderPublish() {
  $("#view").innerHTML = `<div class="page stack"><div><h1>Publishing</h1><p class="muted">Post finished clips to your own accounts. Connect each platform once; the guides list exactly what each one needs.</p></div>
    ${ytCard()}${fbCard()}${ttCard()}
    <div class="card stack"><div class="row" style="justify-content:space-between"><h2 style="margin:0">Upload history</h2>${S.pub.history.length ? `<button class="btn sm" data-act="clear-history">Clear</button>` : ""}</div>
      <div id="receipts" class="stack" style="gap:0">${S.pub.history.length ? S.pub.history.map(receiptRow).join("") : `<div class="empty" style="padding:20px 0">Nothing published yet.</div>`}</div></div></div>`;
}
function openPublishModal(clipId) {
  const clip = S.clips.find((c) => c.id === clipId);
  S.publishClip = { clip, platforms: new Set() };
  loadPublish().then(renderPublishModal);
}
function renderPublishModal() {
  const P = S.publishClip;
  if (!P) { $("#modal-root").innerHTML = ""; return; }
  const st = S.pub.status, d = P.clip.doc;
  const connected = { youtube: st.youtube.connected, facebook: st.facebook.connected, tiktok: st.tiktok.connected };
  const cur = $("#p-title")?.value ?? (d.hook_title || ""), desc = $("#p-desc")?.value ?? "", privacy = $("#p-privacy")?.value || "private";
  $("#modal-root").innerHTML = `<div class="modal-back"><div class="modal" style="width:min(640px,100%)">
    <div class="modal-head"><h2 style="margin:0">Publish clip</h2><button class="btn sm" data-act="close-publish">Close</button></div>
    <div class="stack" style="padding:20px">
      <div class="stack" style="gap:8px"><span class="muted small">Post to</span>${Object.keys(PLATFORMS).map((k) => `<label class="check plat-pick ${connected[k] ? "" : "off"}"><input type="checkbox" data-plat="${k}" ${P.platforms.has(k) ? "checked" : ""} ${connected[k] ? "" : "disabled"} /><span class="plat-dot" style="background:${PLATFORMS[k].color}"></span>${PLATFORMS[k].name}${connected[k] ? "" : ` <button class="btn sm" data-act="goto-publish">Set up</button>`}</label>`).join("")}</div>
      <div class="row" style="justify-content:space-between"><span class="muted small">Title and description</span><button class="btn sm" data-act="kit-write">Write with AI</button></div>
      <label class="field">Title<input type="text" id="p-title" maxlength="100" value="${esc(cur)}" /></label>
      <label class="field">Description / hashtags<textarea id="p-desc" placeholder="Add hashtags like #shorts #clips">${esc(desc)}</textarea></label>
      <div class="grid-2"><label class="field">Visibility<select id="p-privacy">${[["private", "Private / only me"], ["unlisted", "Unlisted / friends"], ["public", "Public"]].map((x) => `<option value="${x[0]}" ${privacy === x[0] ? "selected" : ""}>${x[1]}</option>`).join("")}</select></label>
        <label class="check" style="align-self:end;padding-bottom:10px"><input type="checkbox" id="p-kids" />Made for kids (YouTube)</label></div>
      <p class="muted small">Unverified YouTube projects and unaudited TikTok apps can only post privately. Facebook Reels always publish on the Page.</p>
    </div>
    <div class="modal-foot"><button class="btn" data-act="close-publish">Cancel</button><button class="btn primary" data-act="do-publish" ${P.platforms.size ? "" : "disabled"}>Publish</button></div></div></div>`;
}

// ---------- editor ----------
async function openEditor(id) {
  const clip = S.clips.find((c) => c.id === id);
  const w = await call("clip_words", { clipId: id });
  S.editor = { clip, tab: "trim", ranges: clip.doc.ranges.map((r) => [...r]), words: w.words, doc: JSON.parse(JSON.stringify(clip.doc)) };
  renderEditor();
}
function editorTab() {
  const E = S.editor, d = E.doc;
  if (E.tab === "trim") {
    return `<p class="muted small">Times are positions in the source video (seconds). Changing them re-cuts the clip and re-centres the speaker.</p>
      ${E.ranges.map((r, i) => `<div class="range-row"><span class="muted">${i + 1}</span>
        <label class="field">Start (${fmt(r[0])})<input type="number" step="0.1" min="0" data-range="${i}" data-edge="0" value="${r[0].toFixed(2)}" /></label>
        <label class="field">End (${fmt(r[1])})<input type="number" step="0.1" min="0" data-range="${i}" data-edge="1" value="${r[1].toFixed(2)}" /></label>
        <span class="row"><button class="btn sm" data-nudge="${i},0,-1">◀ start</button><button class="btn sm" data-nudge="${i},1,1">end ▶</button>${E.ranges.length > 1 ? `<button class="btn sm danger" data-rm-range="${i}">✕</button>` : ""}</span></div>`).join("")}
      <div class="row" style="margin-top:8px"><span class="muted small">Length: ${fmt(E.ranges.reduce((a, r) => a + r[1] - r[0], 0))}</span></div>
      <hr style="border-color:var(--line);width:100%" />
      <div class="row"><button class="btn" data-act="split-here">Split at playhead</button><span class="muted small">Pause the preview where the clip should be cut in two.</span></div>
      <div class="row"><button class="btn" data-act="reframe">Re-detect speaker framing</button></div>`;
  }
  if (E.tab === "captions") {
    return `<p class="muted small">Click a word to fix it. Clear a word to hide it. Timing stays in sync.</p>
      <div class="words">${E.words.map((w) => `<span class="word ${w.text !== w.original ? "changed" : ""} ${!w.text ? "hidden-word" : ""}" contenteditable="true" spellcheck="false" data-key="${w.key}" data-original="${esc(w.original)}">${esc(w.text || "")}</span>`).join("")}</div>
      ${E.words.length ? "" : `<div class="empty">No transcript words for this clip.</div>`}`;
  }
  return `<label class="field">Hook title<input type="text" id="e-hook" value="${esc(d.hook_title || "")}" placeholder="No title" /></label>
    <div class="templates">${S.templates.map((t) => tplCard(t, d.template === t.id)).join("")}</div>
    <div class="grid-3">
      <label class="field">Font<select id="e-font">${fontOptions(d.font_family)}</select></label>
      <label class="field">Font size<input type="number" id="e-size" min="12" max="72" value="${d.font_size ?? ""}" /></label>
      <label class="field">Text colour<span class="row"><input type="color" id="e-color" value="${d.font_color || "#ffffff"}" /><button class="btn sm" data-act="e-reset-color">Default</button></span></label>
    </div>
    <div class="row" style="gap:22px"><label class="check"><input type="checkbox" id="e-captions" ${d.captions ? "checked" : ""} />Captions</label>
      <label class="check"><input type="checkbox" id="e-showhook" ${d.show_hook ? "checked" : ""} />Show hook title</label>
      <label class="field" style="min-width:190px">Layout<select id="e-layout">${[["auto", "Follow the speaker"], ["gaming", "Gaming split"], ["cinema", "Cinematic (no facecam)"], ["center", "Centre crop"]].map((x) => `<option value="${x[0]}" ${(d.layout || "auto") === x[0] ? "selected" : ""}>${x[1]}</option>`).join("")}</select></label>
      <label class="field" style="min-width:190px">Outro<select id="e-outro">${[["", "Follow the setting"], ["on", "Add outro"], ["off", "No outro"]].map((x) => `<option value="${x[0]}" ${(d.outro === true ? "on" : d.outro === false ? "off" : "") === x[0] ? "selected" : ""}>${x[1]}</option>`).join("")}</select></label>
      <label class="field" style="min-width:190px">Cut style<select id="e-transition">${[["", "None"], ["dip", "Dip to black"], ["flash", "Flash to white"]].map((x) => `<option value="${x[0]}" ${(d.transition || "") === x[0] ? "selected" : ""}>${x[1]}</option>`).join("")}</select></label>
      ${d.broll.length ? `<label class="check"><input type="checkbox" id="e-rmbroll" />Remove B-roll (${d.broll.length})</label>` : ""}</div>`;
}
function renderEditor() {
  const E = S.editor;
  if (!E) { $("#modal-root").innerHTML = ""; return; }
  const c = E.clip, t = $("#e-video")?.currentTime || 0;
  $("#modal-root").innerHTML = `<div class="modal-back"><div class="modal">
    <div class="modal-head"><h2 style="margin:0">Edit clip</h2><button class="btn sm" data-act="close-editor">Close</button></div>
    <div class="modal-body"><div class="stack">${c.file_path ? `<video id="e-video" class="editor-video" src="${fileSrc(c.file_path)}" controls></video>` : `<div class="editor-video"></div>`}
      <div class="ai-box"><label class="field">AI edit <span class="kbd-hint">Ctrl J</span><textarea id="ai-prompt" rows="2" placeholder="e.g. remove the ums, make it shorter, bigger yellow captions">${esc(E.aiText || "")}</textarea></label>
        <div class="chips">${["Remove filler words", "Tighten the pauses", "Make it shorter", "Keep the best 20 seconds", "Bigger captions"].map((x) => `<button class="chip chip-btn" data-ai-chip="${esc(x)}">${esc(x)}</button>`).join("")}</div>
        <button class="btn brand" data-act="ai-run" id="ai-run">Edit with AI</button></div></div>
      <div class="stack"><div class="tabs">${[["trim", "Trim & split"], ["captions", "Captions"], ["style", "Style & title"]].map((x) => `<button data-tab="${x[0]}" class="${E.tab === x[0] ? "on" : ""}">${x[1]}</button>`).join("")}</div>
        <div class="stack" id="e-tab">${editorTab()}</div></div></div>
    <div class="modal-foot"><button class="btn" data-act="close-editor">Cancel</button><button class="btn primary" data-act="apply-edit">Apply & re-render</button></div></div></div>`;
  const v = $("#e-video"); if (v && t) v.currentTime = t;
}
function collectEditor() {
  const E = S.editor, d = E.doc;
  if (E.tab === "captions") {
    $$(".word").forEach((el) => { const w = E.words.find((x) => x.key === el.dataset.key); if (w) w.text = el.innerText.trim(); });
  }
  if (E.tab === "style") {
    d.hook_title = $("#e-hook").value.trim() || null;
    d.font_family = $("#e-font").value || null;
    d.font_size = $("#e-size").value ? Number($("#e-size").value) : null;
    d.captions = $("#e-captions").checked; d.show_hook = $("#e-showhook").checked;
    d.transition = $("#e-transition").value || null;
    E.outro = $("#e-outro").value; E.layout = $("#e-layout").value;
    E.removeBroll = $("#e-rmbroll")?.checked || false;
  }
}
async function applyEdit() {
  collectEditor();
  const E = S.editor, d = E.doc;
  const known = new Set(E.words.map((w) => w.key));
  const edits = Object.fromEntries(Object.entries(E.clip.doc.word_edits || {}).filter(([k]) => !known.has(k)));
  E.words.forEach((w) => { if (w.text !== w.original) edits[w.key] = w.text; });
  const patch = { ranges: E.ranges, hook_title: d.hook_title, template: d.template, font_family: d.font_family, font_size: d.font_size, font_color: d.font_color, captions: d.captions, show_hook: d.show_hook, transition: d.transition || "none", word_edits: edits, layout: E.layout || d.layout || "auto", outro: E.outro ?? (d.outro === true ? "on" : d.outro === false ? "off" : "") };
  if (E.removeBroll) patch.remove_broll = true;
  if (E.reframe) patch.reframe = true;
  const updated = await call("update_clip", { clipId: E.clip.id, patch });
  S.editor = null; renderEditor(); upsertClip(updated);
}
function upsertClip(c) {
  const i = S.clips.findIndex((x) => x.id === c.id);
  if (i >= 0) S.clips[i] = c; else S.clips.push(c);
  if (S.view !== "task" || c.task_id !== S.taskId) return;
  const el = $(`[data-clip="${c.id}"]`);
  if (el) el.outerHTML = clipCard(c); else $("#clips")?.insertAdjacentHTML("beforeend", clipCard(c));
  refreshHead();
}

// ---------- settings ----------
async function refreshTools() {
  [S.tools, S.info] = await Promise.all([invoke("tools_status"), invoke("app_info")]);
  renderFoot();
}
async function loadUpdates(force) {
  S.updates = null;
  if (S.view === "settings" && $("#tools")) $("#tools").innerHTML = S.tools.map(toolRow).join("");
  try { S.updates = await invoke("tool_updates", { force: !!force }); } catch (e) { S.updates = []; }
  if (S.view === "settings" && $("#tools")) $("#tools").innerHTML = S.tools.map(toolRow).join("");
}
function toolButton(t) {
  if (!t.installed) return `<button class="btn sm" data-install="${t.name}" ${S.installing ? "disabled" : ""}>Install</button>`;
  if (S.installing && S.toolProgress[t.name]) return `<button class="btn sm" disabled><span class="btn-spin"></span>Updating…</button>`;
  if (!S.updates) return `<button class="btn sm" disabled><span class="btn-spin"></span>Checking…</button>`;
  const u = S.updates.find((x) => x.name === t.name);
  if (!u || !u.checked) return `<button class="btn sm" disabled title="Could not reach the update server">Can't check</button>`;
  if (!u.update) return `<button class="btn sm" disabled title="${u.latest ? "Latest: " + esc(u.latest) : "Nothing newer"}">Up to date</button>`;
  return `<button class="btn sm brand" data-install="${t.name}" ${S.installing ? "disabled" : ""}>${u.latest ? "Update to " + esc(u.latest.replace(/^v/, "")) : "Update"}</button>`;
}
function toolRow(t) {
  const p = S.toolProgress[t.name];
  return `<div class="tool"><div class="grow"><div class="tool-name">${esc(t.label)} ${t.required ? "" : `<span class="muted small">optional</span>`}</div>
      <div class="muted small">${esc(t.note)} ${t.installed ? "· " + esc(t.version) : ""}</div>
      ${p ? `<div class="bar" style="margin-top:6px"><i style="width:${Math.round(p.fraction * 100)}%"></i></div><div class="muted small">${esc(p.message)}</div>` : ""}</div>
    <span class="badge ${t.installed ? "completed" : "error"}">${t.installed ? "Installed" : "Missing"}</span>
    ${toolButton(t)}</div>`;
}

function groqCard() {
  const s = S.settings, g = S.groq || {}, tone = { ready: "ok", rate_limited: "warn", unavailable: "warn", not_configured: "off", disabled: "off", key_rejected: "bad", model_unavailable: "bad" }[g.state] || "off";
  const rem = g.remaining && (g.remaining.requests != null || g.remaining.tokens != null) ? `<span class="muted small">Left today: ${g.remaining.requests ?? "?"} requests · ${g.remaining.tokens ?? "?"} tokens</span>` : "";
  return `<div class="card stack"><div class="row" style="justify-content:space-between"><div><h2 style="margin:0">Groq AI (fast, free tier)</h2><p class="muted small">Tried first for every AI step. If Groq is limited or fails, your normal AI model above takes over.</p></div>
      <label class="check"><input type="checkbox" data-set="groq_enabled" ${s.groq_enabled ? "checked" : ""} />Use Groq first</label></div>
    <div class="grid-3"><label class="field">Groq API key<input type="password" data-set="groq_api_key" value="${esc(s.groq_api_key)}" placeholder="gsk_…" autocomplete="off" /></label>
      <label class="field">Model<select data-set="groq_model">${(g.models || []).map((m) => `<option ${s.groq_model === m ? "selected" : ""}>${esc(m)}</option>`).join("")}</select></label>
      <div class="row" style="align-items:flex-end"><button class="btn" data-act="test-groq">Save & test Groq</button></div></div>
    <div class="row"><span class="dot ${tone}"></span><span>${esc(g.message || "")}</span>${g.retry_in ? `<span class="muted small">Retry in ${g.retry_in}s</span>` : ""}${rem}</div></div>`;
}
function outroCard() {
  const s = S.settings;
  return `<div class="card outro-card"><div class="outro-left"><h2>Outro</h2><p class="muted small">The last screen of every video. Set it once, it stays until you change it here.</p></div>
    <div class="outro-preview" id="outro-preview">${S.outroPreview ? `<img src="${S.outroPreview}" alt="Outro preview" />` : `<div class="sk" style="width:100%;height:100%"></div>`}</div>
    <div class="stack outro-right"><label class="field">First line (big, gold)<input type="text" id="o-line1" maxlength="40" value="${esc(s.outro_line1)}" /></label>
      <label class="field">Second line (small, white)<input type="text" id="o-line2" maxlength="60" value="${esc(s.outro_line2)}" /></label>
      <div class="row"><button class="btn brand" data-act="outro-save">Save text</button><button class="btn" data-act="outro-logo">Change logo…</button>
        <label class="field" style="width:110px;flex-direction:row;align-items:center;gap:8px">Seconds<input type="number" id="o-secs" min="1" max="8" step="0.5" value="${s.outro_seconds}" style="width:70px" /></label></div>
      <label class="check"><input type="checkbox" id="o-enabled" ${s.outro_enabled ? "checked" : ""} />Add the outro to every new clip</label>
      <p class="muted small">You can also switch it on or off for a single clip in the editor (Style &amp; title).</p></div></div>`;
}
async function loadStorage() {
  try { S.storage = await invoke("storage_info"); } catch (e) { return; }
  const el = $("#storage-box"); if (el) el.innerHTML = storageHtml();
}
function storageHtml() {
  const st = S.storage; if (!st) return `<span class="muted small">Measuring…</span>`;
  const rows = [["Downloaded videos", st.sources], ["Finished clips", st.clips], ["Cache & temp files", st.cache], ["Tools & models", st.tools]];
  return `<div class="storage-grid">${rows.map((r) => `<div><b>${fmtBytes(r[1])}</b><span class="muted small">${r[0]}</span></div>`).join("")}<div><b>${fmtBytes(st.free)}</b><span class="muted small">Free on this drive</span></div></div>`;
}
async function loadOllama() {
  try { S.ollama = await invoke("list_ollama"); } catch (e) { S.ollama = []; }
  if (S.view === "settings" && S.settings.llm.startsWith("ollama:") && !typingInView()) renderSettings();
}
async function loadAiUsage() {
  const box = $("#ai-usage"); if (!box) return;
  let u; try { u = await invoke("ai_usage"); } catch (e) { return; }
  const nice = (m) => m.replace(/^google-gla:/, "Gemini ").replace(/^ollama:/, "Ollama ").replace(/^openrouter:/, "OpenRouter ").replace(/^groq:/, "Groq ");
  const rows = (u.models || []).map((r) => `<span class="chip" title="Requests answered today / failed">${esc(nice(r.model))} · ${r.ok} ok${r.failed ? " · " + r.failed + " failed" : ""}${r.resting ? " · resting" : ""}</span>`).join("");
  const anyRest = (u.models || []).some((r) => r.resting);
  box.innerHTML = rows ? `<div class="chips" style="align-items:center"><span>Used today:</span>${rows}${anyRest ? `<button class="btn sm" data-act="wake-models">Try resting models again</button>` : ""}</div>` : "No AI requests yet today.";
}
function chatgptCard() {
  const g = S.chatgpt || { connected: false };
  const head = `<div class="row" style="justify-content:space-between"><h2 style="margin:0">Use your ChatGPT plan</h2>${g.connected ? `<span class="chip">Connected${g.email ? " · " + esc(g.email) : ""}</span>` : ""}</div>`;
  const note = g.message && !g.connected ? `<p class="small">${esc(g.message)}</p>` : "";
  if (!g.connected) {
    return `${head}<p class="muted small">Sign in with your own ChatGPT account and the AI uses your plan instead of an API key. Needs ChatGPT Plus or Pro. Your browser opens at OpenAI; Nexora Clip never sees your password. Plan limits apply, and when they run out the free models take over.</p>${note}
      <div class="row"><button class="btn primary" data-act="chatgpt-login" id="gpt-login">Continue with ChatGPT</button></div>`;
  }
  const have = (S.chatgptModels || []).map((m) => [m.slug, m.name]);
  const cur = g.model || (have[0] && have[0][0]) || "";
  const opts = have.some((m) => m[0] === cur) || !cur ? have : [[cur, cur], ...have];
  const using = S.settings && S.settings.llm.startsWith("chatgpt:");
  return `${head}<p class="muted small">${using ? "ChatGPT answers first. If your plan runs out, the free models take over." : "ChatGPT is a backup now: it answers when the free models are busy."}</p>
    <div class="grid-3"><label class="field">Model<select id="gpt-model">${opts.length ? opts.map((m) => `<option value="${esc(m[0])}" ${m[0] === cur ? "selected" : ""}>${esc(m[1])}</option>`).join("") : '<option value="">Loading…</option>'}</select></label>
      <div class="row" style="align-items:flex-end"><button class="btn primary" data-act="chatgpt-use">${using ? "Save model" : "Use ChatGPT first"}</button></div>
      <div class="row" style="align-items:flex-end"><button class="btn" data-act="chatgpt-disconnect">Disconnect</button></div></div>`;
}
async function loadChatgpt(withModels) {
  try { S.chatgpt = await invoke("chatgpt_status"); } catch (e) { return; }
  if (S.chatgpt.connected && (withModels || !S.chatgptModels)) {
    try { S.chatgptModels = await invoke("chatgpt_models"); } catch (e) { S.chatgptModels = []; }
  }
  const box = $("#chatgpt-card");
  if (box && S.view === "settings" && !typingInView()) renderSettings();
}
async function loadOutroPreview() {
  try {
    const path = await invoke("outro_preview");
    S.outroPreview = fileSrc(path) + "?v=" + Date.now();
  } catch (err) { S.outroPreview = null; toastError(err); }
  const el = $("#outro-preview"); if (el && S.outroPreview) el.innerHTML = `<img src="${S.outroPreview}" alt="Outro preview" />`;
}

function renderSettings() {
  const s = S.settings, [prov, ...rest] = s.llm.split(":"), model = rest.join(":");
  const paid = PAID_PROVIDERS.find((p) => p[0] === prov);
  const showGpt = prov === "chatgpt" || (S.chatgpt && S.chatgpt.connected);
  const providerList = [...PROVIDERS, ...(paid ? [paid] : []), ...(showGpt ? [CHATGPT] : [])];
  const P = providerList.find((p) => p[0] === prov) || PROVIDERS[0];
  const listed = FREE_MODELS[P[0]];
  let modelField;
  if (P[0] === "chatgpt") {
    const have = (S.chatgptModels || []).map((m) => [m.slug, m.name]);
    const cur = model || (S.chatgpt && S.chatgpt.model) || "";
    const opts = have.some((m) => m[0] === cur) || !cur ? have : [[cur, cur + " (current)"], ...have];
    modelField = opts.length
      ? `<select id="s-model">${opts.map((m) => `<option value="${esc(m[0])}" ${m[0] === cur ? "selected" : ""}>${esc(m[1])}</option>`).join("")}</select>`
      : `<input type="text" id="s-model" value="${esc(cur)}" placeholder="Loading your models…" />`;
  } else if (listed) {
    const opts = listed.some((m) => m[0] === model) || !model ? listed : [[model, model + " (current)"], ...listed];
    modelField = `<select id="s-model">${opts.map((m) => `<option value="${esc(m[0])}" ${m[0] === (model || P[2]) ? "selected" : ""}>${esc(m[1])}</option>`).join("")}</select>`;
  } else if (P[0] === "ollama") {
    const have = S.ollama || [];
    modelField = have.length
      ? `<select id="s-model">${(have.includes(model) || !model ? have : [model, ...have]).map((m) => `<option value="${esc(m)}" ${m === (model || have[0]) ? "selected" : ""}>${esc(m)}</option>`).join("")}</select>`
      : `<input type="text" id="s-model" value="${esc(model)}" placeholder="Start Ollama, then: ollama pull qwen2.5:7b" />`;
  } else {
    modelField = `<input type="text" id="s-model" value="${esc(model)}" placeholder="${esc(P[2])}" />`;
  }
  const key = (id, label, ph = "") => `<label class="field">${label}<input type="password" data-set="${id}" value="${esc(s[id])}" placeholder="${ph}" autocomplete="off" /></label>`;
  const miss = S.tools.filter((t) => !t.installed);
  $("#view").innerHTML = `<div class="page stack">
    <div class="page-head" style="margin:0"><div><h1>Settings</h1><p class="muted">Keys stay on this PC, in the app's local database.</p></div><button class="btn primary" data-act="save-settings">Save settings</button></div>
    <div class="card"><div class="row" style="justify-content:space-between"><h2 style="margin:0">Tools</h2><button class="btn sm" data-act="recheck-updates" title="Look for newer versions">Check for updates</button>${miss.length ? `<button class="btn primary sm" data-act="install-all" ${S.installing ? "disabled" : ""}>Install everything missing</button>` : ""}</div>
      <div id="tools">${S.tools.map(toolRow).join("")}</div></div>
    <div class="card stack"><h2>AI model (clip selection)</h2>
      <p class="muted small">Only free ways to use AI are listed. If one is busy or out of free quota, the next free one answers by itself, so a clip run does not stop.</p>
      <div class="grid-3"><label class="field">Provider<select id="s-prov">${providerList.map((p) => `<option value="${p[0]}" ${p[0] === P[0] ? "selected" : ""}>${p[1]}</option>`).join("")}</select></label>
        <label class="field">Model${modelField}</label>
        ${P[0] === "chatgpt" ? `<label class="field">Account<input type="text" disabled value="${esc((S.chatgpt && S.chatgpt.email) || "Connected")}" /></label>` : P[3] ? key(P[3], P[1].replace(/ \(.*\)/, "") + " API key") : `<label class="field">Ollama address<input type="text" data-set="ollama_base_url" value="${esc(s.ollama_base_url)}" /></label>`}</div>
      ${P[0] === "custom" ? `<label class="field">Base URL (…/v1)<input type="text" data-set="custom_base_url" value="${esc(s.custom_base_url)}" placeholder="http://localhost:1234/v1" /></label>` : ""}
      <div class="grid-3"><label class="field">Rerank model for long videos (optional, provider:model)<input type="text" data-set="rerank_llm" value="${esc(s.rerank_llm)}" placeholder="same as above" /></label>
        <label class="field">Most clips per video<input type="number" data-set="max_clips" data-num min="1" max="20" value="${s.max_clips}" /></label>
        <div class="row" style="align-items:flex-end"><button class="btn" id="test-llm-btn" data-act="test-llm">Save & test AI connection</button></div></div>
      <div id="ai-test"></div>
      <div id="ai-usage" class="muted small"></div></div>
    <div class="card stack" id="chatgpt-card">${chatgptCard()}</div>
    ${groqCard()}
    <div class="card stack"><h2>Game knowledge and captions</h2>
      <p class="muted small">For GTA VI videos the AI knows the characters, places and release facts, speech recognition is told their names, and captions get their spelling right. Hindi and Urdu captions can be shown in Roman letters ("yaar kya hai").</p>
      <div class="grid-3"><label class="field">Game knowledge<select data-set="knowledge_profile">${[["auto", "Notice it from the video (recommended)"], ["gta6", "GTA VI, always"], ["off", "Off"]].map((o) => `<option value="${o[0]}" ${s.knowledge_profile === o[0] ? "selected" : ""}>${o[1]}</option>`).join("")}</select></label>
        <label class="check" style="align-self:end;padding-bottom:10px"><input type="checkbox" data-set="roman_captions" ${s.roman_captions ? "checked" : ""} />Hindi / Urdu captions in Roman letters</label>
        <div class="row" style="align-items:flex-end"><button class="btn" data-act="open-knowledge">Open my GTA notes</button></div></div></div>
    <div class="card stack"><h2>Transcription</h2>
      <div class="grid-3"><label class="field">Provider<select data-set="transcription_provider" data-rerender>
          ${[["whisper_local", "Whisper on this PC (free, offline)"], ["assemblyai", "AssemblyAI (best accuracy, speakers)"], ["groq", "Groq Whisper (fast, cloud)"], ["openai", "OpenAI Whisper"]].map((o) => `<option value="${o[0]}" ${s.transcription_provider === o[0] ? "selected" : ""}>${o[1]}</option>`).join("")}</select></label>
        ${s.transcription_provider === "whisper_local" ? `<label class="field">Whisper model<select data-set="whisper_model">${[["tiny", "tiny (75 MB, fastest)"], ["base", "base (142 MB)"], ["small", "small (466 MB)"], ["medium", "medium (1.5 GB)"], ["large-v3-turbo", "large-v3-turbo (1.6 GB, best)"]].map((o) => `<option value="${o[0]}" ${s.whisper_model === o[0] ? "selected" : ""}>${o[1]}</option>`).join("")}</select></label>` : ""}
        ${s.transcription_provider === "assemblyai" ? key("assemblyai_api_key", "AssemblyAI API key") : ""}
        ${s.transcription_provider === "groq" ? `<label class="field">Groq key<input type="text" disabled value="${s.groq_api_key ? "Saved in the Groq card above" : "Add it in the Groq card above"}" /></label>` : ""}
        ${s.transcription_provider === "openai" ? key("openai_api_key", "OpenAI API key") : ""}
        <label class="field">Language (blank = auto-detect)<input type="text" data-set="transcription_language" value="${esc(s.transcription_language)}" placeholder="en, es, de…" /></label></div></div>
    <div class="card stack"><h2>Rendering</h2>
      <div class="grid-3"><label class="field">Video encoder<select data-set="encoder">${[["auto", `Auto (now: ${S.info.encoder || "…"})`], ["libx264", "CPU (libx264)"], ["h264_nvenc", "NVIDIA GPU"], ["h264_qsv", "Intel Quick Sync"], ["h264_amf", "AMD GPU"]].map((o) => `<option value="${o[0]}" ${s.encoder === o[0] ? "selected" : ""}>${o[1]}</option>`).join("")}</select></label>
        <label class="field">CPU speed preset<select data-set="x264_preset">${["ultrafast", "veryfast", "fast", "medium", "slow"].map((o) => `<option ${s.x264_preset === o ? "selected" : ""}>${o}</option>`).join("")}</select></label>
        <label class="field">Output size<select data-set="output_height" data-num><option value="1920" ${s.output_height > 1280 ? "selected" : ""}>1080 × 1920</option><option value="1280" ${s.output_height <= 1280 ? "selected" : ""}>720 × 1280 (faster)</option></select></label>
        <label class="field">Clips rendered at once<input type="number" data-set="parallel_renders" data-num min="1" max="4" value="${s.parallel_renders}" /></label>
        <label class="check"><input type="checkbox" data-set="loudnorm" ${s.loudnorm ? "checked" : ""} />Normalize loudness</label>
        <label class="check"><input type="checkbox" data-set="ken_burns" ${s.ken_burns ? "checked" : ""} />Slow zoom on static shots</label>
        <label class="check" title="Renders about twice as fast"><input type="checkbox" data-set="fps_30" ${s.fps_30 ? "checked" : ""} />Limit to 30 fps (faster)</label>
        <label class="check" title="YouTube videos that already have subtitles skip transcription: faster and free"><input type="checkbox" data-set="yt_subtitles" ${s.yt_subtitles ? "checked" : ""} />Use the video's own subtitles when available</label>
        <label class="check" title="Needs a Google key. Gemini looks at a few frames, or at a small copy of videos with little speech, to pick moments, write hooks and spot burned-in subtitles. Those pictures are sent to Google."><input type="checkbox" data-set="gemini_vision" ${s.gemini_vision ? "checked" : ""} />Let Gemini look at the picture (hooks, quiet videos, burned-in subtitles)</label>
        <label class="check"><input type="checkbox" data-set="notify_done" ${s.notify_done ? "checked" : ""} />Notify me when clips are ready</label></div></div>
    ${outroCard()}
    <div class="card stack"><h2>Downloads & B-roll</h2>
      <div class="grid-3"><label class="field">Max download height<select data-set="youtube_max_height" data-num>${[720, 1080, 1440, 2160].map((h) => `<option value="${h}" ${s.youtube_max_height === h ? "selected" : ""}>${h}p</option>`).join("")}</select></label>
        <label class="field">Use cookies from browser (optional)<input type="text" data-set="cookies_from_browser" value="${esc(s.cookies_from_browser)}" placeholder="chrome, edge, firefox" /></label>
        ${key("pexels_api_key", "Pexels API key (B-roll)")}</div></div>
    <div class="card stack"><h2>Fonts & storage</h2>
      <div class="row"><button class="btn" data-act="add-font">Add a font (.ttf / .otf)</button><span class="muted small">${S.fonts.length} fonts available</span></div>
      <div id="storage-box">${storageHtml()}</div>
      <div class="row"><button class="btn" data-act="open-data">Open data folder</button><button class="btn" data-act="clear-leftovers">Clear cache &amp; leftovers</button><button class="btn" data-act="open-logs">Open logs</button></div>
      <p class="muted small">${esc(S.info.data_dir || "")} · Keys are stored encrypted for this Windows account.</p>
      <p class="muted small">Nexora Clip ${esc(S.info.version || "")} · AGPL-3.0</p></div></div>`;
}
function readSettings() {
  const s = S.settings;
  $$("[data-set]").forEach((el) => {
    const k = el.dataset.set;
    s[k] = el.type === "checkbox" ? el.checked : "num" in el.dataset ? Number(el.value) : el.value.trim();
  });
  const prov = $("#s-prov"), model = $("#s-model");
  if (prov) { const P = [...PROVIDERS, ...PAID_PROVIDERS, CHATGPT].find((p) => p[0] === prov.value); s.llm = `${prov.value}:${(model && model.value.trim()) || P[2]}`; }
}
async function saveSettings(quiet) {
  readSettings();
  await call("save_settings", { settings: S.settings });
  if (!quiet) toast("Settings saved");
  await refreshTools();
}
async function installTools(names) {
  S.installing = true; renderSettings();
  for (const name of names) {
    try { await invoke("install_tool", { name }); } catch (err) { toastError(err); }
    delete S.toolProgress[name];
  }
  S.installing = false; await refreshTools(); if (S.view === "settings") renderSettings();
  loadUpdates(true);
}

// ---------- events ----------
document.addEventListener("click", async (ev) => {
  const el = ev.target.closest("[data-nav],[data-act],[data-wf],[data-len],[data-tpl],[data-task],[data-tab],[data-nudge],[data-rm-range],[data-install],[data-q],[data-layout],[data-ai-chip],[data-guide],[data-disc],[data-copy],[data-open]");
  if (!el) return;
  const d = el.dataset, o = S.form.options;
  if (d.nav) return go(d.nav);
  if (d.task) return go("task", d.task);
  if (d.wf) { S.form.workflow = d.wf; return renderCreate(); }
  if (d.len) { o.clip_length = d.len; return renderCreate(); }
  if (d.tpl) { if (S.editor) { collectEditor(); S.editor.doc.template = d.tpl; return renderEditor(); } o.template = d.tpl; return renderCreate(); }
  if (d.tab) { collectEditor(); S.editor.tab = d.tab; return renderEditor(); }
  if (d.nudge) { const [i, edge, dir] = d.nudge.split(",").map(Number); S.editor.ranges[i][edge] = Math.max(0, S.editor.ranges[i][edge] + dir); return renderEditor(); }
  if (d.rmRange) { S.editor.ranges.splice(Number(d.rmRange), 1); return renderEditor(); }
  if (d.install) return installTools([d.install]);
  if (d.aiChip) { const t = $("#ai-prompt"); t.value = d.aiChip; t.focus(); return; }
  if (d.layout) { o.layout = d.layout; const big = d.layout === "gaming" || d.layout === "cinema"; if (big && o.template === "default") o.template = "gaming"; if (!big && o.template === "gaming") o.template = "default"; return renderCreate(); }
  if (d.top !== undefined) { o.render_top = Number(d.top); return renderCreate(); }
  if (d.q) { o.download_quality = d.q; return refreshProbe(); }
  if (d.guide) { S.openGuide[d.guide] = !S.openGuide[d.guide]; return renderPublish(); }
  if (d.disc) { if (await askConfirm({ title: "Disconnect " + PLATFORMS[d.disc].name + "?", text: "You can connect it again later.", label: "Disconnect" })) { await call("publish_disconnect", { provider: d.disc }); await loadPublish(); renderPublish(); } return; }
  if (d.copy) { try { await navigator.clipboard.writeText(d.copy); toast("Copied"); } catch (e) { toast(d.copy); } return; }
  if (d.open) return T.opener.openUrl(d.open);
  const id = d.id;
  switch (d.act) {
    case "goto-settings": return go("settings");
    case "pick-file": {
      const p = await T.dialog.open({ multiple: false, filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a"] }] });
      if (p) { S.form.path = p; S.form.url = ""; renderCreate(); }
      return;
    }
    case "clear-file": S.form.path = ""; return renderCreate();
    case "reset-color": o.font_color = null; return renderCreate();
    case "create": {
      const input = { url: S.form.path ? null : S.form.url, path: S.form.path || null, workflow: S.form.workflow, options: o };
      const task = await call("create_task", { input });
      S.form.url = ""; S.form.path = "";
      return go("task", task.id);
    }
    case "cancel": return call("cancel_task", { id: S.taskId });
    case "resume": await call("resume_task", { id: S.taskId }); return;
    case "del-task": if (await askConfirm({ title: "Delete this video?", text: "Its clips are deleted from this PC too. This cannot be undone.", label: "Delete" })) { await call("delete_task", { id: S.taskId }); go("library"); } return;
    case "zip": {
      const dest = await T.dialog.save({ defaultPath: (S.task.title || "clips").replace(/[\\/:*?"<>|]/g, " ").slice(0, 60) + ".zip", filters: [{ name: "ZIP", extensions: ["zip"] }] });
      if (dest) { const n = await call("export_zip", { taskId: S.taskId, dest }); toast(`Saved ${n} clips`); T.opener.revealItemInDir(dest); }
      return;
    }
    case "merge": { const c = await call("merge_clips", { clipIds: [...S.selected] }); S.selected.clear(); upsertClip(c); return; }
    case "edit": return openEditor(id);
    case "gen": { const c = await call("render_candidate", { clipId: id }); upsertClip(c); return; }
    case "gen-all": { for (const c of S.clips.filter((x) => x.status === "candidate")) upsertClip(await call("render_candidate", { clipId: c.id })); return; }
    case "kit": {
      el.disabled = true; const label = el.textContent; el.innerHTML = '<span class="btn-spin"></span>Writing…';
      try {
        const name = (await call("suggested_name", { clipId: id })).replace(/\.mp4$/i, "") + "_kit.zip";
        const dest = await T.dialog.save({ defaultPath: name, filters: [{ name: "ZIP", extensions: ["zip"] }] });
        if (dest) { await invoke("save_kit", { clipId: id, dest }); toast("Publish kit saved"); T.opener.revealItemInDir(dest); }
      } catch (err) { toastError(err); }
      el.disabled = false; el.textContent = label; return;
    }
    case "kit-write": {
      el.disabled = true; el.innerHTML = '<span class="btn-spin"></span>Writing…';
      try {
        const k = await invoke("make_kit", { clipId: S.publishClip.clip.id });
        const pl = S.publishClip.platforms;
        const only = (p) => pl.size === 1 && pl.has(p);
        $("#p-title").value = k.youtube_title;
        $("#p-desc").value = only("tiktok") ? k.tiktok_caption : only("facebook") ? k.facebook_caption : k.youtube_description;
        toast("Text written. Check it before you publish.");
      } catch (err) { toastError(err); }
      el.disabled = false; el.textContent = "Write with AI"; return;
    }
    case "save": {
      const name = await call("suggested_name", { clipId: id });
      const dest = await T.dialog.save({ defaultPath: name, filters: [{ name: "Video", extensions: ["mp4"] }] });
      if (dest) { await call("save_clip", { clipId: id, dest }); toast("Clip saved"); T.opener.revealItemInDir(dest); }
      return;
    }
    case "del-clip": if (await askConfirm({ title: "Delete this clip?", text: "The clip file is removed from this PC.", label: "Delete" })) { await call("delete_clip", { clipId: id }); S.clips = S.clips.filter((c) => c.id !== id); S.selected.delete(id); renderTask(); } return;
    case "ai-run": {
      const prompt = $("#ai-prompt").value.trim();
      if (!prompt) { toast("Describe the change you want first.", true); return; }
      el.disabled = true; el.innerHTML = '<span class="btn-spin"></span>AI is editing…';
      try {
        const r = await invoke("ai_edit", { clipId: S.editor.clip.id, prompt });
        S.editor = null; renderEditor(); upsertClip(r.clip);
        toast(r.summary + (r.notes.length ? " Skipped: " + r.notes.join(" ") : ""));
      } catch (err) { toastError(err); el.disabled = false; el.textContent = "Edit with AI"; }
      return;
    }
    case "close-editor": S.editor = null; return renderEditor();
    case "apply-edit": return applyEdit();
    case "e-reset-color": S.editor.doc.font_color = null; collectEditor(); return renderEditor();
    case "reframe": S.editor.reframe = true; toast("Framing will be re-detected when you apply."); return;
    case "split-here": {
      const at = $("#e-video")?.currentTime || 0;
      const parts = await call("split_clip", { clipId: S.editor.clip.id, at });
      S.editor = null; renderEditor(); parts.forEach(upsertClip); return;
    }
    case "save-settings": return saveSettings();
    case "test-groq": {
      await saveSettings(true); el.disabled = true; el.innerHTML = '<span class="btn-spin"></span>Testing…';
      try { await invoke("test_groq"); toast("Groq works"); } catch (err) { toastError(err); }
      S.groq = await invoke("groq_status"); renderSettings(); return;
    }
    case "outro-save": {
      readSettings(); S.settings.outro_line1 = $("#o-line1").value.trim(); S.settings.outro_line2 = $("#o-line2").value.trim();
      S.settings.outro_seconds = Math.min(8, Math.max(1, Number($("#o-secs").value) || 2.5)); S.settings.outro_enabled = $("#o-enabled").checked;
      await call("save_settings", { settings: S.settings }); S.outroPreview = null; $("#outro-preview").innerHTML = '<div class="sk" style="width:100%;height:100%"></div>';
      toast("Outro saved"); return loadOutroPreview();
    }
    case "outro-logo": {
      const f = await T.dialog.open({ multiple: false, filters: [{ name: "Picture", extensions: ["png", "jpg", "jpeg", "webp"] }] });
      if (!f) return;
      await call("outro_set_logo", { path: f }); S.outroPreview = null; $("#outro-preview").innerHTML = '<div class="sk" style="width:100%;height:100%"></div>';
      toast("Logo changed"); return loadOutroPreview();
    }
    case "test-llm": {
      const box = $("#ai-test"); const label = "Save & test AI connection";
      el.disabled = true; el.innerHTML = '<span class="btn-spin"></span>Testing…';
      box.innerHTML = `<div class="errcard testing"><div class="spin"></div><div class="grow"><b>Asking the model…</b><p class="muted small">This takes a few seconds.</p></div></div>`;
      try {
        await saveSettings(true); const r = await invoke("test_llm"); box.innerHTML = okCard(r.name + " works", `It answered in ${(r.ms / 1000).toFixed(1)} s.`); }
      catch (err) { box.innerHTML = errorCard(errInfo(err)); }
      el.disabled = false; el.textContent = label; loadAiUsage(); return;
    }
    case "chatgpt-login": {
      el.disabled = true; el.innerHTML = '<span class="btn-spin"></span>Waiting for your browser…';
      try { await invoke("chatgpt_login"); } catch (err) { toastError(err); el.disabled = false; el.textContent = "Continue with ChatGPT"; }
      return;
    }
    case "chatgpt-disconnect": {
      if (!(await askConfirm({ title: "Disconnect ChatGPT?", text: "Nexora Clip forgets the sign-in and asks OpenAI to revoke it. You can connect again any time.", label: "Disconnect" }))) return;
      try { await invoke("chatgpt_disconnect"); } catch (err) { return toastError(err); }
      S.chatgptModels = null;
      if (S.settings.llm.startsWith("chatgpt:")) { S.settings.llm = "google-gla:gemini-3.1-flash-lite"; await call("save_settings", { settings: S.settings }); }
      toast("ChatGPT disconnected"); return loadChatgpt(false);
    }
    case "chatgpt-use": {
      const slug = ($("#gpt-model") || {}).value; if (!slug) return toast("Pick a model first", true);
      try { await invoke("chatgpt_set_model", { model: slug }); S.settings.llm = `chatgpt:${slug}`; await call("save_settings", { settings: S.settings }); }
      catch (err) { return toastError(err); }
      toast("ChatGPT will answer first"); return loadChatgpt(false);
    }
    case "err-settings": return go("settings");
    case "recheck-updates": return loadUpdates(true);
    case "wake-models": await invoke("ai_reset_rest"); toast("All models will be tried again"); return loadAiUsage();
    case "open-knowledge": return call("open_folder", { which: "knowledge" });
    case "install-all": return installTools(S.tools.filter((t) => !t.installed).map((t) => t.name).filter((n) => n !== "ffprobe" || !S.tools.find((t) => t.name === "ffmpeg" && !t.installed)));
    case "add-font": {
      const p = await T.dialog.open({ multiple: false, filters: [{ name: "Font", extensions: ["ttf", "otf"] }] });
      if (p) { await call("add_font", { path: p }); S.fonts = await call("list_fonts"); toast("Font added"); renderSettings(); }
      return;
    }
    case "open-data": return call("open_folder", { which: "data" });
    case "open-logs": return call("open_folder", { which: "logs" });
    case "clear-leftovers": { const freed = await call("clear_leftovers"); toast(`Freed ${fmtBytes(freed)}`); S.storage = null; return loadStorage(); }
    case "publish": return openPublishModal(id);
    case "close-publish": S.publishClip = null; return renderPublishModal();
    case "goto-publish": S.publishClip = null; renderPublishModal(); return go("publish");
    case "do-publish": {
      const P = S.publishClip;
      try {
        await invoke("publish_clip", { clipId: P.clip.id, platforms: [...P.platforms], title: $("#p-title").value, description: $("#p-desc").value, privacy: $("#p-privacy").value, madeForKids: $("#p-kids").checked });
        S.publishClip = null; renderPublishModal(); toast("Upload started. Follow it under Publishing."); go("publish");
      } catch (err) { toastError(err); }
      return;
    }
    case "yt-import": {
      const f = await T.dialog.open({ multiple: false, filters: [{ name: "OAuth client JSON", extensions: ["json"] }] });
      if (!f) return;
      const text = await T.core.invoke("read_text_file", { path: f });
      await call("youtube_configure", { document: text }); toast("Google OAuth client imported"); await loadPublish(); renderPublish(); return;
    }
    case "yt-login": await call("youtube_login"); toast("Finish signing in in your browser"); return;
    case "fb-connect": {
      el.disabled = true; el.textContent = "Connecting…";
      try { await invoke("facebook_connect", { appId: $("#fb-id").value, appSecret: $("#fb-secret").value, token: $("#fb-token").value }); toast("Facebook connected"); }
      catch (err) { toastError(err); }
      await loadPublish(); renderPublish(); return;
    }
    case "tt-save": await call("tiktok_configure", { key: $("#tt-key").value, secret: $("#tt-secret").value }); toast("TikTok keys saved"); await loadPublish(); renderPublish(); return;
    case "tt-login": await call("tiktok_login"); toast("Finish signing in in your browser"); return;
    case "clear-history": await call("publish_clear_history"); await loadPublish(); renderPublish(); return;
  }
});
document.addEventListener("input", (ev) => {
  const el = ev.target, o = S.form.options;
  const map = { "f-url": () => { S.form.url = el.value; scheduleProbe(); }, "f-sec-start": () => (o.section_start = Number(el.value) || 0), "f-sec-end": () => (o.section_end = Number(el.value) || null), "f-topic": () => (o.topic = el.value), "f-instr": () => (o.instructions = el.value),
    "f-parts": () => (o.part_count = Number(el.value) || 3), "f-size": () => (o.font_size = el.value ? Number(el.value) : null),
    "f-color": () => (o.font_color = el.value), "e-color": () => (S.editor.doc.font_color = el.value) };
  if (map[el.id]) map[el.id]();
  if (el.dataset.range !== undefined) S.editor.ranges[Number(el.dataset.range)][Number(el.dataset.edge)] = Number(el.value) || 0;
  if (el.id === "ai-prompt" && S.editor) S.editor.aiText = el.value;
  if (el.classList?.contains("word")) { const t = el.innerText.trim(); el.classList.toggle("changed", t !== el.dataset.original); el.classList.toggle("hidden-word", !t); }
});
document.addEventListener("change", async (ev) => {
  const el = ev.target, o = S.form.options;
  if (el.id === "f-font") o.font_family = el.value || null;
  if (el.id === "f-captions") o.captions = el.checked;
  if (el.id === "f-hook") o.hook_titles = el.checked;
  if (el.id === "f-broll") o.broll = el.checked;
  if (el.id === "f-transition") o.transition = el.value || null;
  if (el.id === "f-section") { o.section_start = el.checked ? 0 : null; o.section_end = el.checked ? Math.floor(S.probe?.data?.duration || 60) : null; refreshProbe(); }
  if (el.id === "fb-page") { await call("facebook_select", { id: el.value }); toast("Page selected"); }
  if (el.dataset.plat) { el.checked ? S.publishClip.platforms.add(el.dataset.plat) : S.publishClip.platforms.delete(el.dataset.plat); renderPublishModal(); }
  if (el.id === "s-prov") { readSettings(); const P = [...PROVIDERS, ...PAID_PROVIDERS, CHATGPT].find((p) => p[0] === el.value); S.settings.llm = `${el.value}:${P[0] === "ollama" ? (S.ollama && S.ollama[0]) || "" : P[0] === "chatgpt" ? (S.chatgpt && S.chatgpt.model) || (S.chatgptModels && S.chatgptModels[0] && S.chatgptModels[0].slug) || "" : P[2]}`; renderSettings(); loadAiUsage(); }
  if ("rerender" in el.dataset) { readSettings(); await saveSettings(true); renderSettings(); }
  if (el.dataset.pick) { el.checked ? S.selected.add(el.dataset.pick) : S.selected.delete(el.dataset.pick); el.closest(".clip").classList.toggle("sel", el.checked); refreshHead(); }
  if (el.dataset.export && el.value) {
    const preset = el.value, id = el.dataset.export; el.value = "";
    const name = (await call("suggested_name", { clipId: id })).replace(/\.mp4$/, ` (${preset}).mp4`);
    const dest = await T.dialog.save({ defaultPath: name, filters: [{ name: "Video", extensions: ["mp4"] }] });
    if (dest) { toast(`Exporting for ${preset}…`); await call("export_clip", { clipId: id, preset, dest }); toast("Export finished"); T.opener.revealItemInDir(dest); }
  }
});
document.addEventListener("keydown", (ev) => {
  if (ev.key.toLowerCase() === "n" && !ev.ctrlKey && !ev.metaKey && !["INPUT", "TEXTAREA", "SELECT"].includes(ev.target.tagName) && !ev.target.isContentEditable && !S.editor) go("create");
  if (ev.key === "Enter" && ev.target.id === "f-url") $('[data-act="create"]').click();
  if (ev.key === "Enter" && ev.target.classList?.contains("word")) { ev.preventDefault(); ev.target.blur(); }
  if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "j" && S.editor) { ev.preventDefault(); $("#ai-prompt")?.focus(); }
  if (ev.key === "Escape" && S.editor) { S.editor = null; renderEditor(); }
});

T.event.listen("task-progress", ({ payload: p }) => {
  if (S.view === "task" && p.id === S.taskId && $("#t-bar")) {
    $("#t-bar").style.width = p.progress + "%"; $("#t-pct").textContent = p.progress + "%"; $("#t-msg").textContent = p.message;
    const idx = stageIndex(p.progress), shown = $$("#progress-card .step.active").length ? $$("#progress-card .step").findIndex((e) => e.classList.contains("active")) : -1;
    if (idx !== shown) { S.task.progress = p.progress; S.task.message = p.message; $("#progress-card").outerHTML = progressCard(S.task); }
  }
});
T.event.listen("task-update", async ({ payload: t }) => {
  if (S.view === "task" && t.id === S.taskId) {
    const hadClips = S.clips.length; S.task = t;
    if (!hadClips || t.clip_count !== hadClips) { await loadTask(); renderTask(); } else refreshHead();
    if (t.status === "completed") toast("Clips are ready");
  } else if (S.view === "library") { S.tasks = await invoke("list_tasks"); renderLibrary(); }
  else if (t.status === "completed") toast(`Clips are ready: ${t.title}`);
});
T.event.listen("publish-update", ({ payload: r }) => {
  const i = S.pub.history.findIndex((x) => x.id === r.id);
  if (i >= 0) S.pub.history[i] = r; else S.pub.history.unshift(r);
  const row = $(`[data-rid="${r.id}"]`);
  if (S.view === "publish") { if (row) row.outerHTML = receiptRow(r); else $("#receipts") && renderPublish(); }
  if (["published", "uploaded"].includes(r.status)) toast(`Published to ${PLATFORMS[r.provider]?.name || r.provider}`);
  if (r.status === "failed") toast(r.message, true);
});
T.event.listen("publish-auth", async ({ payload: p }) => {
  toast(p.message, !/connected/i.test(p.message) && !/Waiting/i.test(p.message));
  if (S.view === "publish") { await loadPublish(); renderPublish(); }
});
T.event.listen("chatgpt-auth", async ({ payload: p }) => {
  toast(p.message, !p.connected && !/Waiting/i.test(p.message) && !!p.message);
  S.chatgptModels = null;
  if (S.view === "settings") await loadChatgpt(true);
});
T.event.listen("clip-update", ({ payload: c }) => upsertClip(c));
T.event.listen("clip-progress", ({ payload: p }) => { const el = $(`[data-clip-pct="${p.id}"]`); if (el) el.textContent = `Rendering ${p.progress}%`; });
T.event.listen("tool-progress", ({ payload: p }) => {
  S.toolProgress[p.name] = p;
  if (S.view === "settings" && $("#tools")) $("#tools").innerHTML = S.tools.map(toolRow).join("");
});
T.event.listen("tauri://drag-drop", ({ payload }) => {
  const p = payload?.paths?.[0];
  if (p && S.view === "create") { S.form.path = p; S.form.url = ""; renderCreate(); }
});

$("#theme-toggle").addEventListener("click", toggleTheme);
(async function init() {
  paintIcons();
  [S.settings, S.templates, S.fonts] = await Promise.all([invoke("get_settings"), invoke("list_templates"), invoke("list_fonts")]);
  await refreshTools();
  go("create");
})();
