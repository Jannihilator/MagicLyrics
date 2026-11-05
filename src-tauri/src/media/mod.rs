//! Platform-neutral "what is playing" layer.
//!
//! Each platform implements [`MediaSource`]; the rest of the app never touches
//! platform APIs. Windows uses the system media session (GSMTC), which works for
//! Spotify and any other player that reports to Windows, with no login or API key.
//! macOS reads the system Now Playing info via mediaremote-adapter (`macos.rs`).
//! Linux (MPRIS) can be added later behind the same trait.

pub mod browser;
#[cfg(windows)]
mod gsmtc;
// Pure std + serde, so its parser tests also run on Windows.
#[cfg(any(target_os = "macos", test))]
mod macos;

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub source_app: String,
    /// Looks like a song (always true for music players; guessed for browser media).
    pub music: bool,
}

impl Track {
    /// Identity of a track, ignoring duration (which can arrive a moment later).
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.artist, self.title, self.album)
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub track: Track,
    pub playing: bool,
    pub position_ms: u64,
}

pub trait MediaSource {
    /// Current track and playback position, or `None` when nothing is playing.
    fn snapshot(&mut self) -> Option<Snapshot>;
    /// Album art bytes and MIME type for the current track.
    fn album_art(&mut self) -> Option<(Vec<u8>, String)>;
    fn toggle_play_pause(&mut self) -> Result<(), String>;
}

pub enum Command {
    TogglePlayPause,
}

/// Pick the backend for this platform. Both follow whichever player most recently
/// started playing (no per-app priority). `resource_dir` is where bundled helpers live.
pub fn new_source(resource_dir: Option<std::path::PathBuf>) -> Box<dyn MediaSource> {
    #[cfg(windows)]
    {
        let _ = resource_dir;
        Box::new(gsmtc::GsmtcSource::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MediaRemoteSource::new(resource_dir))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = resource_dir;
        Box::new(Unsupported)
    }
}

/// Apply platform-neutral clean-up (browser / YouTube metadata) to a backend snapshot.
pub fn refine(mut s: Snapshot) -> Snapshot {
    s.track = browser::refine(s.track);
    s
}

/// Called once on the media worker thread before using the backend.
pub fn init_thread() {
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(
            windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
        );
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
struct Unsupported;

#[cfg(not(any(windows, target_os = "macos")))]
impl MediaSource for Unsupported {
    fn snapshot(&mut self) -> Option<Snapshot> {
        None
    }
    fn album_art(&mut self) -> Option<(Vec<u8>, String)> {
        None
    }
    fn toggle_play_pause(&mut self) -> Result<(), String> {
        Err("media control is not implemented on this platform yet".into())
    }
}
