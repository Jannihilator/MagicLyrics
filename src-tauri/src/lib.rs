mod cache;
mod lyrics;
mod media;
mod overlay;
mod settings;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use cache::{Cached, LyricsCache};
use lyrics::{Lyrics, Outcome, Query};
use media::{Command, Track};
use overlay::Rect;
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position, Size, State,
    WebviewWindow, WindowEvent,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playback {
    playing: bool,
    position_ms: u64,
    /// Unix ms when `position_ms` was sampled; the UI extrapolates from here.
    sampled_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsState {
    /// "idle" | "loading" | "found" | "notFound" | "offline" | "notMusic"
    status: String,
    lyrics: Option<Lyrics>,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    track: Option<Track>,
    playback: Option<Playback>,
    art: Option<String>,
    lyrics: LyricsState,
}

pub struct AppState {
    ui: Mutex<UiState>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    settings_dirty: AtomicBool,
    media_tx: Mutex<mpsc::Sender<Command>>,
    controls_open: AtomicBool,
    hit_rects: Mutex<Vec<Rect>>,
    lyrics_gen: AtomicU64,
    http: reqwest::Client,
    cache: LyricsCache,
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn query_for(t: &Track) -> Query {
    Query { title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), duration_ms: t.duration_ms }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn get_state(state: State<AppState>) -> UiState {
    state.ui.lock().unwrap().clone()
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn update_settings(
    state: State<AppState>,
    show_art: Option<bool>,
    show_play_pause: Option<bool>,
    show_title: Option<bool>,
    font_size: Option<u32>,
) -> Settings {
    let mut s = state.settings.lock().unwrap();
    if let Some(v) = show_art {
        s.show_art = v;
    }
    if let Some(v) = show_play_pause {
        s.show_play_pause = v;
    }
    if let Some(v) = show_title {
        s.show_title = v;
    }
    if let Some(v) = font_size {
        s.font_size = v.clamp(10, 64);
    }
    state.settings_dirty.store(true, Ordering::Relaxed);
    s.clone()
}

#[tauri::command]
fn toggle_play_pause(state: State<AppState>) {
    let _ = state.media_tx.lock().unwrap().send(Command::TogglePlayPause);
}

#[tauri::command]
fn set_controls_open(state: State<AppState>, open: bool) {
    state.controls_open.store(open, Ordering::Relaxed);
}

#[tauri::command]
fn set_hit_rects(state: State<AppState>, rects: Vec<Rect>) {
    if std::env::var_os("MAGICLYRICS_DEBUG").is_some() {
        eprintln!("[hit] rects {rects:?}");
    }
    *state.hit_rects.lock().unwrap() = rects.into_iter().take(8).collect();
}

#[tauri::command]
fn refetch_lyrics(app: AppHandle, state: State<AppState>) {
    let track = state.ui.lock().unwrap().track.clone();
    if let Some(t) = track {
        state.cache.remove(&query_for(&t));
        start_lyrics(&app, t);
    }
}

/// Save a sync correction for the current song (kept with its cached lyrics).
#[tauri::command]
fn set_lyrics_offset(state: State<AppState>, offset_ms: i64) {
    let offset_ms = offset_ms.clamp(-10 * 60_000, 10 * 60_000);
    let mut ui = state.ui.lock().unwrap();
    let Some(track) = ui.track.clone() else { return };
    if let Some(l) = ui.lyrics.lyrics.as_mut() {
        l.offset_ms = offset_ms;
        state.cache.put(&query_for(&track), Some(l));
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    save_and_exit(&app);
}

fn save_and_exit(app: &AppHandle) {
    let st = app.state::<AppState>();
    let s = st.settings.lock().unwrap().clone();
    settings::save(&st.settings_path, &s);
    app.exit(0);
}

// ---------------------------------------------------------------- tray / menu bar

/// System-tray (Windows) / menu-bar (macOS) icon: always a way to hide or quit,
/// even when the overlay itself is off-screen or unresponsive to clicks.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;

    let toggle = MenuItem::with_id(app, "toggle", "Hide Lyrics", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit MagicLyrics", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &sep, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("MagicLyrics")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, ev| match ev.id.as_ref() {
            "toggle" => {
                if let Some(win) = app.get_webview_window("main") {
                    let visible = win.is_visible().unwrap_or(true);
                    let _ = if visible { win.hide() } else { win.show() };
                    let _ = toggle.set_text(if visible { "Show Lyrics" } else { "Hide Lyrics" });
                }
            }
            "quit" => save_and_exit(app),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

// ---------------------------------------------------------------- lyrics

fn set_lyrics(app: &AppHandle, gen: u64, status: &str, lyrics: Option<Lyrics>) {
    let st = app.state::<AppState>();
    if st.lyrics_gen.load(Ordering::SeqCst) != gen {
        return; // a newer track took over
    }
    let ls = LyricsState { status: status.into(), lyrics };
    st.ui.lock().unwrap().lyrics = ls.clone();
    let _ = app.emit("lyrics", ls);
}

fn start_lyrics(app: &AppHandle, track: Track) {
    let st = app.state::<AppState>();
    let gen = st.lyrics_gen.fetch_add(1, Ordering::SeqCst) + 1;
    if !track.music {
        return set_lyrics(app, gen, "notMusic", None);
    }
    let q = query_for(&track);
    match st.cache.get(&q) {
        Cached::Hit(l) => return set_lyrics(app, gen, "found", Some(l)),
        Cached::KnownMiss => return set_lyrics(app, gen, "notFound", None),
        Cached::Unknown => {}
    }
    set_lyrics(app, gen, "loading", None);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let st = app.state::<AppState>();
        let (status, l) = match lyrics::find(&st.http, &q).await {
            Outcome::Found(l) => {
                st.cache.put(&q, Some(&l));
                ("found", Some(l))
            }
            Outcome::NotFound => {
                st.cache.put(&q, None);
                ("notFound", None)
            }
            Outcome::Offline => ("offline", None),
        };
        set_lyrics(&app, gen, status, l);
    });
}

// ---------------------------------------------------------------- media worker

fn spawn_media_worker(app: AppHandle, rx: Receiver<Command>, preferred_app: String) {
    std::thread::spawn(move || {
        media::init_thread();
        let mut src = media::new_source(&preferred_app, app.path().resource_dir().ok());
        let mut cur_key = String::new();
        let mut stable_polls = 0u32;
        let mut lyrics_started = false;
        let mut art_tries = 0u32;
        let mut have_art = false;
        let mut last: Option<(bool, u64, Instant)> = None;

        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Command::TogglePlayPause) => {
                    if let Err(e) = src.toggle_play_pause() {
                        eprintln!("[media] play/pause failed: {e}");
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            let snap = src.snapshot().map(media::refine);
            let st = app.state::<AppState>();
            let key = snap.as_ref().map(|s| s.track.key()).unwrap_or_default();

            if key != cur_key {
                cur_key = key;
                stable_polls = 0;
                lyrics_started = false;
                art_tries = 0;
                have_art = false;
                last = None;
                let track = snap.as_ref().map(|s| s.track.clone());
                {
                    let mut ui = st.ui.lock().unwrap();
                    ui.track = track.clone();
                    ui.art = None;
                    ui.playback = None;
                    ui.lyrics = LyricsState { status: "idle".into(), lyrics: None };
                }
                if track.is_none() {
                    st.lyrics_gen.fetch_add(1, Ordering::SeqCst);
                }
                let _ = app.emit("track", track);
            } else {
                stable_polls += 1;
            }
            let Some(s) = snap else { continue };

            // Wait one extra poll so title and timeline (duration) both belong to the new song.
            if stable_polls >= 1 && !lyrics_started {
                lyrics_started = true;
                st.ui.lock().unwrap().track = Some(s.track.clone());
                let _ = app.emit("track", Some(s.track.clone()));
                start_lyrics(&app, s.track.clone());
            }

            if !have_art && art_tries < 8 {
                art_tries += 1;
                if let Some((bytes, mime)) = src.album_art() {
                    have_art = true;
                    let url = format!("data:{mime};base64,{}", B64.encode(bytes));
                    st.ui.lock().unwrap().art = Some(url.clone());
                    let _ = app.emit("art", url);
                }
            }

            // Only notify the UI on play/pause or seeks; it interpolates in between.
            let now = Instant::now();
            let changed = match last {
                None => true,
                Some((was_playing, pos, at)) => {
                    let predicted = if was_playing { pos + now.duration_since(at).as_millis() as u64 } else { pos };
                    was_playing != s.playing || (predicted as i64 - s.position_ms as i64).abs() > 750
                }
            };
            if changed {
                last = Some((s.playing, s.position_ms, now));
                let pb = Playback { playing: s.playing, position_ms: s.position_ms, sampled_at_ms: unix_ms() };
                st.ui.lock().unwrap().playback = Some(pb.clone());
                let _ = app.emit("playback", pb);
            }
        }
    });
}

// ---------------------------------------------------------------- window

/// Saved geometry is in logical points on macOS and physical pixels elsewhere.
/// On macOS tao's "physical" values each use a different display's scale factor,
/// so they don't line up across mixed-scale monitors; logical points (macOS's own
/// unit) do. Returns how many saved units one "physical" unit of `scale` is.
fn geom_div(scale: f64) -> f64 {
    if cfg!(target_os = "macos") { scale } else { 1.0 }
}

fn to_size(w: u32, h: u32) -> Size {
    if cfg!(target_os = "macos") { LogicalSize::new(w as f64, h as f64).into() } else { PhysicalSize::new(w, h).into() }
}

fn to_position(x: i32, y: i32) -> Position {
    if cfg!(target_os = "macos") { LogicalPosition::new(x as f64, y as f64).into() } else { PhysicalPosition::new(x, y).into() }
}

/// Restore saved size/position; returns false when a default position was used.
fn restore_geometry(win: &WebviewWindow, s: &Settings) -> bool {
    if let (Some(w), Some(h)) = (s.width, s.height) {
        if w >= 100 && h >= 50 {
            let _ = win.set_size(to_size(w, h));
        }
    }
    if let (Some(x), Some(y)) = (s.x, s.y) {
        // Only restore if that spot is still on a connected monitor.
        let on_screen = win.available_monitors().unwrap_or_default().iter().any(|m| {
            let k = geom_div(m.scale_factor());
            let (px, py) = (m.position().x as f64 / k, m.position().y as f64 / k);
            let (pw, ph) = (m.size().width as f64 / k, m.size().height as f64 / k);
            let (cx, cy) = ((x + 40) as f64, (y + 20) as f64);
            cx >= px && cy >= py && cx < px + pw && cy < py + ph
        });
        if on_screen {
            let _ = win.set_position(to_position(x, y));
            return true;
        }
    }
    // Default: bottom-right corner of the primary screen, above the taskbar.
    if let (Ok(Some(m)), Ok(size)) = (win.primary_monitor(), win.outer_size()) {
        let wa = m.work_area();
        let margin = (24.0 * m.scale_factor()) as i32;
        let x = wa.position.x + wa.size.width as i32 - size.width as i32 - margin;
        let y = wa.position.y + wa.size.height as i32 - size.height as i32 - margin;
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
    false
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // macOS: no Dock icon or app menu, like a menu-bar utility.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let settings_path = settings::path(&app.path().app_config_dir()?);
            let mut s = settings::load(&settings_path);
            if cfg!(target_os = "macos") && !s.geometry_logical {
                // Saved by an older build in mismatched units: start from the default spot once.
                (s.x, s.y, s.width, s.height) = (None, None, None, None);
                s.geometry_logical = true;
            }
            let cache = LyricsCache::new(app.path().app_cache_dir()?.join("lyrics"));
            let (tx, rx) = mpsc::channel();
            let preferred = s.preferred_app.clone();

            let win = app.get_webview_window("main").expect("main window");
            let restored = restore_geometry(&win, &s);
            if !restored {
                if let (Ok(p), Ok(sz), Ok(sf)) = (win.outer_position(), win.inner_size(), win.scale_factor()) {
                    let k = geom_div(sf);
                    s.x = Some((p.x as f64 / k).round() as i32);
                    s.y = Some((p.y as f64 / k).round() as i32);
                    s.width = Some((sz.width as f64 / k).round() as u32);
                    s.height = Some((sz.height as f64 / k).round() as u32);
                }
            }

            app.manage(AppState {
                ui: Mutex::new(UiState::default()),
                settings: Mutex::new(s),
                settings_path,
                // Persist the default placement so the window opens in the same spot next time.
                settings_dirty: AtomicBool::new(!restored),
                media_tx: Mutex::new(tx),
                controls_open: AtomicBool::new(false),
                hit_rects: Mutex::new(Vec::new()),
                lyrics_gen: AtomicU64::new(0),
                http: lyrics::http_client(),
                cache,
            });

            build_tray(app)?;
            overlay::harden(&win);
            // macOS: stay visible on every Space and over full-screen apps.
            #[cfg(target_os = "macos")]
            let _ = win.set_visible_on_all_workspaces(true);
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.show();

            overlay::spawn_hit_tester(app.handle().clone());
            spawn_media_worker(app.handle().clone(), rx, preferred);
            Ok(())
        })
        .on_window_event(|win, ev| {
            let Some(st) = win.try_state::<AppState>() else { return };
            match ev {
                WindowEvent::Moved(p) => {
                    let k = geom_div(win.scale_factor().unwrap_or(1.0));
                    let mut s = st.settings.lock().unwrap();
                    s.x = Some((p.x as f64 / k).round() as i32);
                    s.y = Some((p.y as f64 / k).round() as i32);
                    st.settings_dirty.store(true, Ordering::Relaxed);
                }
                WindowEvent::Resized(sz) if sz.width > 0 && sz.height > 0 => {
                    let k = geom_div(win.scale_factor().unwrap_or(1.0));
                    let mut s = st.settings.lock().unwrap();
                    s.width = Some((sz.width as f64 / k).round() as u32);
                    s.height = Some((sz.height as f64 / k).round() as u32);
                    st.settings_dirty.store(true, Ordering::Relaxed);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_settings,
            update_settings,
            toggle_play_pause,
            set_controls_open,
            set_hit_rects,
            refetch_lyrics,
            set_lyrics_offset,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("error while running MagicLyrics");
}
