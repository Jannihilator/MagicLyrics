"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);
const app = $("app");
const linesEl = $("lines");
const statusEl = $("status");
const viewport = $("viewport");
const art = $("art");
const pp = $("pp");

const ICON_PLAY = '<svg viewBox="0 0 16 16"><path d="M4 2.5v11l9-5.5z"/></svg>';
const ICON_PAUSE = '<svg viewBox="0 0 16 16"><path d="M3.5 2.5h3v11h-3zM9.5 2.5h3v11h-3z"/></svg>';
const LEAD_MS = 150; // show a line slightly before it is sung

const state = {
  track: null,
  trackKey: "",
  playback: null,
  lyrics: { status: "idle", lyrics: null },
  settings: null,
  open: false,
};
let lineEls = [];
let timed = []; // [{t, el}] for synced lyrics
let activeIdx = -2;

const keyOf = (t) => (t ? `${t.artist}\u001f${t.title}\u001f${t.album}` : "");

/** Player position as reported by the media session. */
function rawPositionMs() {
  const p = state.playback;
  if (!p) return 0;
  return p.playing ? p.positionMs + (Date.now() - p.sampledAtMs) : p.positionMs;
}

/** Position on the lyrics timeline (player position + this song's sync offset). */
function positionMs() {
  return rawPositionMs() + (state.lyrics.lyrics?.offsetMs || 0);
}

/** Start time of line `idx` on the lyrics timeline (estimated for unsynced lyrics). */
function lineStartMs(idx) {
  const l = state.lyrics.lyrics;
  if (l.synced) return timed[idx].t;
  const dur = state.track?.durationMs || 0;
  return Math.round((idx / lineEls.length) * dur);
}

// ------------------------------------------------------------------ rendering

function setStatus(text) {
  statusEl.textContent = text || "";
}

function renderMeta() {
  const t = state.track;
  const l = state.lyrics.lyrics;
  // Controls bar: only the sync correction (if any); lyrics source goes in the tooltip.
  $("meta").textContent = l?.offsetMs
    ? `sync ${l.offsetMs > 0 ? "+" : "−"}${(Math.abs(l.offsetMs) / 1000).toFixed(1)}s`
    : "";
  $("drag").title = "Drag to move" + (l ? ` · lyrics: ${l.source}${l.synced ? "" : " (unsynced)"}` : "");
  $("songTitle").textContent = t?.title || "";
  $("songArtist").textContent = t?.artist || "";
  app.classList.toggle("idle", !t);
}

function renderLyrics() {
  linesEl.textContent = "";
  lineEls = [];
  timed = [];
  activeIdx = -2;
  linesEl.style.transform = "translateY(0)";

  const { status, lyrics } = state.lyrics;
  renderMeta();

  if (!state.track) return setStatus("Waiting for music…");
  if (status === "loading" || status === "idle") return setStatus("Searching lyrics…");
  if (status === "notFound") return setStatus("No lyrics found");
  if (status === "notMusic") return setStatus("Not detected as a song");
  if (status === "offline") return setStatus("Offline — lyrics not cached yet");
  if (!lyrics) return setStatus("");
  if (lyrics.instrumental || lyrics.lines.length === 0) return setStatus("♪ Instrumental ♪");

  setStatus("");
  linesEl.classList.toggle("unsynced", !lyrics.synced);
  const frag = document.createDocumentFragment();
  for (const line of lyrics.lines) {
    const el = document.createElement("div");
    el.className = "line";
    el.dataset.idx = String(lineEls.length);
    if (!line.text) {
      el.textContent = lyrics.synced ? "♪" : " ";
      el.classList.add("gap");
    } else {
      el.textContent = line.text;
    }
    frag.appendChild(el);
    lineEls.push(el);
    if (lyrics.synced) timed.push({ t: line.timeMs, el });
  }
  linesEl.appendChild(frag);
  tick(true);
}

