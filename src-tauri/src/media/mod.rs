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
    /// Whether the player accepts previous / next right now (true where unknown).
    pub can_prev: bool,
    pub can_next: bool,
}

pub trait MediaSource {
    /// Current track and playback position, or `None` when nothing is playing.
    fn snapshot(&mut self) -> Option<Snapshot>;
    /// Album art bytes and MIME type for the current track.
    fn album_art(&mut self) -> Option<(Vec<u8>, String)>;
    fn toggle_play_pause(&mut self) -> Result<(), String>;
    /// Skip to the next track, or back to the previous one.
    fn skip(&mut self, next: bool) -> Result<(), String>;
}

pub enum Command {
    TogglePlayPause,
    Skip { next: bool },
}

/// Pick the backend for this platform. Both follow whichever player most recently
/// started playing. Windows can see every player, so there a browser video that
/// isn't a song doesn't replace music that is still playing ([`choose`]); macOS
/// only reports the one the system picked. `resource_dir` is where bundled helpers
/// live.
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

/// The track the user paused from the overlay. It stays followed (so the next
/// press resumes it, even while a video plays on) until any player starts
/// playing, the paused one included. Skipping while held keeps following that
/// player, whatever it moves on to.
#[cfg(any(windows, test))]
#[derive(Default)]
pub struct Hold {
    /// Source app, title (`None` after a skip: any title), and the start counter
    /// when it was paused.
    held: Option<(String, Option<String>, u64)>,
}

#[cfg(any(windows, test))]
impl Hold {
    /// The overlay's play/pause was pressed on `track`, which was `playing` before
    /// the press; `starts` counts how often any player has started playing.
    pub fn toggled(&mut self, track: Option<&Track>, playing: bool, starts: u64) {
        self.held = match track {
            Some(t) if playing => Some((t.source_app.clone(), Some(t.title.clone()), starts)),
            _ => None,
        };
    }

    /// The overlay skipped on `source_app`: if that's the held player, keep following
    /// it through the title change.
    pub fn skipped(&mut self, source_app: &str) {
        if let Some(h) = self.held.as_mut().filter(|h| h.0 == source_app) {
            h.1 = None;
        }
    }

    /// The held (source app, title), unless a player has started since.
    pub fn current(&mut self, starts: u64) -> Option<(&str, Option<&str>)> {
        if self.held.as_ref().is_some_and(|h| h.2 != starts) {
            self.held = None;
        }
        self.held.as_ref().map(|(a, t, _)| (a.as_str(), t.as_deref()))
    }
}

/// Which of the players to follow, given their raw snapshots best-ranked first
/// (most recently started, playing before paused). The first one, unless it is a
/// browser video that isn't a song: then the first song that is playing, or, when
/// that video is paused too, the first song at all. `held` (source app, title) is
/// a track the user just paused from the overlay; it stays chosen while present
/// (any title from that app when the title is `None`).
/// `None` only for an empty list.
#[cfg(any(windows, test))]
pub fn choose(ranked: &[Snapshot], held: Option<(&str, Option<&str>)>) -> Option<usize> {
    if let Some((app, title)) = held {
        let matches = |s: &Snapshot| s.track.source_app == app && title.is_none_or(|t| s.track.title == t);
        if let Some(i) = ranked.iter().position(matches) {
            return Some(i);
        }
    }
    let first = ranked.first()?;
    if browser::is_music(&first.track) {
        return Some(0);
    }
    ranked
        .iter()
        .position(|s| browser::is_music(&s.track) && (s.playing || !first.playing))
        .or(Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(app: &str, title: &str, artist: &str, secs: u64, playing: bool) -> Snapshot {
        let track = Track {
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: secs * 1000,
            source_app: app.into(),
            music: true,
        };
        Snapshot { track, playing, position_ms: 0, can_prev: true, can_next: true }
    }

    fn spotify(playing: bool) -> Snapshot {
        snap("Spotify.exe", "Some Song", "Some Artist", 200, playing)
    }

    fn vlog(playing: bool) -> Snapshot {
        snap("Chrome", "My weekend vlog", "Some Channel", 900, playing)
    }

    fn mv(playing: bool) -> Snapshot {
        snap("Chrome", "Some Artist - Some Song (Official Music Video)", "SomeArtistVEVO", 210, playing)
    }

    #[test]
    fn non_song_video_does_not_replace_playing_music() {
        assert_eq!(choose(&[vlog(true), spotify(true)], None), Some(1));
        assert_eq!(choose(&[vlog(true), vlog(false), spotify(true)], None), Some(2));
    }

    #[test]
    fn paused_music_does_not_beat_a_playing_video() {
        assert_eq!(choose(&[vlog(true), spotify(false)], None), Some(0));
    }

    #[test]
    fn all_paused_prefers_the_song() {
        assert_eq!(choose(&[vlog(false), spotify(false)], None), Some(1));
    }

    #[test]
    fn songs_and_lone_videos_are_followed() {
        assert_eq!(choose(&[mv(true), spotify(true)], None), Some(0), "a music video takes over");
        assert_eq!(choose(&[spotify(false), vlog(true)], None), Some(0));
        assert_eq!(choose(&[vlog(true)], None), Some(0), "nothing else: show the video");
        assert_eq!(choose(&[], None), None);
    }

    #[test]
    fn hold_lasts_until_a_player_starts() {
        let mut h = Hold::default();
        h.toggled(Some(&spotify(true).track), true, 4);
        assert_eq!(h.current(4), Some(("Spotify.exe", Some("Some Song"))));
        // Resumed from the overlay: no hold.
        h.toggled(Some(&spotify(false).track), false, 4);
        assert_eq!(h.current(4), None);
        // Paused again, then a player (another video, or Spotify itself) starts.
        h.toggled(Some(&spotify(true).track), true, 4);
        assert_eq!(h.current(5), None);
        assert_eq!(h.current(4), None, "stays ended");
    }

    #[test]
    fn song_paused_from_the_overlay_stays() {
        // Spotify paused with the overlay while a video keeps playing.
        let held = Some(("Spotify.exe", Some("Some Song")));
        assert_eq!(choose(&[vlog(true), spotify(false)], held), Some(1));
        // Gone (player closed): back to the usual choice.
        assert_eq!(choose(&[vlog(true)], held), Some(0));
    }

    #[test]
    fn skip_while_held_keeps_following_the_player() {
        // Spotify paused from the overlay, a video plays on, then next is pressed
        // and Spotify stays paused on the next song.
        let mut h = Hold::default();
        h.toggled(Some(&spotify(true).track), true, 4);
        h.skipped("Other.exe");
        assert_eq!(h.current(4), Some(("Spotify.exe", Some("Some Song"))), "another app: unchanged");
        h.skipped("Spotify.exe");
        assert_eq!(h.current(4), Some(("Spotify.exe", None)));
        let next = snap("Spotify.exe", "Next Song", "Artist", 200, false);
        assert_eq!(choose(&[vlog(true), next], h.current(4)), Some(1));
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
    fn skip(&mut self, _next: bool) -> Result<(), String> {
        Err("media control is not implemented on this platform yet".into())
    }
}
