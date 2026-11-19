"use strict";
// Loaded only when the interface is opened from a phone or another browser (not inside the desktop window).
// It gives app.js the same few functions the desktop window provides, but backed by the PC over HTTP.
(() => {
  if (window.__TAURI__) return;
  document.documentElement.classList.add("remote");

  const b64url = (s) => btoa(unescape(encodeURIComponent(s))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const clean = (s) => String(s || "file").split(/[\\/]/).pop().replace(/[^\p{L}\p{N} ._()-]/gu, "_").slice(0, 120) || "file";

  async function invoke(cmd, args) {
    let r;
    try {
      r = await fetch("/api/" + cmd, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(args || {}), credentials: "same-origin" });
    } catch (e) {
      window.dispatchEvent(new Event("nx-offline"));
      throw "Can't reach your PC. Is it on and connected?";
    }
    if (r.status === 401) {
      document.body.innerHTML = "<p style='padding:28px;font-size:18px'>This phone is not paired. On your PC open Settings, Remote access, and scan the QR code.</p>";
      throw "Not paired";
    }
    const data = await r.json().catch(() => null);
    if (!r.ok) throw (data && data.error) || "Request failed (" + r.status + ")";
    return data;
  }

  const handlers = {};
  const es = new EventSource("/events");
  let opened = false;
  es.onopen = () => { if (opened) window.dispatchEvent(new Event("nx-online")); opened = true; };
  es.onerror = () => window.dispatchEvent(new Event("nx-offline"));
  es.onmessage = (m) => {
    try {
      const { event, payload } = JSON.parse(m.data);
      (handlers[event] || []).forEach((f) => f({ payload }));
    } catch (e) { /* ignore a bad frame */ }
  };

  function uploadBar() {
    const el = document.createElement("div");
    el.style.cssText = "position:fixed;left:12px;right:12px;bottom:84px;z-index:50;padding:12px 14px;border-radius:12px;background:var(--card,#222);border:1px solid var(--border,#444);color:var(--foreground,#fff);font-size:14px;box-shadow:0 10px 30px rgba(0,0,0,.4)";
    el.innerHTML = "<div id='up-t'>Uploading to your PC…</div><div style='height:6px;border-radius:9px;background:rgba(127,127,127,.3);margin-top:8px;overflow:hidden'><i id='up-b' style='display:block;height:100%;width:0;background:var(--brand,#f60)'></i></div>";
    document.body.appendChild(el);
    return el;
  }

  function upload(file) {
    return new Promise((resolve, reject) => {
      const bar = uploadBar();
      const xhr = new XMLHttpRequest();
      xhr.open("POST", "/upload?name=" + encodeURIComponent(clean(file.name)));
      xhr.upload.onprogress = (e) => {
        if (!e.lengthComputable) return;
        const pct = Math.round((e.loaded / e.total) * 100);
        bar.querySelector("#up-b").style.width = pct + "%";
        bar.querySelector("#up-t").textContent = "Uploading to your PC… " + pct + "%";
      };
      xhr.onload = () => {
        bar.remove();
        let data = null;
        try { data = JSON.parse(xhr.responseText); } catch (e) { /* not JSON */ }
        if (xhr.status >= 200 && xhr.status < 300 && typeof data === "string") resolve(data);
        else reject((data && data.error) || "Upload failed (" + xhr.status + ")");
      };
      xhr.onerror = () => { bar.remove(); reject("Upload failed. Check the connection to your PC."); };
      xhr.send(file);
    });
  }

  function pickFile(options) {
    return new Promise((resolve) => {
      const input = document.createElement("input");
      input.type = "file";
      const exts = ((options && options.filters) || []).flatMap((f) => f.extensions || []).map((e) => "." + e);
      if (exts.length) input.accept = exts.join(",");
      input.onchange = async () => {
        const f = input.files && input.files[0];
        if (!f) return resolve(null);
        try { resolve(await upload(f)); } catch (err) {
          alert(typeof err === "string" ? err : "Upload failed");
          resolve(null);
        }
      };
      input.click();
    });
  }

  window.__TAURI__ = {
    remote: true,
    core: { invoke, convertFileSrc: (p) => "/f/" + b64url(p) },
    event: {
      listen: async (name, fn) => {
        (handlers[name] = handlers[name] || []).push(fn);
        return () => { handlers[name] = (handlers[name] || []).filter((f) => f !== fn); };
      },
    },
    dialog: {
      open: (options) => pickFile(options),
      // Saving on the phone means: the PC writes the file, then the phone downloads it.
      save: async (options) => {
        const id = (crypto.randomUUID ? crypto.randomUUID() : String(Date.now()) + Math.random().toString(16).slice(2)).slice(0, 36);
        return "__remote__/" + id + "/" + clean(options && options.defaultPath);
      },
    },
    opener: {
      openUrl: (u) => window.open(u, "_blank", "noopener"),
      revealItemInDir: (p) => {
        const m = /^__remote__\/([^/]+)\/(.+)$/.exec(String(p));
        if (m) location.href = "/dl/" + m[1] + "/" + encodeURIComponent(m[2]);
      },
    },
  };
})();