function currentIndex() {
  const pos = positionMs();
  const l = state.lyrics.lyrics;
  if (!l || lineEls.length === 0) return -1;
  if (l.synced) {
    // Last line whose start time has passed (binary search).
    let lo = 0, hi = timed.length - 1, ans = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (timed[mid].t <= pos + LEAD_MS) { ans = mid; lo = mid + 1; } else hi = mid - 1;
    }
    return ans;
  }
  // Unsynced: spread lines evenly over the song as a rough guide.
  const dur = state.track?.durationMs || 0;
  if (!dur) return 0;
  return Math.min(lineEls.length - 1, Math.floor((pos / dur) * lineEls.length));
}

function tick(force = false) {
  const idx = currentIndex();
  if (idx === activeIdx && !force) return;
  if (lineEls[activeIdx]) lineEls[activeIdx].classList.remove("active");
  activeIdx = idx;
  const el = lineEls[idx] || lineEls[0];
  if (!el) return;
  if (idx >= 0) el.classList.add("active");
  const center = el.offsetTop + el.offsetHeight / 2;
  linesEl.style.transform = `translateY(${Math.round(viewport.clientHeight / 2 - center)}px)`;
}

function renderPlayButton() {
  pp.innerHTML = state.playback?.playing ? ICON_PAUSE : ICON_PLAY;
}

function applySettings(s) {
  state.settings = s;
  app.classList.toggle("hide-art", !s.showArt);
  app.classList.toggle("hide-pp", !s.showPlayPause);
  app.classList.toggle("hide-title", !s.showTitle);
  document.documentElement.style.setProperty("--fs", `${s.fontSize}px`);
  $("optArt").checked = s.showArt;
  $("optTitle").checked = s.showTitle;
  $("optPP").checked = s.showPlayPause;
  requestAnimationFrame(() => { tick(true); reportHandleRect(); });
}

// ------------------------------------------------------------------ click-through

// Sent in physical pixels: WebView2's devicePixelRatio includes Windows text
// scaling, so it can differ from the window's scale factor.
// While the handle is pressed the whole window stays clickable, so a quick drag
// that leaves the small handle doesn't fall through to the app underneath.
// The play button, when shown, is clickable too (no need to open the controls).
let handlePressed = false;
function reportHandleRect() {
  const boxes = handlePressed
    ? [{ left: 0, top: 0, width: innerWidth, height: innerHeight }]
    : [$("handle"), pp].filter((el) => el.offsetParent !== null).map((el) => el.getBoundingClientRect());
  const pad = 3;
  const k = window.devicePixelRatio || 1;
  invoke("set_hit_rects", {
    rects: boxes.map((r) => ({
      x: (r.left - pad) * k, y: (r.top - pad) * k, w: (r.width + pad * 2) * k, h: (r.height + pad * 2) * k,
    })),
  });
}

let autoCloseTimer = null;
function setOpen(open) {
  state.open = open;
  app.classList.toggle("open", open);
  $("handle").title = open ? "Hide controls" : "Show controls";
  invoke("set_controls_open", { open });
  clearTimeout(autoCloseTimer);
}

// Don't leave the window blocking clicks if the controls are forgotten open.
document.addEventListener("mouseleave", () => {
  if (!state.open) return;
  clearTimeout(autoCloseTimer);
  autoCloseTimer = setTimeout(() => setOpen(false), 6000);
});
document.addEventListener("mouseenter", () => clearTimeout(autoCloseTimer));

// ------------------------------------------------------------------ controls

