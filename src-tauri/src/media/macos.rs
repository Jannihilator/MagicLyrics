//! macOS backend: the system "Now Playing" info (what Control Center shows), so it
//! works for Spotify, Apple Music, and YouTube / YouTube Music in Safari, Chrome, etc.
//!
//! Since macOS 15.4 only Apple-signed processes may read the MediaRemote framework.
//! We use ungive/mediaremote-adapter (BSD-3), which runs inside the system's
//! `/usr/bin/perl` and prints now-playing updates as JSON lines. Its script and
//! helper framework are bundled with the app (see scripts/fetch-mediaremote-adapter.sh).

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::{MediaSource, Snapshot, Track};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PERL: &str = "/usr/bin/perl";
const SCRIPT: &str = "mediaremote-adapter.pl";
const FRAMEWORK: &str = "MediaRemoteAdapter.framework";

struct Adapter {
    script: PathBuf,
    framework: PathBuf,
}

impl Adapter {
    /// Bundled app: Contents/Resources/<script>, Contents/Frameworks/<framework>.
    /// `npm run dev`: src-tauri/macos/ (filled by the fetch script).
    fn locate(resource_dir: Option<&Path>) -> Option<Adapter> {
        let mut candidates = Vec::new();
        if let Some(r) = resource_dir {
            candidates.push((r.join(SCRIPT), r.join("../Frameworks").join(FRAMEWORK)));
            candidates.push((r.join(SCRIPT), r.join(FRAMEWORK)));
        }
        #[cfg(debug_assertions)]
        {
            let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("macos");
            candidates.push((dev.join(SCRIPT), dev.join(FRAMEWORK)));
        }
        candidates
            .into_iter()
            .find(|(s, f)| s.is_file() && f.is_dir())
            .map(|(script, framework)| Adapter { script, framework })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(PERL);
        c.arg(&self.script).arg(&self.framework).args(args);
        c.stdin(Stdio::null()).stderr(Stdio::null());
        c
    }

    /// Run a one-shot command, giving up after `timeout`.
    fn run(&self, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
        let mut child = self.command(args).stdout(Stdio::piped()).spawn().ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut out, &mut buf);
            buf
        });
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        reader.join().ok()
    }
}

pub struct MediaRemoteSource {
    adapter: Option<Adapter>,
    /// Latest now-playing payload from the stream (`None` = nothing playing).
    latest: Arc<Mutex<Option<Value>>>,
    child: Option<Child>,
    next_start: Instant,
}

impl MediaRemoteSource {
    pub fn new(resource_dir: Option<PathBuf>) -> Self {
        let adapter = Adapter::locate(resource_dir.as_deref());
        if adapter.is_none() {
            eprintln!("[media] mediaremote-adapter not found; run scripts/fetch-mediaremote-adapter.sh");
        }
        Self { adapter, latest: Arc::new(Mutex::new(None)), child: None, next_start: Instant::now() }
    }

    /// Keep one `stream` process running; restart it (with a pause) if it exits.
    fn ensure_stream(&mut self) {
        if let Some(c) = self.child.as_mut() {
            match c.try_wait() {
                Ok(None) => return,
                _ => {
                    self.child = None;
                    *self.latest.lock().unwrap() = None;
                    self.next_start = Instant::now() + Duration::from_secs(5);
                }
            }
        }
        let Some(a) = self.adapter.as_ref() else { return };
        if Instant::now() < self.next_start {
            return;
        }
        let spawned = a
            .command(&["stream", "--no-diff", "--no-artwork", "--micros", "--debounce=100"])
            .stdout(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[media] could not start {PERL}: {e}");
                self.next_start = Instant::now() + Duration::from_secs(30);
                return;
            }
        };
        let Some(out) = child.stdout.take() else { return };
        let latest = self.latest.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let payload = v.get("payload").cloned().filter(|p| p.as_object().is_some_and(|o| !o.is_empty()));
                *latest.lock().unwrap() = payload;
            }
        });
        self.child = Some(child);
    }
}

impl Drop for MediaRemoteSource {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn now_micros() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as f64).unwrap_or(0.0)
}

fn str_of(p: &Value, k: &str) -> String {
    p.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string()
}

