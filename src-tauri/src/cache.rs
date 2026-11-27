//! On-disk lyrics cache (app cache dir / lyrics / "<artist> - <title>.json").
//! Hits are kept forever so songs already looked up work offline.
//! "Not found" is remembered for a day so new uploads to the sources are picked up.
//! Hits saved under older lookup rules ([`RULES`]) are looked up again once.

use crate::lyrics::{Lyrics, Query};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MISS_TTL_SECS: u64 = 24 * 3600;

/// Bump when the lookup picks different lyrics for the same song, so cached hits
/// get one fresh lookup. 2: original script over romanizations / translations.
const RULES: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    lyrics: Option<Lyrics>,
    fetched_at: u64,
    /// [`RULES`] at the time of saving; 0 for entries from before it existed.
    #[serde(default)]
    rules: u32,
}

pub struct LyricsCache {
    dir: PathBuf,
}

pub enum Cached {
    Hit(Lyrics),
    /// Found under older lookup rules: still usable, but worth looking up again.
    Stale(Lyrics),
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
            Some(l) if e.rules < RULES => Cached::Stale(l),
            Some(l) => Cached::Hit(l),
            None if now().saturating_sub(e.fetched_at) < MISS_TTL_SECS => Cached::KnownMiss,
            None => Cached::Unknown,
        }
    }

    pub fn put(&self, q: &Query, lyrics: Option<&Lyrics>) {
        self.write(q, lyrics, RULES);
    }

    /// Save an edit (the sync offset) to cached lyrics. Unlike [`put`](Self::put)
    /// this isn't a fresh lookup, so an entry from older rules stays due for one.
    pub fn update(&self, q: &Query, lyrics: &Lyrics) {
        let rules = std::fs::read(self.path(q))
            .ok()
            .and_then(|b| serde_json::from_slice::<Entry>(&b).ok())
            .map_or(RULES, |e| e.rules);
        self.write(q, Some(lyrics), rules);
    }

    fn write(&self, q: &Query, lyrics: Option<&Lyrics>, rules: u32) {
        let e = Entry { lyrics: lyrics.cloned(), fetched_at: now(), rules };
        if let Ok(json) = serde_json::to_vec(&e) {
            let _ = std::fs::write(self.path(q), json);
        }
    }

    pub fn remove(&self, q: &Query) {
        let _ = std::fs::remove_file(self.path(q));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_from_older_rules_are_stale() {
        let dir = std::env::temp_dir().join(format!("magiclyrics-cache-test-{}", std::process::id()));
        let cache = LyricsCache::new(dir.clone());
        let q = Query { title: "Song".into(), artist: "Artist".into(), album: String::new(), duration_ms: 0 };
        let l = Lyrics::plain("t", "a\nb").unwrap();
        // Written before `rules` existed.
        let old = serde_json::json!({ "lyrics": l, "fetchedAt": now() });
        std::fs::write(cache.path(&q), serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(matches!(cache.get(&q), Cached::Stale(_)));
        cache.update(&q, &l);
        assert!(matches!(cache.get(&q), Cached::Stale(_)), "an offset edit isn't a lookup");
        cache.put(&q, Some(&l));
        assert!(matches!(cache.get(&q), Cached::Hit(_)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