// Handle: click toggles the controls; press-and-drag moves the window (controls stay as they are).
const handle = $("handle");
let press = null;
function setHandlePressed(on) {
  if (handlePressed === on) return;
  handlePressed = on;
  reportHandleRect();
}
handle.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  press = { x: e.screenX, y: e.screenY, dragged: false };
  setHandlePressed(true);
});
document.addEventListener("pointermove", (e) => {
  if (!press || press.dragged) return;
  if (Math.hypot(e.screenX - press.x, e.screenY - press.y) > 3) {
    press.dragged = true;
    setHandlePressed(false);
    appWindow.startDragging(); // OS takes over the drag; the click that follows is ignored
  }
});
document.addEventListener("pointerup", () => {
  setHandlePressed(false);
  if (press?.dragged) press = null;
});
handle.addEventListener("click", () => {
  const wasDrag = press?.dragged;
  press = null;
  if (!wasDrag) setOpen(!state.open);
});

// Re-sync: with controls open, click the line that is being sung right now.
linesEl.addEventListener("click", (e) => {
  const el = e.target.closest(".line");
  const l = state.lyrics.lyrics;
  if (!state.open || !el || !l || !state.playback) return;
  const offsetMs = Math.round(lineStartMs(Number(el.dataset.idx)) - rawPositionMs());
  l.offsetMs = offsetMs;
  invoke("set_lyrics_offset", { offsetMs });
  renderMeta();
  tick(true);
});

pp.addEventListener("click", () => invoke("toggle_play_pause"));
$("optArt").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showArt: e.target.checked })));
$("optTitle").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showTitle: e.target.checked })));
$("optPP").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showPlayPause: e.target.checked })));
$("fontDown").addEventListener("click", async () =>
  applySettings(await invoke("update_settings", { fontSize: state.settings.fontSize - 2 })));
$("fontUp").addEventListener("click", async () =>
  applySettings(await invoke("update_settings", { fontSize: state.settings.fontSize + 2 })));
$("refetch").addEventListener("click", () => invoke("refetch_lyrics"));
$("quit").addEventListener("click", () => invoke("quit"));
$("drag").addEventListener("mousedown", (e) => e.button === 0 && appWindow.startDragging());
$("grip").addEventListener("mousedown", (e) => e.button === 0 && appWindow.startResizeDragging("SouthEast"));
for (const edge of document.querySelectorAll(".edge")) {
  edge.addEventListener("mousedown", (e) => e.button === 0 && appWindow.startResizeDragging(edge.dataset.dir));
}
document.addEventListener("contextmenu", (e) => e.preventDefault());

// ------------------------------------------------------------------ events

function setTrack(t) {
  const key = keyOf(t);
  if (key !== state.trackKey) {
    state.trackKey = key;
    state.playback = null;
    state.lyrics = { status: "idle", lyrics: null };
    art.removeAttribute("src");
    state.track = t;
    renderLyrics();
    renderPlayButton();
  } else {
    state.track = t; // same song, refreshed details (e.g. duration)
    renderMeta();
  }
}

async function init() {
  applySettings(await invoke("get_settings"));
  const s = await invoke("get_state");
  setTrack(s.track);
  state.playback = s.playback;
  if (s.art) art.src = s.art;
  state.lyrics = s.lyrics;
  renderLyrics();
  renderPlayButton();

  await listen("track", (e) => setTrack(e.payload));
  await listen("art", (e) => { art.src = e.payload; });
  await listen("lyrics", (e) => { state.lyrics = e.payload; renderLyrics(); });
  await listen("playback", (e) => { state.playback = e.payload; renderPlayButton(); tick(); });

  reportHandleRect();
  window.addEventListener("resize", () => { reportHandleRect(); tick(true); });
  // The play button moves when the title wraps or art/title/play are toggled.
  new ResizeObserver(() => reportHandleRect()).observe($("side"));
  new ResizeObserver(() => reportHandleRect()).observe($("song"));
  // Moving to a monitor with different scaling changes devicePixelRatio.
  const watchDpr = () => matchMedia(`(resolution: ${devicePixelRatio}dppx)`)
    .addEventListener("change", () => { reportHandleRect(); watchDpr(); }, { once: true });
  watchDpr();
  // Cheap: only re-renders when the active line changes.
  setInterval(() => { if (state.playback?.playing && lineEls.length) tick(); }, 100);
}

init();
