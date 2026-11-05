//! On-disk lyrics cache (app cache dir / lyrics / "<artist> - <title>.json").
//! Hits are kept forever so songs already looked up work offline.
//! "Not found" is remembered for a day so new uploads to the sources are picked up.

use crate::lyrics::{Lyrics, Query};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MISS_TTL_SECS: u64 = 24 * 3600;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    lyrics: Option<Lyrics>,
    fetched_at: u64,
}

pub struct LyricsCache {
    dir: PathBuf,
}

pub enum Cached {
    Hit(Lyrics),
    KnownMiss,
    Unknown,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl LyricsCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path(&self, q: &Query) -> PathBuf {
        let key = format!("{} - {}", q.artist, q.title).to_lowercase();
        let safe: String = key
            .chars()
            .map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' })
            .take(150)
            .collect();
        self.dir.join(format!("{}.json", safe.trim()))
    }

    pub fn get(&self, q: &Query) -> Cached {
        let Ok(bytes) = std::fs::read(self.path(q)) else { return Cached::Unknown };
        let Ok(e) = serde_json::from_slice::<Entry>(&bytes) else { return Cached::Unknown };
        match e.lyrics {
            Some(l) => Cached::Hit(l),
            None if now().saturating_sub(e.fetched_at) < MISS_TTL_SECS => Cached::KnownMiss,
            None => Cached::Unknown,
        }
    }

    pub fn put(&self, q: &Query, lyrics: Option<&Lyrics>) {
        let e = Entry { lyrics: lyrics.cloned(), fetched_at: now() };
        if let Ok(json) = serde_json::to_vec(&e) {
            let _ = std::fs::write(self.path(q), json);
        }
    }

    pub fn remove(&self, q: &Query) {
        let _ = std::fs::remove_file(self.path(q));
    }
}
