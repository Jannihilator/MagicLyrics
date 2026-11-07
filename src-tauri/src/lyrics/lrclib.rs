//! LRCLIB (https://lrclib.net): free, keyless, community-maintained synced lyrics.

use super::{normalize, Lyrics, Query};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

const SOURCE: &str = "LRCLIB";

#[derive(Deserialize, Clone)]
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
    let mut best = pick(&search(client, &title, Some(&artist)).await?, q, &title, &artist, false);

    // 3) LRCLIB's artist filter is literal, so 张栋梁 misses 張棟樑 and an English
    //    name misses the Chinese one. Without a synced hit, search by title alone and
    //    compare artists script-folded. A different artist is accepted only for a
    //    Chinese title that matches exactly with a near-identical duration (the same
    //    recording listed under another name); never for other titles, where common
    //    names like "Hello" would pull in unrelated songs.
    if !best.as_ref().is_some_and(|r| r.synced_lyrics.is_some()) {
        let loose = dur_s > 0 && super::is_chinese(&title);
        if let Some(r) = pick(&search(client, &title, None).await?, q, &title, &artist, loose) {
            if best.is_none() || r.synced_lyrics.is_some() {
                best = Some(r);
            }
        }
    }
    let found = best.and_then(|r| to_lyrics(&r));
    Ok(match (found, exact_plain) {
        (Some(f), _) if f.synced => Some(f),
        (_, Some(e)) => Some(e),
        (f, None) => f,
    })
}

async fn search(client: &Client, title: &str, artist: Option<&str>) -> Result<Vec<Record>, String> {
    let mut params = vec![("track_name", title)];
    if let Some(a) = artist {
        params.push(("artist_name", a));
    }
    let resp = client
        .get("https://lrclib.net/api/search")
        .query(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("search: HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

/// Normalize for comparison. Chinese is folded into Simplified: Traditional ->
/// Simplified is many-to-one (張棟樑 and 張棟梁 both become 张栋梁), the other
/// direction is not.
fn norm(s: &str) -> String {
    normalize(&zhconv::zhconv(s, zhconv::Variant::ZhHans))
}

/// Best-scoring record. With `loose_artist`, a non-matching artist is allowed but
/// the title must match exactly and the duration must be within 3 s.
fn pick(records: &[Record], q: &Query, title: &str, artist: &str, loose_artist: bool) -> Option<Record> {
    let dur_s = q.duration_ms / 1000;
    let want_title = norm(title);
    let want_artist = norm(artist);
    let score = |r: &Record| -> Option<i64> {
        let t = norm(&super::clean_title(r.track_name.as_deref().unwrap_or("")));
        let a = norm(r.artist_name.as_deref().unwrap_or(""));
        // An empty name would "contain"-match anything.
        if t.is_empty() || (t != want_title && !t.contains(&want_title) && !want_title.contains(&t)) {
            return None;
        }
        let artist_ok =
            want_artist.is_empty() || (!a.is_empty() && (a.contains(&want_artist) || want_artist.contains(&a)));
        if !artist_ok && !(loose_artist && t == want_title) {
            return None;
        }
        if !variant_ok(r.track_name.as_deref().unwrap_or(""), &q.title) {
            return None;
        }
        let mut s = 0i64;
        if dur_s > 0 {
            let d = (r.duration.unwrap_or(0.0) - dur_s as f64).abs();
            if d > if artist_ok { 20.0 } else { 3.0 } {
                return None;
            }
            s -= (d * 10.0) as i64;
        }
        if artist_ok {
            s += 500;
        }
        if r.synced_lyrics.is_some() {
            s += 1000;
        }
        if t == want_title {
            s += 100;
        }
        Some(s)
    };
    records
        .iter()
        .filter_map(|r| score(r).map(|s| (s, r)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, r)| r.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(title: &str, artist: &str, dur: f64, synced: bool) -> Record {
        Record {
            track_name: Some(title.into()),
            artist_name: Some(artist.into()),
            duration: Some(dur),
            instrumental: Some(false),
            plain_lyrics: Some("a\nb\nc".into()),
            synced_lyrics: synced.then(|| "[00:01.00]a\n[00:02.00]b\n[00:03.00]c".into()),
        }
    }

    fn q(title: &str, artist: &str, secs: u64) -> Query {
        Query { title: title.into(), artist: artist.into(), album: String::new(), duration_ms: secs * 1000 }
    }

    #[test]
    fn simplified_artist_matches_traditional_variant() {
        // 张栋梁 -> 張棟梁 in Traditional, but LRCLIB's synced copy says 張棟樑.
        let rs = [rec("寂寞邊界", "張棟梁", 271.5, false), rec("寂寞邊界", "張棟樑", 271.0, true)];
        let r = pick(&rs, &q("寂寞邊界", "张栋梁", 271), "寂寞邊界", "张栋梁", false).unwrap();
        assert_eq!(r.artist_name.as_deref(), Some("張棟樑"));
    }

    #[test]
    fn strict_rejects_other_artist() {
        let rs = [rec("Hello", "Someone Else", 230.0, true)];
        assert!(pick(&rs, &q("Hello", "Nobody", 230), "Hello", "Nobody", false).is_none());
    }

    #[test]
    fn loose_needs_exact_title_and_close_duration() {
        let qq = q("愛", "無名", 240);
        assert!(pick(&[rec("愛你", "某人", 240.0, true)], &qq, "愛", "無名", true).is_none());
        assert!(pick(&[rec("愛", "某人", 246.0, true)], &qq, "愛", "無名", true).is_none());
        assert!(pick(&[rec("愛", "某人", 241.0, true)], &qq, "愛", "無名", true).is_some());
    }

    #[test]
    fn empty_names_do_not_match_everything() {
        let qq = q("寂寞邊界", "張棟樑", 271);
        assert!(pick(&[rec("", "張棟樑", 271.0, true)], &qq, "寂寞邊界", "張棟樑", false).is_none());
        assert!(pick(&[rec("寂寞邊界", "", 271.0, true)], &qq, "寂寞邊界", "張棟樑", false).is_none());
    }
}
