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
  backdropLuma: null, // brightness 0..1 of what's behind the overlay (Windows); null = unknown
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

const fmtOffset = (ms) => `${ms > 0 ? "+" : ms < 0 ? "−" : "±"}${(Math.abs(ms) / 1000).toFixed(1)}s`;

function renderMeta() {
  const t = state.track;
  const l = state.lyrics.lyrics;
  // Controls bar: only the sync correction (if any); lyrics source goes in the tooltip.
  $("meta").textContent = l?.offsetMs ? `sync ${fmtOffset(l.offsetMs)}` : "";
  $("syncOffset").textContent = fmtOffset(l?.offsetMs || 0);
  $("sync").disabled = !l || l.lines.length === 0 || l.instrumental;
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
  setSyncing(false);
  setY(0);

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
  if (syncing) return; // lyrics stay put while syncing; only the highlight moves
  setY(centeredY(el));
}

let curY = 0;
function setY(y) {
  curY = Math.round(y);
  linesEl.style.transform = `translateY(${curY}px)`;
}
/** translateY that puts `el` in the middle of the viewport. */
const centeredY = (el) => viewport.clientHeight / 2 - (el.offsetTop + el.offsetHeight / 2);

function renderPlayButton() {
  pp.innerHTML = state.playback?.playing ? ICON_PAUSE : ICON_PLAY;
}

const THEME_LABEL = { auto: "Auto", light: "Light", dark: "Dark" };
// Panel background alpha. In-between values looked muddy, so just three.
const OPACITY = { low: 0.2, medium: 0.8, high: 1 };
// Accent swatches: [color, name, ink for text/check marks drawn on top of it].
const ACCENTS = [
  ["#1ed760", "Green", "#0b0b0d"],
  ["#ff3b30", "Red", "#fff"],
  ["#0a84ff", "Blue", "#fff"],
  ["#bf5af2", "Purple", "#fff"],
  ["#ff375f", "Pink", "#fff"],
  ["#ff9f0a", "Orange", "#0b0b0d"],
  ["#ffd60a", "Yellow", "#0b0b0d"],
  ["#40c8e0", "Teal", "#0b0b0d"],
];

let curOpacity = "medium";
const PANEL_LUMA = { dark: 0.05, light: 0.96 };

/**
 * Panel color: the Color setting, or in Auto a light panel over dark backgrounds.
 * Text color: whatever reads best on the panel blended with the backdrop that shows
 * through it (at low opacity that's mostly the backdrop). Hysteresis avoids flicker.
 */
function applyTone() {
  const theme = state.settings?.theme || "auto";
  const b = state.backdropLuma;
  const light = theme === "auto"
    ? b != null && (app.classList.contains("light") ? b < 0.45 : b < 0.35)
    : theme === "light";
  app.classList.toggle("light", light);

  const a = OPACITY[curOpacity] ?? OPACITY.medium;
  const panel = light ? PANEL_LUMA.light : PANEL_LUMA.dark;
  const seen = b == null ? panel : a * panel + (1 - a) * b;
  app.classList.toggle("ink", app.classList.contains("ink") ? seen > 0.45 : seen > 0.55);

  $("themeLabel").textContent = THEME_LABEL[theme] || THEME_LABEL.auto;
  for (const btn of $("themeMenu").children) btn.classList.toggle("sel", btn.dataset.v === theme);
}

// ------------------------------------------------------------------ sliders

/** Snap-point ticks under a range input, and the filled part of its track. */
function initSlider(input) {
  const { min, max, step } = input;
  const ticks = input.nextElementSibling;
  for (let v = +min; v <= +max; v += +step) {
    const i = document.createElement("i");
    i.style.left = `${((v - min) / (max - min)) * 100}%`;
    ticks.appendChild(i);
  }
}
function setSlider(input, value, label) {
  input.value = value;
  input.style.setProperty("--p", `${((input.value - input.min) / (input.max - input.min)) * 100}%`);
  input.closest(".slider").querySelector("output").textContent = label;
}
const fontLabel = (v) => `${v}px`;

function applyFontSize(v) {
  document.documentElement.style.setProperty("--fs", `${v}px`);
  setSlider($("fontSize"), v, fontLabel(v));
}
function applyOpacity(level) {
  curOpacity = level in OPACITY ? level : "medium";
  document.documentElement.style.setProperty("--bg-a", String(OPACITY[curOpacity]));
  for (const b of $("opacity").children) b.classList.toggle("sel", b.dataset.v === curOpacity);
  applyTone();
}
function applyAccent(color) {
  const [c, , ink] = ACCENTS.find(([c]) => c === color) || [color, "", "#0b0b0d"];
  document.documentElement.style.setProperty("--accent", c);
  document.documentElement.style.setProperty("--on-accent", ink);
  for (const b of $("accent").children) b.classList.toggle("sel", b.dataset.v === c);
}

