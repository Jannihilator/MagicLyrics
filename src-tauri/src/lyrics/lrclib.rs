//! LRCLIB (https://lrclib.net): free, keyless, community-maintained synced lyrics.

use super::{normalize, Lyrics, Query};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

const SOURCE: &str = "LRCLIB";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    track_name: Option<String>,
    artist_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

/// Variants we should not match unless the playing title asks for them.
fn variant_ok(candidate_title: &str, wanted_title: &str) -> bool {
    let c = candidate_title.to_lowercase();
    let w = wanted_title.to_lowercase();
    ["instrumental", "karaoke", "sped up", "slowed", "nightcore", "8d audio", "reverb"]
        .iter()
        .all(|k| !c.contains(k) || w.contains(k))
}

fn to_lyrics(r: &Record) -> Option<Lyrics> {
    if let Some(s) = r.synced_lyrics.as_deref().and_then(|s| Lyrics::synced(SOURCE, s)) {
        return Some(s);
    }
    if let Some(p) = r.plain_lyrics.as_deref().and_then(|p| Lyrics::plain(SOURCE, p)) {
        return Some(p);
    }
    if r.instrumental == Some(true) {
        return Some(Lyrics::instrumental(SOURCE));
    }
    None
}

pub async fn fetch(client: &Client, q: &Query) -> Result<Option<Lyrics>, String> {
    let dur_s = q.duration_ms / 1000;
    let mut exact_plain: Option<Lyrics> = None;

    // 1) Exact signature match (fast, most accurate) when duration is known.
    //    Only a synced hit ends the search; a plain one is kept as a fallback.
    if dur_s > 0 {
        let resp = client
            .get("https://lrclib.net/api/get")
            .query(&[
                ("track_name", q.title.as_str()),
                ("artist_name", q.artist.as_str()),
                ("album_name", q.album.as_str()),
                ("duration", &dur_s.to_string()),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            if let Ok(r) = resp.json::<Record>().await {
                match to_lyrics(&r) {
                    Some(l) if l.synced || l.instrumental => return Ok(Some(l)),
                    other => exact_plain = other,
                }
            }
        } else if resp.status() != StatusCode::NOT_FOUND {
            return Err(format!("get: HTTP {}", resp.status()));
        }
    }

    // 2) Search with cleaned title + primary artist, pick the closest match.
    let title = q.clean_title();
    let artist = q.primary_artist();
    let resp = client
        .get("https://lrclib.net/api/search")
        .query(&[("track_name", title.as_str()), ("artist_name", artist.as_str())])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("search: HTTP {}", resp.status()));
    }
    let records: Vec<Record> = resp.json().await.map_err(|e| e.to_string())?;

    let want_title = normalize(&title);
    let want_artist = normalize(&artist);
    let score = |r: &Record| -> Option<i64> {
        let t = normalize(&super::clean_title(r.track_name.as_deref().unwrap_or("")));
        let a = normalize(r.artist_name.as_deref().unwrap_or(""));
        if t != want_title && !t.contains(&want_title) && !want_title.contains(&t) {
            return None;
        }
        if !want_artist.is_empty() && !a.contains(&want_artist) && !want_artist.contains(&a) {
            return None;
        }
        if !variant_ok(r.track_name.as_deref().unwrap_or(""), &q.title) {
            return None;
        }
        let mut s = 0i64;
        if dur_s > 0 {
            let d = (r.duration.unwrap_or(0.0) - dur_s as f64).abs();
            if d > 20.0 {
                return None;
            }
            s -= (d * 10.0) as i64;
        }
        if r.synced_lyrics.is_some() {
            s += 1000;
        }
        if t == want_title {
            s += 100;
        }
        Some(s)
    };
    let best = records
        .iter()
        .filter_map(|r| score(r).map(|s| (s, r)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, r)| r);
    let found = best.and_then(to_lyrics);
    Ok(match (found, exact_plain) {
        (Some(f), _) if f.synced => Some(f),
        (_, Some(e)) => Some(e),
        (f, None) => f,
    })
}
