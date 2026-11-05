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
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

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
    #[cfg(target_os = "macos")]
    macos::float_everywhere(win);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = win;
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use tauri::WebviewWindow;

    // NSWindowCollectionBehavior bits.
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    const STATIONARY: usize = 1 << 4;
    const IGNORES_CYCLE: usize = 1 << 6;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    /// NSStatusWindowLevel: above normal and floating windows, including full-screen apps.
    const STATUS_WINDOW_LEVEL: isize = 25;

    /// Show on every Space, including other apps' full-screen Spaces. `alwaysOnTop` alone
    /// (NSFloatingWindowLevel, no FullScreenAuxiliary) keeps the window out of those.
    /// Needs the Accessory activation policy, which `run()` sets. Main thread only.
    pub fn float_everywhere(win: &WebviewWindow) {
        let Ok(ptr) = win.ns_window() else { return };
        if ptr.is_null() {
            return;
        }
        unsafe {
            let ns = &*(ptr as *const AnyObject);
            let behavior = CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY;
            let _: () = msg_send![ns, setCollectionBehavior: behavior];
            let _: () = msg_send![ns, setLevel: STATUS_WINDOW_LEVEL];
        }
    }
}

fn cursor_over_clickable(app: &AppHandle, win: &WebviewWindow, state: &AppState) -> bool {
    // Copy and release the lock before any window call: those round-trip to the main
    // thread, which may itself be blocked waiting for this lock in `set_hit_rects`
    // (sync commands run on the main thread) — that deadlock froze the app on resize.
    let rects: Vec<Rect> = state.hit_rects.lock().unwrap().clone();
    if rects.is_empty() {
        return false;
    }
    let (Ok(c), Ok(p)) = (app.cursor_position(), win.inner_position()) else { return false };
    #[cfg(not(target_os = "macos"))]
    let (cx, cy) = (c.x - p.x as f64, c.y - p.y as f64);
    // macOS: tao converts the cursor to "physical" with the *primary* display's scale but
    // the window position with the scale of the display the window is on. Go through
    // logical points (macOS's native unit) so this works on mixed-scale multi-monitor setups.
    #[cfg(target_os = "macos")]
    let (cx, cy) = {
        let s = win.scale_factor().unwrap_or(1.0);
        let ps = app.primary_monitor().ok().flatten().map(|m| m.scale_factor()).unwrap_or(s);
        ((c.x / ps - p.x as f64 / s) * s, (c.y / ps - p.y as f64 / s) * s)
    };
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

            // Brightness of what's behind the overlay, ~2/s. The UI picks the panel tone
            // (auto) and a readable text color from it; only meaningful changes are sent.
            #[cfg(windows)]
            if tick % 12 == 0 {
                if let Some(luma) = backdrop_luma(&win) {
                    let luma = (luma * 50.0).round() / 50.0; // 2% steps
                    let changed = {
                        let mut ui = state.ui.lock().unwrap();
                        let changed = ui.backdrop_luma.map_or(true, |old| (old - luma).abs() >= 0.04);
                        if changed {
                            ui.backdrop_luma = Some(luma);
                        }
                        changed
                    };
                    if changed {
                        let _ = app.emit("backdrop", luma);
                    }
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

/// Average brightness (0 = black, 1 = white) of a thin band of screen just outside
/// the window's edges — a stand-in for what's behind it, since the overlay covers
/// that. Clipped to the window's monitor so off-screen areas don't read as black.
#[cfg(windows)]
fn backdrop_luma(win: &WebviewWindow) -> Option<f64> {
    if !win.is_visible().unwrap_or(false) {
        return None;
    }
    let (p, sz, m) = (win.outer_position().ok()?, win.outer_size().ok()?, win.current_monitor().ok()??);
    let (mp, ms) = (m.position(), m.size());
    let clip = (mp.x, mp.y, mp.x + ms.width as i32, mp.y + ms.height as i32);
    let (l, t, r, b) = (p.x, p.y, p.x + sz.width as i32, p.y + sz.height as i32);
    const GAP: i32 = 2; // skip the pixels right at the edge
    const BAND: i32 = 10;
    let bands = [
        (l - GAP - BAND, t - GAP - BAND, r + GAP + BAND, t - GAP), // above
        (l - GAP - BAND, b + GAP, r + GAP + BAND, b + GAP + BAND), // below
        (l - GAP - BAND, t, l - GAP, b),                           // left
        (r + GAP, t, r + GAP + BAND, b),                           // right
    ];
    let (mut sum, mut n) = (0.0, 0u64);
    for (x0, y0, x1, y1) in bands {
        let (x0, y0, x1, y1) = (x0.max(clip.0), y0.max(clip.1), x1.min(clip.2), y1.min(clip.3));
        if x1 - x0 < 2 || y1 - y0 < 2 {
            continue;
        }
        if let Some((s, c)) = win32::luma_sum(x0, y0, x1 - x0, y1 - y0) {
            sum += s;
            n += c;
        }
    }
    (n > 0).then(|| sum / n as f64)
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

    /// Sum of per-pixel brightness over a screen rectangle (physical pixels), and the pixel count.
    pub fn luma_sum(x: i32, y: i32, w: i32, h: i32) -> Option<(f64, u64)> {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
            SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        };
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp.into());
            let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
            SelectObject(mem, old);

            let mut px = vec![0u8; (w * h * 4) as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let lines = if ok {
                GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
            } else {
                0
            };
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            if lines != h {
                return None;
            }
            // BGRA; Rec. 709 weights on the (gamma-encoded) values are close enough here.
            let sum = px
                .chunks_exact(4)
                .map(|c| (0.0722 * c[0] as f64 + 0.7152 * c[1] as f64 + 0.2126 * c[2] as f64) / 255.0)
                .sum();
            Some((sum, (w * h) as u64))
        }
    }

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