function applySettings(s) {
  state.settings = s;
  app.classList.toggle("hide-art", !s.showArt);
  app.classList.toggle("hide-pp", !s.showPlayPause);
  app.classList.toggle("hide-title", !s.showTitle);
  applyFontSize(s.fontSize);
  applyOpacity(s.panelOpacity);
  applyAccent(s.accent);
  $("optArt").checked = s.showArt;
  $("optTitle").checked = s.showTitle;
  $("optPP").checked = s.showPlayPause;
  applyTone();
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
/** The whole window takes clicks while the controls are open or lyrics are being synced. */
const updateInteractive = () => invoke("set_controls_open", { open: state.open || syncing });

function setOpen(open) {
  state.open = open;
  app.classList.toggle("open", open);
  if (!open) setMenuOpen(false);
  $("handle").title = open ? "Hide controls" : "Show controls";
  updateInteractive();
  clearTimeout(autoCloseTimer);
}

// Don't leave the window blocking clicks if the controls (or sync) are forgotten.
document.addEventListener("mouseleave", () => {
  if (!state.open && !syncing) return;
  clearTimeout(autoCloseTimer);
  autoCloseTimer = setTimeout(() => { setOpen(false); setSyncing(false); }, state.open ? 6000 : 30000);
});

// Clicking anywhere outside the controls closes them.
document.addEventListener("pointerdown", (e) => {
  if (state.open && !e.target.closest("#controls, #handle, #grip, .edge")) setOpen(false);
}, true);
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

// ------------------------------------------------------------------ sync mode
// The lyrics stop following the song (the sung line stays highlighted) and can be
// scrolled with the wheel or by dragging. Clicking a line says "this is being sung
// now"; click again to fine-tune, then Done.

let syncing = false;
function setSyncing(on) {
  if (on === syncing) return;
  syncing = on;
  app.classList.toggle("syncing", on);
  if (on) setOpen(false);
  else { clearTimeout(autoCloseTimer); tick(true); }
  updateInteractive();
}

function scrollLyrics(y) {
  if (!lineEls.length) return;
  const first = centeredY(lineEls[0]), last = centeredY(lineEls[lineEls.length - 1]);
  setY(Math.max(last, Math.min(first, y)));
}

function setOffset(offsetMs) {
  const l = state.lyrics.lyrics;
  if (!l) return;
  l.offsetMs = offsetMs;
  invoke("set_lyrics_offset", { offsetMs });
  renderMeta();
  tick(true);
}
/** "Line `idx` is being sung right now." */
function syncTo(idx) {
  if (!state.lyrics.lyrics || !state.playback) return;
  setOffset(Math.round(lineStartMs(idx) - rawPositionMs()));
}

viewport.addEventListener("wheel", (e) => {
  if (!syncing) return;
  e.preventDefault();
  scrollLyrics(curY - e.deltaY);
}, { passive: false });

let lyricPress = null;
viewport.addEventListener("pointerdown", (e) => {
  if (!syncing || e.button !== 0) return;
  lyricPress = { y: e.clientY, startY: curY, dragged: false };
  viewport.setPointerCapture(e.pointerId);
});
viewport.addEventListener("pointermove", (e) => {
  if (!lyricPress) return;
  if (!lyricPress.dragged && Math.abs(e.clientY - lyricPress.y) > 4) lyricPress.dragged = true;
  if (lyricPress.dragged) scrollLyrics(lyricPress.startY + (e.clientY - lyricPress.y));
});
viewport.addEventListener("pointerup", (e) => {
  const p = lyricPress;
  lyricPress = null;
  if (!p || p.dragged) return;
  // A click, not a drag: sync to the line under the pointer.
  const el = document.elementsFromPoint(e.clientX, e.clientY).find((n) => n.classList?.contains("line"));
  if (el) syncTo(Number(el.dataset.idx));
});
viewport.addEventListener("pointercancel", () => { lyricPress = null; });

$("sync").addEventListener("click", () => setSyncing(true));
$("syncDone").addEventListener("click", () => setSyncing(false));
$("syncReset").addEventListener("click", () => setOffset(0));

pp.addEventListener("click", () => invoke("toggle_play_pause"));
$("optArt").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showArt: e.target.checked })));
$("optTitle").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showTitle: e.target.checked })));
$("optPP").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { showPlayPause: e.target.checked })));
// Sliders preview live while dragging and save when released.
initSlider($("fontSize"));
$("fontSize").addEventListener("input", (e) => { applyFontSize(+e.target.value); tick(true); peek(); });
// Peek: hide everything but the size slider while it's in use, so the lyrics show
// at the new size. Ends shortly after release (or after the last key/wheel step).
let peekHeld = false, peekTimer = 0;
function peek() {
  clearTimeout(peekTimer);
  app.classList.add("peek");
  if (!peekHeld) peekTimer = setTimeout(() => app.classList.remove("peek"), 800);
}
$("fontSize").addEventListener("pointerdown", () => { peekHeld = true; peek(); });
for (const t of ["pointerup", "pointercancel"]) document.addEventListener(t, () => {
  if (!peekHeld) return;
  peekHeld = false;
  peek();
});
$("fontSize").addEventListener("change", async (e) =>
  applySettings(await invoke("update_settings", { fontSize: +e.target.value })));
