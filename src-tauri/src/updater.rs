//! In-app updates from GitHub Releases (tauri-plugin-updater).
//!
//! CI publishes `latest.json` with each release; it lists the new version and a
//! download per platform, each signed with the project's updater key. The app only
//! installs files whose signature matches the public key in `tauri.conf.json`.
//!
//! Runs entirely on the Rust side: the UI calls `check_update` / `install_update`
//! and listens for `update` events, so the web layer gets no updater permissions.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager, State, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

/// How often to look for a new release while the app is running.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// "idle" | "checking" | "upToDate" | "available" | "downloading" | "error"
    state: String,
    /// Version running now.
    current: String,
    /// Newer version found, if any.
    available: Option<String>,
    /// Download progress 0..1 while "downloading" (None if the size is unknown).
    progress: Option<f64>,
    error: Option<String>,
}

pub struct Updates {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Update>>,
    busy: AtomicBool,
    tray_item: Mutex<Option<MenuItem<Wry>>>,
}

impl Updates {
    pub fn new(current: String) -> Self {
        Self {
            status: Mutex::new(UpdateStatus { state: "idle".into(), current, ..Default::default() }),
            pending: Mutex::new(None),
            busy: AtomicBool::new(false),
            tray_item: Mutex::new(None),
        }
    }

    pub fn set_tray_item(&self, item: MenuItem<Wry>) {
        *self.tray_item.lock().unwrap() = Some(item);
    }
}

fn tray_label(s: &UpdateStatus) -> (String, bool) {
    match s.state.as_str() {
        "checking" => ("Checking for Updates…".into(), false),
        "upToDate" => (format!("Up to Date (v{})", s.current), true),
        "available" => (format!("Install Update v{}", s.available.as_deref().unwrap_or("?")), true),
        "downloading" => ("Downloading Update…".into(), false),
        "error" => ("Update Failed — Try Again".into(), true),
        _ => ("Check for Updates…".into(), true),
    }
}

/// Change the status, then refresh the tray item and tell the UI.
fn set(app: &AppHandle, f: impl FnOnce(&mut UpdateStatus)) {
    let u = app.state::<Updates>();
    let s = {
        let mut s = u.status.lock().unwrap();
        f(&mut s);
        s.clone()
    };
    if let Some(item) = u.tray_item.lock().unwrap().as_ref() {
        let (text, enabled) = tray_label(&s);
        let _ = item.set_text(text);
        let _ = item.set_enabled(enabled);
    }
    let _ = app.emit("update", s);
}

/// Look for a newer release. `quiet`: background check, so a failure (offline,
/// GitHub down) leaves the status alone instead of showing an error.
pub async fn check(app: &AppHandle, quiet: bool) {
    let u = app.state::<Updates>();
    if u.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    if !quiet {
        set(app, |s| {
            s.state = "checking".into();
            s.error = None;
        });
    }
    let result = match app.updater() {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(Some(update)) => {
            let version = update.version.clone();
            eprintln!("[update] v{version} available");
            *u.pending.lock().unwrap() = Some(update);
            set(app, |s| {
                s.state = "available".into();
                s.available = Some(version);
                s.error = None;
            });
        }
        Ok(None) => {
            *u.pending.lock().unwrap() = None;
            if !quiet {
                set(app, |s| {
                    s.state = "upToDate".into();
                    s.available = None;
                });
            }
        }
        Err(e) => {
            eprintln!("[update] check failed: {e}");
            if !quiet {
                set(app, |s| {
                    s.state = "error".into();
                    s.error = Some(e);
                });
            }
        }
    }
    u.busy.store(false, Ordering::SeqCst);
}

/// Download, verify and install the pending update, then restart.
/// Windows: the installer closes the app itself and starts the new version.
pub async fn install(app: &AppHandle) {
    let u = app.state::<Updates>();
    let Some(update) = u.pending.lock().unwrap().clone() else {
        return check(app, false).await;
    };
    if u.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    set(app, |s| {
        s.state = "downloading".into();
        s.progress = Some(0.0);
        s.error = None;
    });
    crate::save_settings(app); // Windows exits inside download_and_install

    let mut done: u64 = 0;
    let mut last_pct: i64 = -1;
    let progress_app = app.clone();
    let result = update
        .download_and_install(
            move |chunk, total| {
                done += chunk as u64;
                let p = total.filter(|t| *t > 0).map(|t| (done as f64 / t as f64).min(1.0));
                let pct = p.map(|p| (p * 100.0) as i64).unwrap_or(-1);
                if pct != last_pct {
                    last_pct = pct;
                    set(&progress_app, |s| s.progress = p);
                }
            },
            || {},
        )
        .await;

    match result {
        Ok(()) => app.restart(),
        Err(e) => {
            eprintln!("[update] install failed: {e}");
            u.busy.store(false, Ordering::SeqCst);
            set(app, |s| {
                s.state = "error".into();
                s.progress = None;
                s.error = Some(e.to_string());
            });
        }
    }
}

/// Tray item: install if an update is waiting, otherwise check now.
pub fn on_tray_click(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let has_update = app.state::<Updates>().pending.lock().unwrap().is_some();
        if has_update { install(&app).await } else { check(&app, false).await }
    });
}

/// Quiet check shortly after launch, then every few hours.
pub fn spawn_background_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            let waiting = app.state::<Updates>().pending.lock().unwrap().is_some();
            if !waiting {
                check(&app, true).await;
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub fn get_update_status(updates: State<Updates>) -> UpdateStatus {
    updates.status.lock().unwrap().clone()
}

#[tauri::command]
pub async fn check_update(app: AppHandle) {
    check(&app, false).await;
}

#[tauri::command]
pub async fn install_update(app: AppHandle) {
    install(&app).await;
}
