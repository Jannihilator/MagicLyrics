//! User settings persisted as JSON in the app config dir.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub show_art: bool,
    pub show_play_pause: bool,
    pub show_title: bool,
    pub font_size: u32,
    /// "auto" (follow what's behind the overlay), "dark" or "light".
    pub theme: String,
    /// Panel background opacity: "low" (20%), "medium" (80%) or "high" (100%).
    pub panel_opacity: String,
    /// Accent color of the controls, "#rrggbb".
    pub accent: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// macOS: x/y/width/height are logical points (older builds saved mismatched
    /// "physical" values, which are discarded once). Unused on Windows (physical pixels).
    pub geometry_logical: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_art: true,
            show_play_pause: true,
            show_title: true,
            font_size: 18,
            theme: "auto".into(),
            panel_opacity: "medium".into(),
            accent: "#1ed760".into(),
            x: None,
            y: None,
            width: None,
            height: None,
            geometry_logical: false,
        }
    }
}

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

pub fn load(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        // Write-then-rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}