$("opacity").addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-v]");
  if (b) applySettings(await invoke("update_settings", { panelOpacity: b.dataset.v }));
});
for (const [c, name] of ACCENTS) {
  const b = document.createElement("button");
  b.dataset.v = c;
  b.title = name;
  b.setAttribute("aria-label", name);
  b.style.setProperty("--c", c);
  $("accent").appendChild(b);
}
$("accent").addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-v]");
  if (b) applySettings(await invoke("update_settings", { accent: b.dataset.v }));
});

// Color dropdown.
const themeMenu = $("themeMenu");
function setMenuOpen(open) {
  themeMenu.hidden = !open;
  if (!open) return;
  const r = $("themeBtn").getBoundingClientRect();
  const below = r.bottom + 3 + themeMenu.offsetHeight <= innerHeight;
  themeMenu.style.left = `${Math.min(r.left, innerWidth - themeMenu.offsetWidth - 4)}px`;
  themeMenu.style.top = `${below ? r.bottom + 3 : Math.max(4, r.top - 3 - themeMenu.offsetHeight)}px`;
}
$("themeBtn").addEventListener("click", (e) => { e.stopPropagation(); setMenuOpen(themeMenu.hidden); });
themeMenu.addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-v]");
  if (!b) return;
  setMenuOpen(false);
  applySettings(await invoke("update_settings", { theme: b.dataset.v }));
});
document.addEventListener("click", () => setMenuOpen(false));
$("controls").addEventListener("scroll", () => setMenuOpen(false));

// ------------------------------------------------------------------ updates
// The Rust side checks GitHub in the background and owns the status; this only shows it.

let update = { state: "idle", current: "", available: null, progress: null, error: null };
let upToDateTimer = 0;
function renderUpdate(u) {
  update = u;
  const btn = $("update");
  const pct = u.progress != null ? ` ${Math.round(u.progress * 100)}%` : "";
  const [text, title] = {
    checking: ["⬆ Checking…", "Looking for a newer version"],
    upToDate: ["✓ Up to date", `v${u.current} is the latest version`],
    available: [`⬆ Update to v${u.available}`, `Download and install v${u.available}, then restart (now v${u.current})`],
    downloading: [`⬆ Updating…${pct}`, "Downloading the update; the app restarts when done"],
    error: ["⬆ Update failed — retry", `Couldn't update: ${u.error || "unknown error"}`],
  }[u.state] || ["⬆ Check for updates", `Check for a newer version (now v${u.current})`];
  btn.textContent = text;
  btn.title = title;
  const note = $("updateNote");
  note.hidden = u.state !== "error";
  note.textContent = u.state === "error" ? u.error || "Unknown error" : "";
  btn.disabled = u.state === "checking" || u.state === "downloading";
  btn.classList.toggle("primary", u.state === "available");
  app.classList.toggle("has-update", u.state === "available");
  // "Up to date" is a passing confirmation, not a permanent label.
  clearTimeout(upToDateTimer);
  if (u.state === "upToDate") upToDateTimer = setTimeout(() => renderUpdate({ ...update, state: "idle" }), 4000);
}
$("update").addEventListener("click", () =>
  invoke(update.state === "available" ? "install_update" : "check_update"));

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
  state.backdropLuma = s.backdropLuma ?? null;
  applyTone();
  renderLyrics();
  renderPlayButton();

  renderUpdate(await invoke("get_update_status"));
  await listen("update", (e) => renderUpdate(e.payload));
  await listen("track", (e) => setTrack(e.payload));
  await listen("backdrop", (e) => { state.backdropLuma = e.payload; applyTone(); });
  await listen("art", (e) => { art.src = e.payload; });
  await listen("lyrics", (e) => { state.lyrics = e.payload; renderLyrics(); });
  await listen("playback", (e) => { state.playback = e.payload; renderPlayButton(); tick(); });

  reportHandleRect();
  window.addEventListener("resize", () => { reportHandleRect(); if (syncing) scrollLyrics(curY); else tick(true); });
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
