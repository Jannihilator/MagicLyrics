//! Lyrics lookup: an ordered chain of providers plus an on-disk cache.
//!
//! - Chinese songs: LRCLIB
//! - Everything else: LRCLIB -> lyrics.ovh
//!
//! Time-synced lyrics win over plain ones; if no provider has synced lyrics the
//! first plain result is used. Chinese results in Simplified script are converted
//! to Traditional.

mod lrc;
mod lrclib;
mod lyricsovh;

use reqwest::Client;
use serde::{Deserialize, Serialize};

pub use lrc::parse_lrc;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Start time in ms; `None` for unsynced lyrics.
    pub time_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    pub source: String,
    pub synced: bool,
    pub instrumental: bool,
    pub lines: Vec<Line>,
    /// User sync correction for this song, added to the player position.
    #[serde(default)]
    pub offset_ms: i64,
}

impl Lyrics {
    pub fn plain(source: &str, text: &str) -> Option<Lyrics> {
        let lines: Vec<Line> = trim_blank_edges(
            text.lines().map(|l| Line { time_ms: None, text: l.trim().to_string() }).collect(),
        );
        if lines.iter().all(|l| l.text.is_empty()) {
            return None;
        }
        Some(Lyrics { source: source.into(), synced: false, instrumental: false, lines, offset_ms: 0 })
    }

    pub fn synced(source: &str, lrc_text: &str) -> Option<Lyrics> {
        let lines = parse_lrc(lrc_text);
        if lines.iter().filter(|l| !l.text.is_empty()).count() < 3 {
            return None;
        }
        Some(Lyrics { source: source.into(), synced: true, instrumental: false, lines, offset_ms: 0 })
    }

    pub fn instrumental(source: &str) -> Lyrics {
        Lyrics { source: source.into(), synced: false, instrumental: true, lines: vec![], offset_ms: 0 }
    }
}

fn trim_blank_edges(mut v: Vec<Line>) -> Vec<Line> {
    while v.first().is_some_and(|l| l.text.is_empty()) {
        v.remove(0);
    }
    while v.last().is_some_and(|l| l.text.is_empty()) {
        v.pop();
    }
    v
}

#[derive(Clone, Debug)]
pub struct Query {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

impl Query {
    /// First listed artist ("A, B" / "A & B" / "A feat. B" -> "A").
    pub fn primary_artist(&self) -> String {
        let a = self.artist.as_str();
        let cut = [", ", " & ", " feat", " ft.", " Feat", "、", " x ", " X "]
            .iter()
            .filter_map(|sep| a.find(sep))
            .min()
            .unwrap_or(a.len());
        a[..cut].trim().to_string()
    }

    /// Title without "(feat. X)", " - Remastered 2011", "[Live]" and similar decorations.
    pub fn clean_title(&self) -> String {
        clean_title(&self.title)
    }
}

pub fn clean_title(t: &str) -> String {
    let mut s = t.to_string();
    // " - Remastered 2009", " - Live at ...", " - Radio Edit"
    if let Some(i) = s.find(" - ") {
        s.truncate(i);
    }
    let re = regex::Regex::new(r"(?i)\s*[\(\[（【][^\)\]）】]*(feat|ft\.|with|remaster|live|version|edit|mix|mono|stereo|demo|acoustic|bonus|explicit|版|劇|主題曲|插曲)[^\)\]）】]*[\)\]）】]").unwrap();
    s = re.replace_all(&s, "").to_string();
    let s = s.trim();
    if s.is_empty() { t.trim().to_string() } else { s.to_string() }
}

/// Lowercase, strip spaces and punctuation; for fuzzy title/artist comparison.
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Han characters present and no Japanese kana / Korean hangul.
pub fn is_chinese(s: &str) -> bool {
    let mut han = false;
    for c in s.chars() {
        let u = c as u32;
        if (0x3040..=0x30FF).contains(&u) || (0xAC00..=0xD7AF).contains(&u) {
            return false;
        }
        if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
            han = true;
        }
    }
    han
}

#[derive(Clone, Copy, Debug)]
enum Provider {
    Lrclib,
    LyricsOvh,
}

pub enum Outcome {
    Found(Lyrics),
    /// Every provider answered and none had the song.
    NotFound,
    /// No provider could be reached; don't remember this as "not found".
    Offline,
}

async fn fetch(p: Provider, client: &Client, q: &Query) -> Result<Option<Lyrics>, String> {
    match p {
        Provider::Lrclib => lrclib::fetch(client, q).await,
        Provider::LyricsOvh => lyricsovh::fetch(client, q).await,
    }
}

