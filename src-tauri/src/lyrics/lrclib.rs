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
    album_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

/// Variants we should not match unless the playing title asks for them.
fn variant_ok(candidate_title: &str, wanted_title: &str) -> bool {
    let c = candidate_title.to_lowercase();
    let w = wanted_title.to_lowercase();
    [
        "instrumental", "karaoke", "sped up", "slowed", "nightcore", "8d audio", "reverb",
        "japanese ver", "korean ver", "chinese ver", "english ver",
    ]
    .iter()
        .all(|k| !c.contains(k) || w.contains(k))
}

/// Romanized / translated uploads ("Song (Romanized)", "Genius Romanizations",
/// "歌名 (中文翻譯)"). A keyword the playing title or artist itself contains doesn't
/// count, so a song called "Lost in Translation" is still found.
fn is_transliteration(r: &Record, q: &Query) -> bool {
    let name = format!("{} {}", r.track_name.as_deref().unwrap_or(""), r.artist_name.as_deref().unwrap_or("")).to_lowercase();
    let wanted = format!("{} {}", q.title, q.artist).to_lowercase();
    [
        "romaniz", "romanis", "romaji", "rōmaji", "translation", "translated", "pinyin",
        "翻譯", "翻译", "中譯", "中译", "羅馬音", "罗马音", "羅馬拼音", "罗马拼音",
    ]
    .iter()
    .any(|k| name.contains(k) && !wanted.contains(k))
}

/// The lyrics as shown (after LRC parsing, which drops credit lines like "作词 : …").
fn lyric_text(r: &Record) -> String {
    to_lyrics(r).map(|l| l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

/// A romanization rather than the original lyrics.
fn romanized(r: &Record) -> bool {
    super::looks_romanized(&lyric_text(r))
}

/// Lyrics with a translation or romanization alongside each line ([`super::is_paired`]).
fn paired(r: &Record) -> bool {
    to_lyrics(r).is_some_and(|l| l.synced && super::is_paired(&l.lines))
}

/// Original script first, then synced over plain, then clean over paired with a
/// translation.
fn quality(r: &Record) -> (bool, bool, bool) {
    (!romanized(r), to_lyrics(r).is_some_and(|l| l.synced), !paired(r))
}

/// The search's best record or the exact signature match, whichever is better;
/// the exact match wins ties, being this very recording.
fn choose(best: Option<Record>, exact: Option<Record>) -> Option<Record> {
    match (best, exact) {
        (Some(b), Some(e)) => Some(if quality(&b) > quality(&e) { b } else { e }),
        (b, e) => b.or(e),
    }
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
    let mut exact: Option<Record> = None;

    // 1) Exact signature match (fast, most accurate) when duration is known.
    //    Only a clean synced hit in the song's own script ends the search; anything
    //    else (plain, romanized, or with a translation alongside) is a fallback.
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
                    Some(l) if (l.synced && !romanized(&r) && !paired(&r) && !is_transliteration(&r, q)) || l.instrumental => {
                        return Ok(Some(l))
                    }
                    Some(_) if !is_transliteration(&r, q) => exact = Some(r),
                    _ => {}
                }
            }
        } else if resp.status() != StatusCode::NOT_FOUND {
            return Err(format!("get: HTTP {}", resp.status()));
        }
    }

    // 2) Search with cleaned title + primary artist, pick the closest match.
    let title = q.clean_title();
    let artist = q.primary_artist();
    let mut best = pick(&search(client, &[("track_name", &title), ("artist_name", &artist)]).await?, q, &title, &artist, false);

    // 3) LRCLIB's artist filter is literal, so 张栋梁 misses 張棟樑 and an English
    //    name misses the Chinese one. Without a synced hit, search by title alone and
    //    compare artists script-folded. A different artist is accepted only for a
    //    Chinese title that matches exactly with a near-identical duration (the same
    //    recording listed under another name); never for other titles, where common
    //    names like "Hello" would pull in unrelated songs.
    if !best.as_ref().is_some_and(|r| quality(r) == (true, true, true)) {
        let loose = dur_s > 0 && super::is_chinese(&title);
        if let Some(r) = pick(&search(client, &[("track_name", &title)]).await?, q, &title, &artist, loose) {
            if best.as_ref().map_or(true, |b| quality(&r) > quality(b)) {
                best = Some(r);
            }
        }
    }

    // 4) Translated titles: official uploads can carry an English title
    //    ("Loneliness") that LRCLIB only lists as 寂寞邊界. When the player reports
    //    the album, take the one song on that album with the same length (±2 s), then
    //    search again under its real title for a synced copy.
    if best.is_none() && exact.is_none() && dur_s > 0 && !q.album.trim().is_empty() {
        if let Some(hit) = album_match(&search(client, &[("q", q.album.trim())]).await?, q) {
            let real = super::clean_title(hit.track_name.as_deref().unwrap_or(""));
            let real_artist = hit.artist_name.clone().unwrap_or_default();
            let loose = super::is_chinese(&real);
            best = match pick(&search(client, &[("track_name", &real)]).await?, q, &real, &real_artist, loose) {
                Some(r) if r.synced_lyrics.is_some() => Some(r),
                _ => Some(hit),
            };
        }
    }
    Ok(choose(best, exact).and_then(|r| to_lyrics(&r)))
}

