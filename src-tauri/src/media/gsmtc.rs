//! Windows backend: Global System Media Transport Controls.
//! Reads the same data the Windows volume flyout shows. No network, no login.

use super::{MediaSource, Snapshot, Track};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;

/// 100ns ticks between 1601-01-01 (WinRT DateTime epoch) and 1970-01-01.
const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

pub struct GsmtcSource {
    preferred: String,
    manager: Option<Manager>,
    session: Option<Session>,
}

impl GsmtcSource {
    pub fn new(preferred_app: &str) -> Self {
        Self { preferred: preferred_app.to_lowercase(), manager: None, session: None }
    }

    fn pick_session(&mut self) -> Option<Session> {
        if self.manager.is_none() {
            self.manager = Manager::RequestAsync().ok()?.get().ok();
        }
        let mgr = self.manager.as_ref()?;
        if !self.preferred.is_empty() {
            if let Ok(list) = mgr.GetSessions() {
                for i in 0..list.Size().unwrap_or(0) {
                    if let Ok(s) = list.GetAt(i) {
                        let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
                        if id.to_lowercase().contains(&self.preferred) {
                            return Some(s);
                        }
                    }
                }
            }
        }
        mgr.GetCurrentSession().ok()
    }
}

fn now_ticks() -> i64 {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    ms * 10_000 + EPOCH_DIFF_TICKS
}

impl MediaSource for GsmtcSource {
    fn snapshot(&mut self) -> Option<Snapshot> {
        let session = self.pick_session();
        self.session = session.clone();
        let s = session?;

        let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
        let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        if title.trim().is_empty() {
            return None;
        }
        let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default();
        let source_app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();

        let playing = s
            .GetPlaybackInfo()
            .and_then(|p| p.PlaybackStatus())
            .map(|st| st == Status::Playing)
            .unwrap_or(false);

        let (mut pos_ticks, end_ticks, updated) = match s.GetTimelineProperties() {
            Ok(t) => (
                t.Position().map(|d| d.Duration).unwrap_or(0),
                t.EndTime().map(|d| d.Duration).unwrap_or(0),
                t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0),
            ),
            Err(_) => (0, 0, 0),
        };
        // The timeline is a sample taken at `updated`; extrapolate to now while playing.
        if playing && updated > 0 {
            let elapsed = now_ticks() - updated;
            if elapsed > 0 && elapsed < 24 * 3600 * 10_000_000 {
                pos_ticks += elapsed;
            }
        }
        if end_ticks > 0 {
            pos_ticks = pos_ticks.min(end_ticks);
        }

        Some(Snapshot {
            track: Track {
                title,
                artist,
                album,
                duration_ms: (end_ticks.max(0) / 10_000) as u64,
                source_app,
                music: true,
            },
            playing,
            position_ms: (pos_ticks.max(0) / 10_000) as u64,
        })
    }

    fn album_art(&mut self) -> Option<(Vec<u8>, String)> {
        let s = self.session.as_ref()?;
        let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
        let thumb = props.Thumbnail().ok()?;
        let stream = thumb.OpenReadAsync().ok()?.get().ok()?;
        let size = stream.Size().ok()? as u32;
        if size == 0 || size > 8 * 1024 * 1024 {
            return None;
        }
        let mime = stream.ContentType().map(|h| h.to_string()).unwrap_or_default();
        let reader = DataReader::CreateDataReader(&stream).ok()?;
        reader.LoadAsync(size).ok()?.get().ok()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).ok()?;
        let mime = if mime.starts_with("image/") { mime } else { "image/jpeg".into() };
        Some((buf, mime))
    }

    fn toggle_play_pause(&mut self) -> Result<(), String> {
        let s = match self.session.clone() {
            Some(s) => s,
            None => self.pick_session().ok_or("no media session")?,
        };
        s.TryTogglePlayPauseAsync()
            .and_then(|op| op.get())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