/// Keep only the Han-script words, or only the Latin-script words, of a mixed
/// name such as "告白氣球 Love Confession" / "周杰倫 Jay Chou". `None` if not mixed.
fn script_part(s: &str, han: bool) -> Option<String> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let is_han = |w: &str| w.chars().any(|c| is_chinese(&c.to_string()));
    if !words.iter().any(|w| is_han(w)) || words.iter().all(|w| is_han(w)) {
        return None;
    }
    let kept: Vec<&str> = words.into_iter().filter(|w| is_han(w) == han).collect();
    let out = kept.join(" ").trim_matches(|c: char| !c.is_alphanumeric() && c != ')' && c != '.').to_string();
    (!out.is_empty()).then_some(out)
}

/// The query as given, then (for bilingual titles such as Chinese MVs on YouTube)
/// its Chinese-only and English-only forms, since databases list just one of them.
pub fn variants(q: &Query) -> Vec<Query> {
    let mut out = vec![q.clone()];
    for han in [true, false] {
        let title = script_part(&q.title, han);
        let artist = script_part(&q.artist, han);
        if title.is_none() && artist.is_none() {
            continue;
        }
        let v = Query {
            title: title.unwrap_or_else(|| q.title.clone()),
            artist: artist.unwrap_or_else(|| q.artist.clone()),
            album: String::new(),
            duration_ms: q.duration_ms,
        };
        if !out.iter().any(|o| o.title == v.title && o.artist == v.artist) {
            out.push(v);
        }
    }
    out
}

pub async fn find(client: &Client, q: &Query) -> Outcome {
    let chinese = is_chinese(&q.title) || is_chinese(&q.artist);
    let mut first_plain: Option<Lyrics> = None;
    let mut reached_any = false;
    for v in variants(q) {
        match find_one(client, &v, &mut first_plain, &mut reached_any).await {
            Some(l) => return Outcome::Found(finish(l, chinese)),
            None => {}
        }
    }
    match first_plain {
        Some(l) => Outcome::Found(finish(l, chinese)),
        None if reached_any => Outcome::NotFound,
        None => Outcome::Offline,
    }
}

/// Run the provider chain for one query; returns a synced/instrumental hit, and
/// remembers the first plain result in `first_plain`.
async fn find_one(
    client: &Client,
    q: &Query,
    first_plain: &mut Option<Lyrics>,
    reached_any: &mut bool,
) -> Option<Lyrics> {
    let chinese = is_chinese(&q.title) || is_chinese(&q.artist);
    let order: &[Provider] = if chinese {
        &[Provider::Lrclib]
    } else {
        &[Provider::Lrclib, Provider::LyricsOvh]
    };
    let debug = std::env::var_os("MAGICLYRICS_DEBUG").is_some();
    for &p in order {
        let r = fetch(p, client, q).await;
        if debug {
            let what = match &r {
                Ok(Some(l)) => format!("found synced={} lines={}", l.synced, l.lines.len()),
                Ok(None) => "none".into(),
                Err(e) => format!("error {e}"),
            };
            eprintln!("[lyrics] {p:?} for {:?} / {:?} ({} ms): {what}", q.artist, q.title, q.duration_ms);
        }
        match r {
            Ok(Some(l)) => {
                *reached_any = true;
                if l.synced || l.instrumental {
                    return Some(l);
                }
                first_plain.get_or_insert(l);
            }
            Ok(None) => *reached_any = true,
            Err(e) => eprintln!("[lyrics] {p:?} failed: {e}"),
        }
    }
    None
}

fn finish(mut l: Lyrics, chinese: bool) -> Lyrics {
    if !chinese {
        // Some uploads interleave Chinese translations; drop them when they're the minority.
        let non_empty = l.lines.iter().filter(|x| !x.text.is_empty()).count();
        let han = l.lines.iter().filter(|x| is_chinese(&x.text)).count();
        if han > 0 && han * 5 < non_empty * 3 {
            l.lines.retain(|x| !is_chinese(&x.text));
        }
    }
    if chinese {
        let all: String = l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n");
        if zhconv::is_hans(&all) {
            for line in &mut l.lines {
                line.text = zhconv::zhconv(&line.text, zhconv::Variant::ZhHant);
            }
        }
    }
    l
}