/// The single song on the query's album whose length is within 2 s. `None` when
/// two different songs on the album fit (ambiguous).
fn album_match(records: &[Record], q: &Query) -> Option<Record> {
    let want_album = norm(&q.album);
    let dur = (q.duration_ms / 1000) as f64;
    let fits: Vec<&Record> = records
        .iter()
        .filter(|r| {
            !want_album.is_empty()
                && norm(r.album_name.as_deref().unwrap_or("")) == want_album
                && r.duration.is_some_and(|d| (d - dur).abs() <= 2.0)
                && variant_ok(r.track_name.as_deref().unwrap_or(""), &q.title)
                && !is_transliteration(r, q)
                && to_lyrics(r).is_some()
        })
        .collect();
    let title_of = |r: &Record| norm(&super::clean_title(r.track_name.as_deref().unwrap_or("")));
    let first = title_of(fits.first()?);
    if first.is_empty() || fits.iter().any(|r| title_of(r) != first) {
        return None;
    }
    fits.iter().find(|r| r.synced_lyrics.is_some()).or(fits.first()).map(|r| (*r).clone())
}

async fn search(client: &Client, params: &[(&str, &str)]) -> Result<Vec<Record>, String> {
    let resp = client
        .get("https://lrclib.net/api/search")
        .query(params)
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
    let cjk_names = super::cjk_names(q);
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
        if !variant_ok(r.track_name.as_deref().unwrap_or(""), &q.title) || is_transliteration(r, q) {
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
        // A romanized copy loses to the original even when the original isn't
        // synced. Between otherwise equal copies, a clean one beats one with a
        // translation alongside, and for a song with CJK names a CJK copy beats a
        // Latin one (which may be a translation, or the song may be in English).
        if romanized(r) {
            s -= 1200;
        } else if cjk_names && !super::has_cjk(&lyric_text(r)) {
            s -= 150;
        }
        if paired(r) {
            s -= 200;
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
            album_name: Some("Greatest Hits".into()),
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
    fn album_match_needs_one_song_of_that_length() {
        let qq = Query { title: "Loneliness".into(), artist: "Nicholas Teo".into(), album: "Greatest Hits".into(), duration_ms: 272_000 };
        let hit = album_match(&[rec("陌路", "x", 291.0, true), rec("寂寞邊界", "張棟梁", 271.0, false)], &qq).unwrap();
        assert_eq!(hit.track_name.as_deref(), Some("寂寞邊界"));
        // Two different songs of that length: ambiguous.
        assert!(album_match(&[rec("寂寞邊界", "x", 271.0, false), rec("陌路", "x", 273.0, true)], &qq).is_none());
        // Same length on another album: no.
        let mut other = rec("寂寞邊界", "x", 271.0, false);
        other.album_name = Some("Other".into());
        assert!(album_match(&[other], &qq).is_none());
    }

    fn with_text(mut r: Record, synced: &str) -> Record {
        r.plain_lyrics = Some(synced.lines().map(|l| l.split(']').last().unwrap_or("")).collect::<Vec<_>>().join("\n"));
        r.synced_lyrics = Some(synced.into());
        r
    }

    // Made-up lines (not real lyrics).
    const ROMAJI: &str = "[00:01.00]Asa no hikari ni te wo nobashite\n[00:02.00]Kimi to aruita kaerimichi\n[00:03.00]Zutto issho ni ite kureru kana";
    const KANA: &str = "[00:01.00]朝の光に手を伸ばして\n[00:02.00]君と歩いた帰り道\n[00:03.00]ずっと一緒にいてくれるかな";
    const ENGLISH: &str = "[00:01.00]Walking slowly through the quiet morning\n[00:02.00]Coffee spilled across the kitchen table\n[00:03.00]I never thought the night would end like this";
    /// English song, Japanese translation interleaved line by line.
    const EN_WITH_JA: &str = "[00:01.00]Walking slowly through the quiet morning\n[00:01.00]朝の光に手を伸ばして\n[00:02.00]Coffee spilled across the kitchen table\n[00:02.00]君と歩いた帰り道";

    fn kana(r: &Record) -> bool {
        super::super::has_kana_hangul(&lyric_text(r))
    }

    #[test]
    fn prefers_original_script_over_romaji() {
        // Romanized player names: the romaji copy reads as a romanization and loses.
        for order in [[ROMAJI, KANA], [KANA, ROMAJI]] {
            let rs = order.map(|t| with_text(rec("Yoru no Uta", "Someone", 261.0, true), t));
            let r = pick(&rs, &q("Yoru no Uta", "Someone", 261), "Yoru no Uta", "Someone", false).unwrap();
            assert!(kana(&r));
        }
        // Japanese names: an unsynced original beats a synced romanization.
        let mut plain = rec("夜の歌", "誰か", 261.0, false);
        plain.plain_lyrics = Some("朝の光に手を伸ばして\n君と歩いた帰り道\nずっと一緒にいてくれるかな".into());
        let rs = [with_text(rec("夜の歌", "誰か", 261.0, true), ROMAJI), plain];
        let r = pick(&rs, &q("夜の歌", "誰か", 261), "夜の歌", "誰か", false).unwrap();
        assert!(r.synced_lyrics.is_none());
    }

    fn first_line(r: &Record) -> String {
        to_lyrics(r).and_then(|l| l.lines.first().map(|x| x.text.clone())).unwrap_or_default()
    }

    #[test]
    fn english_song_keeps_english_copy() {
        // A copy with a Japanese translation alongside loses to the plain English one,
        // also for an artist with a CJK name.
        for artist in ["Someone", "誰か"] {
            for order in [[ENGLISH, EN_WITH_JA], [EN_WITH_JA, ENGLISH]] {
                let rs = order.map(|t| with_text(rec("Morning", artist, 200.0, true), t));
                let r = pick(&rs, &q("Morning", artist, 200), "Morning", artist, false).unwrap();
                assert!(!paired(&r), "{artist}");
            }
        }
        assert!(paired(&with_text(rec("Morning", "Someone", 200.0, true), EN_WITH_JA)));
        assert!(!paired(&with_text(rec("Morning", "Someone", 200.0, true), ENGLISH)));
    }

    #[test]
    fn cjk_names_prefer_cjk_copy() {
        let rs = [with_text(rec("夜の歌", "誰か", 261.0, true), ENGLISH), with_text(rec("夜の歌", "誰か", 261.0, true), KANA)];
        let r = pick(&rs, &q("夜の歌", "誰か", 261), "夜の歌", "誰か", false).unwrap();
        assert!(kana(&r), "got {}", first_line(&r));
    }

    #[test]
    fn clean_copy_beats_paired_exact_match() {
        let clean = with_text(rec("Morning", "Someone", 200.0, true), ENGLISH);
        let mut exact = with_text(rec("Morning", "Someone", 200.0, true), EN_WITH_JA);
        exact.album_name = Some("Exact".into());
        assert!(!paired(&choose(Some(clean), Some(exact)).unwrap()));
    }

    #[test]
    fn exact_match_loses_only_to_better_script() {
        let romaji = with_text(rec("Yoru no Uta", "Someone", 261.0, true), ROMAJI);
        let mut kana_plain = rec("Yoru no Uta", "Someone", 261.0, false);
        kana_plain.plain_lyrics = Some("朝の光に手を伸ばして\n君と歩いた帰り道\nずっと一緒にいてくれるかな".into());
        // Synced romaji exact hit vs unsynced original from the search: the original.
        let r = choose(Some(kana_plain.clone()), Some(romaji.clone())).unwrap();
        assert!(kana(&r));
        // Equal quality: the exact match.
        let mut exact = with_text(rec("Yoru no Uta", "Someone", 261.0, true), KANA);
        exact.album_name = Some("Exact".into());
        let r = choose(Some(with_text(rec("Yoru no Uta", "Someone", 261.0, true), KANA)), Some(exact)).unwrap();
        assert_eq!(r.album_name.as_deref(), Some("Exact"));
        assert!(choose(None, Some(romaji)).is_some());
    }

    #[test]
    fn skips_romanization_uploads() {
        let rs = [with_text(rec("夜の歌 (Romanized)", "Genius Romanizations", 261.0, true), ROMAJI)];
        assert!(pick(&rs, &q("夜の歌", "誰か", 261), "夜の歌", "", false).is_none());
        let rs = [with_text(rec("Morning (中文翻譯)", "Someone", 200.0, true), ENGLISH)];
        assert!(pick(&rs, &q("Morning", "Someone", 200), "Morning", "Someone", false).is_none());
    }

    #[test]
    fn translation_in_the_real_title_is_fine() {
        let rs = [with_text(rec("Lost in Translation", "Someone", 200.0, true), ENGLISH)];
        let qq = q("Lost in Translation", "Someone", 200);
        assert!(pick(&rs, &qq, "Lost in Translation", "Someone", false).is_some());
    }

    #[test]
    fn credit_lines_do_not_hide_romaji() {
        let r = with_text(rec("夜の歌", "誰か", 261.0, true), &format!("[00:00.00]作词 : 某人\n{ROMAJI}"));
        assert!(romanized(&r));
    }

    #[test]
    fn language_versions_need_asking_for() {
        let rs = [with_text(rec("Morning (Japanese Ver.)", "Someone", 200.0, true), KANA)];
        assert!(pick(&rs, &q("Morning", "Someone", 200), "Morning", "Someone", false).is_none());
        let qq = q("Morning (Japanese Ver.)", "Someone", 200);
        assert!(pick(&rs, &qq, "Morning", "Someone", false).is_some());
    }

    #[test]
    fn empty_names_do_not_match_everything() {
        let qq = q("寂寞邊界", "張棟樑", 271);
        assert!(pick(&[rec("", "張棟樑", 271.0, true)], &qq, "寂寞邊界", "張棟樑", false).is_none());
        assert!(pick(&[rec("寂寞邊界", "", 271.0, true)], &qq, "寂寞邊界", "張棟樑", false).is_none());
    }
}