/// Turn one adapter payload into a snapshot (pure; unit-tested below).
fn parse(p: &Value, now_us: f64) -> Option<Snapshot> {
    let title = str_of(p, "title");
    if title.is_empty() {
        return None;
    }
    // Web media reports the browser as the parent app (e.g. com.google.Chrome).
    let parent = str_of(p, "parentApplicationBundleIdentifier");
    let source_app = if parent.is_empty() { str_of(p, "bundleIdentifier") } else { parent };
    let playing = p.get("playing").and_then(Value::as_bool).unwrap_or(false);
    let num = |k: &str| p.get(k).and_then(Value::as_f64);

    let duration_us = num("durationMicros").unwrap_or(0.0).max(0.0);
    let mut pos_us = num("elapsedTimeMicros").unwrap_or(0.0).max(0.0);
    // `elapsedTime` was sampled at `timestamp`; extrapolate to now while playing.
    if playing {
        if let Some(ts) = num("timestampEpochMicros") {
            let rate = num("playbackRate").filter(|r| *r > 0.0).unwrap_or(1.0);
            let dt = now_us - ts;
            if dt > 0.0 && dt < 24.0 * 3600.0 * 1e6 {
                pos_us += dt * rate;
            }
        }
    }
    if duration_us > 0.0 {
        pos_us = pos_us.min(duration_us);
    }

    Some(Snapshot {
        track: Track {
            title,
            artist: str_of(p, "artist"),
            album: str_of(p, "album"),
            duration_ms: (duration_us / 1000.0) as u64,
            source_app,
            music: true,
        },
        playing,
        position_ms: (pos_us / 1000.0) as u64,
    })
}

impl MediaSource for MediaRemoteSource {
    fn snapshot(&mut self) -> Option<Snapshot> {
        self.ensure_stream();
        let p = self.latest.lock().unwrap().clone()?;
        parse(&p, now_micros())
    }

    fn album_art(&mut self) -> Option<(Vec<u8>, String)> {
        let a = self.adapter.as_ref()?;
        let out = a.run(&["get"], Duration::from_secs(3))?;
        let v: Value = serde_json::from_slice(&out).ok()?;
        let data = v.get("artworkData")?.as_str()?;
        let bytes = B64.decode(data.trim()).ok()?;
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return None;
        }
        let mime = str_of(&v, "artworkMimeType");
        let mime = if mime.starts_with("image/") { mime } else { "image/jpeg".into() };
        Some((bytes, mime))
    }

    fn toggle_play_pause(&mut self) -> Result<(), String> {
        let a = self.adapter.as_ref().ok_or("mediaremote-adapter not bundled")?;
        // 2 = kMRTogglePlayPause
        a.run(&["send", "2"], Duration::from_secs(3)).map(|_| ()).ok_or_else(|| "send failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payload_and_extrapolates() {
        let p: Value = serde_json::json!({
            "bundleIdentifier": "com.google.Chrome.helper",
            "parentApplicationBundleIdentifier": "com.google.Chrome",
            "playing": true,
            "title": "Taylor Swift - Anti-Hero (Official Music Video)",
            "artist": "TaylorSwiftVEVO",
            "durationMicros": 201_000_000.0,
            "elapsedTimeMicros": 10_000_000.0,
            "timestampEpochMicros": 1_000_000_000.0,
            "playbackRate": 1.0
        });
        let s = parse(&p, 1_000_000_000.0 + 2_500_000.0).unwrap();
        assert_eq!(s.track.source_app, "com.google.Chrome");
        assert_eq!(s.track.duration_ms, 201_000);
        assert_eq!(s.position_ms, 12_500);
        assert!(s.playing);
    }

    #[test]
    fn paused_and_empty() {
        let p: Value = serde_json::json!({
            "bundleIdentifier": "com.spotify.client", "playing": false, "title": "Song",
            "elapsedTimeMicros": 5_000_000.0, "timestampEpochMicros": 0.0
        });
        let s = parse(&p, 9e15).unwrap();
        assert_eq!(s.position_ms, 5_000);
        assert_eq!(s.track.source_app, "com.spotify.client");
        assert!(parse(&serde_json::json!({ "title": "" }), 0.0).is_none());
    }
}