pub fn http_client() -> Client {
    Client::builder()
        .user_agent(concat!("MagicLyrics/", env!("CARGO_PKG_VERSION"), " (desktop lyrics overlay)"))
        .connect_timeout(std::time::Duration::from_secs(4))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("http client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chinese() {
        assert!(is_chinese("小幸運"));
        assert!(!is_chinese("Blinding Lights"));
        assert!(!is_chinese("夜に駆ける"));
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Yesterday - Remastered 2009"), "Yesterday");
        assert_eq!(clean_title("Stay (feat. Justin Bieber)"), "Stay");
        assert_eq!(clean_title("小幸運 (電影《我的少女時代》主題曲)"), "小幸運");
    }

    fn live(title: &str, artist: &str, album: &str, secs: u64) -> Outcome {
        let q = Query { title: title.into(), artist: artist.into(), album: album.into(), duration_ms: secs * 1000 };
        tauri::async_runtime::block_on(find(&http_client(), &q))
    }

    /// Network tests: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_english() {
        match live("I Hate Myself Sometimes", "Patrick Brasca", "I Hate Myself Sometimes", 175) {
            Outcome::Found(l) => {
                assert!(l.synced, "expected synced from {}", l.source);
                assert!(l.lines.iter().all(|x| !is_chinese(&x.text)));
            }
            _ => panic!("not found"),
        }
    }

    #[test]
    #[ignore]
    fn live_chinese_traditional() {
        match live("小幸運", "Hebe Tien", "小幸運", 266) {
            Outcome::Found(l) => {
                println!("source={} synced={}", l.source, l.synced);
                let all: String = l.lines.iter().map(|x| x.text.clone()).collect();
                assert!(!zhconv::is_hans(&all), "should be Traditional");
            }
            _ => panic!("not found"),
        }
    }

    #[test]
    #[ignore]
    fn live_artist_spelling_mismatch() {
        for (title, artist, secs) in [
            ("寂寞邊界", "张栋梁", 271),
            ("寂寞邊界", "Nicholas Teo", 271),
            ("甲乙丙丁", "Jacky Cheung", 241),
            ("甲乙丙丁", "郑中基", 244),
        ] {
            match live(title, artist, "", secs) {
                Outcome::Found(l) => println!("{title}/{artist}: {} synced={}", l.source, l.synced),
                _ => panic!("not found: {title} / {artist}"),
            }
        }
    }

    /// Common titles by an artist LRCLIB doesn't know must not borrow another
    /// artist's lyrics.
    #[test]
    #[ignore]
    fn live_no_other_artist_for_common_titles() {
        for (title, secs) in [("Hello", 230), ("Home", 200), ("Stay", 190), ("愛", 240), ("想你", 250)] {
            if let Outcome::Found(l) = live(title, "Nobody Real Band", "", secs) {
                panic!("{title}: got lyrics from {} ({:?}...)", l.source, l.lines.first().map(|x| &x.text));
            }
        }
    }

    #[test]
    fn bilingual_variants() {
        let q = Query { title: "告白氣球 Love Confession".into(), artist: "周杰倫 Jay Chou".into(), album: "".into(), duration_ms: 215_000 };
        let v: Vec<(String, String)> = variants(&q).into_iter().map(|q| (q.artist, q.title)).collect();
        assert_eq!(v, vec![
            ("周杰倫 Jay Chou".into(), "告白氣球 Love Confession".into()),
            ("周杰倫".into(), "告白氣球".into()),
            ("Jay Chou".into(), "Love Confession".into()),
        ]);
        let plain = Query { title: "Anti-Hero".into(), artist: "Taylor Swift".into(), album: "".into(), duration_ms: 0 };
        assert_eq!(variants(&plain).len(), 1);
    }

    /// YouTube-style metadata through the song detector and the full lookup.
    #[test]
    #[ignore]
    fn live_youtube() {
        for (title, channel, secs) in [
            ("Taylor Swift - Anti-Hero (Official Music Video)", "TaylorSwiftVEVO", 201),
            ("周杰倫 Jay Chou【告白氣球 Love Confession】-Official Music Video", "周杰倫 Jay Chou", 215),
        ] {
            let g = crate::media::browser::classify(title, channel, "", secs * 1000);
            assert!(g.music);
            match live(&g.title, &g.artist, "", secs) {
                Outcome::Found(l) => println!("{title}: {} synced={} lines={}", l.source, l.synced, l.lines.len()),
                _ => panic!("not found: {g:?}"),
            }
        }
    }

    #[test]
    fn primary_artist() {
        let q = Query { title: "x".into(), artist: "Kid LAROI, Justin Bieber".into(), album: "".into(), duration_ms: 0 };
        assert_eq!(q.primary_artist(), "Kid LAROI");
    }
}
