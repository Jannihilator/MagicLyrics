//! Overlay behaviour: never take focus, and let clicks fall through everywhere
//! except a few small areas reported by the UI (the handle, and the play button
//! when it's shown), or the whole window while the controls are open.
//!
//! Click-through is decided by polling the cursor position (no global mouse hooks,
//! so no input-monitoring permission): when the cursor is over a clickable area,
//! the window starts accepting mouse input; when it leaves, input passes through again.

use crate::AppState;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewWindow};

/// Physical pixels relative to the window's client area.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Extra platform hardening on top of `focusable: false`.
pub fn harden(win: &WebviewWindow) {
    #[cfg(windows)]
    {
        win32::no_mouse_activate(win);
        // Tauri attaches its resize-border window around window creation; retry until it exists.
        if !win32::diagonal_resize(win) {
            let win = win.clone();
            std::thread::spawn(move || {
                for _ in 0..40 {
                    std::thread::sleep(Duration::from_millis(250));
                    let (tx, rx) = std::sync::mpsc::channel();
                    let w = win.clone();
                    if win.run_on_main_thread(move || { let _ = tx.send(win32::diagonal_resize(&w)); }).is_err() {
                        return;
                    }
                    if rx.recv().unwrap_or(false) {
                        return;
                    }
                }
            });
        }
    }
    #[cfg(not(windows))]
    let _ = win;
}

fn cursor_over_clickable(app: &AppHandle, win: &WebviewWindow, state: &AppState) -> bool {
    let rects = state.hit_rects.lock().unwrap();
    if rects.is_empty() {
        return false;
    }
    let (Ok(c), Ok(p)) = (app.cursor_position(), win.inner_position()) else { return false };
    let (cx, cy) = (c.x - p.x as f64, c.y - p.y as f64);
    rects.iter().any(|r| cx >= r.x && cx < r.x + r.w && cy >= r.y && cy < r.y + r.h)
}

pub fn spawn_hit_tester(app: AppHandle) {
    std::thread::spawn(move || {
        let mut interactive: Option<bool> = None;
        let mut tick: u32 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            tick = tick.wrapping_add(1);
            let Some(win) = app.get_webview_window("main") else { continue };
            let state = app.state::<AppState>();

            let want = state.controls_open.load(Ordering::Relaxed)
                || cursor_over_clickable(&app, &win, &state);
            if interactive != Some(want) {
                if win.set_ignore_cursor_events(!want).is_ok() {
                    interactive = Some(want);
                }
            }

            // Persist settings (window geometry changes often while dragging) ~1/s.
            if tick % 25 == 0 && state.settings_dirty.swap(false, Ordering::Relaxed) {
                let s = state.settings.lock().unwrap().clone();
                crate::settings::save(&state.settings_path, &s);
            }
        }
    });
}

#[cfg(windows)]
mod win32 {
    use tauri::WebviewWindow;
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, ShowWindow, HTTRANSPARENT, MA_NOACTIVATE, SW_HIDE, WM_MOUSEACTIVATE, WM_NCHITTEST,
    };

    fn hwnd(win: &WebviewWindow) -> Option<HWND> {
        win.hwnd().ok().map(|h| HWND(h.0 as *mut core::ffi::c_void))
    }

    unsafe extern "system" fn no_activate_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Clicking the window (e.g. the handle or a control) must not activate it.
    pub fn no_mouse_activate(win: &WebviewWindow) {
        if let Some(h) = hwnd(win) {
            unsafe {
                let _ = SetWindowSubclass(h, Some(no_activate_proc), 0x4d4c, 0);
            }
        }
    }

    /// Tauri's invisible resize-border window resizes along one axis per edge.
    /// Make it transparent to the mouse; the UI's own edge zones (which always
    /// resize diagonally) take over.
    unsafe extern "system" fn border_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_NCHITTEST {
            return LRESULT(HTTRANSPARENT as isize);
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Returns false if Tauri's border window doesn't exist yet.
    pub fn diagonal_resize(win: &WebviewWindow) -> bool {
        let Some(parent) = hwnd(win) else { return false };
        unsafe {
            match FindWindowExW(Some(parent), None, w!("TAURI_DRAG_RESIZE_BORDERS"), w!("TAURI_DRAG_RESIZE_WINDOW")) {
                Ok(border) => {
                    let _ = ShowWindow(border, SW_HIDE);
                    SetWindowSubclass(border, Some(border_proc), 0x4d4d, 0).as_bool()
                }
                Err(_) => false,
            }
        }
    }
}
