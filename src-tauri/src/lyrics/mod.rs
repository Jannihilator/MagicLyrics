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
mod script;

use reqwest::Client;
use serde::{Deserialize, Serialize};

pub use lrc::parse_lrc;
pub use script::{class, cjk_names, groups, has_cjk, has_kana_hangul, is_mixed, is_paired, looks_romanized, Class};

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
    class(s) == Class::Han
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

/// Artist and title swapped, for uploads titled song-first ("晴天 - 周杰倫",
/// "【周杰倫】晴天") that the song detector reads the wrong way round. Only for
/// tracks without an album: players that report one (Spotify, YouTube Music
/// official tracks) give artist and title in their proper fields.
fn swapped(q: &Query) -> Option<Query> {
    let (t, a) = (q.title.trim(), q.artist.trim());
    (q.album.trim().is_empty() && !t.is_empty() && !a.is_empty() && normalize(t) != normalize(a)).then(|| Query {
        title: a.to_string(),
        artist: t.to_string(),
        album: String::new(),
        duration_ms: q.duration_ms,
    })
}

pub async fn find(client: &Client, q: &Query) -> Outcome {
    let mut first_plain: Option<Lyrics> = None;
    let mut reached_any = false;
    for v in variants(q) {
        match find_one(client, &v, &mut first_plain, &mut reached_any).await {
            Some(l) => return Outcome::Found(finish(l, q)),
            None => {}
        }
    }
    // Nothing at all under the given order: try it the other way round.
    if first_plain.is_none() {
        for v in swapped(q).iter().flat_map(variants) {
            match find_one(client, &v, &mut first_plain, &mut reached_any).await {
                Some(l) => return Outcome::Found(finish(l, q)),
                None => {}
            }
        }
    }
    match first_plain {
        Some(l) => Outcome::Found(finish(l, q)),
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

/// What the lyrics themselves say about the song's language.
#[derive(Debug)]
struct SongScript {
    /// A Chinese, Japanese or Korean song (see [`song_script`]).
    cjk: bool,
    /// Chinese in particular: CJK, with kana / hangul in under a fifth of the lines
    /// (Japanese artists can have all-kanji names, such as 米津玄師).
    chinese: bool,
}

/// The script most (70%+) of `classes` share: `Some(true)` for CJK, `Some(false)`
/// for Latin; `None` when there are under three or they're mixed.
fn vote(classes: &[Class]) -> Option<bool> {
    let n = classes.len();
    let cjk = classes.iter().filter(|c| matches!(c, Class::Han | Class::KanaHangul)).count();
    if n < 3 {
        None
    } else if cjk * 10 >= n * 7 {
        Some(true)
    } else if (n - cjk) * 10 >= n * 7 {
        Some(false)
    } else {
        None
    }
}

/// Is this a Chinese / Japanese / Korean song? Evidence, strongest first:
/// 1. Latin lines that read as a romanization (romaji, pinyin…): yes.
/// 2. In files pairing each line with its translation or romanization, the
///    original conventionally comes first: the script that leads most pairs. (So a
///    file that puts a Chinese translation *before* each English line is read as a
///    Chinese song; that layout is rare.)
/// 3. Lines with a timestamp to themselves, which have nothing beside them: their
///    majority script. Only a tie-breaker, since untranslated lines are often
///    English hooks ("Oh baby") in a Korean or Chinese song.
/// 4. The player's names.
fn song_script(lines: &[Line], q: &Query) -> SongScript {
    let latin: Vec<&str> = lines.iter().map(|x| x.text.as_str()).filter(|t| class(t) == Class::Latin).collect();
    let mut single = Vec::new();
    let mut leading = Vec::new();
    for g in groups(lines) {
        let cs = script::classes(g);
        if cs.len() == 1 {
            single.push(cs[0]);
        } else if is_mixed(g) {
            leading.push(cs[0]);
        }
    }
    let cjk = looks_romanized(&latin.join("\n")) || vote(&leading).or_else(|| vote(&single)).unwrap_or(cjk_names(q));
    let non_empty = lines.iter().filter(|x| !x.text.is_empty()).count();
    let jk = lines.iter().filter(|x| has_kana_hangul(&x.text)).count();
    let chinese = !(jk > 0 && jk * 5 >= non_empty) && (cjk || is_chinese(&q.title) || is_chinese(&q.artist));
    SongScript { cjk, chinese }
}

/// Files that pair their lines with a translation or romanization ([`is_paired`])
/// keep one line per timestamp: for a CJK song the CJK line (Han first for Chinese,
/// kana / hangul first otherwise); for any other song the Latin one, since the CJK
/// line is then a translation. Lines in the same script (a duet, two short lines)
/// all stay, as does everything in files that aren't paired (an English ad-lib
/// beside a Korean line). Blank lines sharing a timestamp with text go.
fn drop_paired_lines(lines: &mut Vec<Line>, song: &SongScript) {
    let rank = |x: &Line| -> u8 {
        match class(&x.text) {
            Class::Blank => 0,
            Class::Latin => if song.cjk { 1 } else { 4 },
            Class::KanaHangul => if song.chinese { 2 } else { 3 },
            Class::Han => if song.chinese { 3 } else { 2 },
        }
    };
    let paired = is_paired(lines);
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    for g in groups(lines) {
        let text: Vec<&Line> = g.iter().filter(|x| class(&x.text) != Class::Blank).collect();
        if text.is_empty() {
            out.extend(g.iter().cloned());
        } else if paired && is_mixed(g) {
            // max_by_key keeps the last of equals; ties go to the earlier line.
            out.push(text.iter().rev().max_by_key(|x| rank(x)).map(|x| (*x).clone()).unwrap());
        } else {
            out.extend(text.into_iter().cloned());
        }
    }
    *lines = out;
}

/// Clean-up applied to every result, and again to cached ones (which may predate a
/// rule). Running it twice changes nothing: once pairs are gone, the remaining lines
/// carry the same verdict on the song's script.
pub fn finish(mut l: Lyrics, q: &Query) -> Lyrics {
    let song = song_script(&l.lines, q);
    if l.synced {
        drop_paired_lines(&mut l.lines, &song);
    }
    // Some uploads interleave Chinese translations; drop them when they're the
    // minority. Not for songs with Chinese-script names, whose Han-only lines may be
    // the original (Japanese written in kanji, by 米津玄師 say).
    if !song.chinese && !is_chinese(&q.title) && !is_chinese(&q.artist) {
        let non_empty = l.lines.iter().filter(|x| !x.text.is_empty()).count();
        let han = l.lines.iter().filter(|x| is_chinese(&x.text)).count();
        if han > 0 && han * 5 < non_empty * 3 {
            l.lines.retain(|x| !is_chinese(&x.text));
        }
    }
    if song.chinese {
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

    /// Exactly what YouTube Music reports (media session title / artist / album)
    /// for three uploads of 寂寞邊界, through the song detector and the lookup.
    #[test]
    #[ignore]
    fn live_youtube_music_uploads() {
        for (title, channel, album, secs) in [
            ("張棟樑-寂寞邊界", "林music BST", "", 270),
            ("Loneliness", "Nicholas Teo", "Nicholas Greatest Hits Compilation", 272),
            ("張棟樑《寂寞邊界》【歌詞】", "魚魚愛音樂", "", 264),
        ] {
            let g = crate::media::browser::classify(title, channel, album, secs * 1000);
            assert!(g.music, "{title}");
            match live(&g.title, &g.artist, album, secs) {
                Outcome::Found(l) => println!("{title} -> {}/{}: {} synced={}", g.artist, g.title, l.source, l.synced),
                _ => panic!("not found: {title} -> {g:?}"),
            }
        }
    }

    /// Song-first upload titles, read by the detector as artist = song.
    #[test]
    #[ignore]
    fn live_song_first_uploads() {
        for (title, secs) in [("晴天-周杰倫", 269), ("晴天 - 周杰倫", 269), ("【周杰倫】晴天", 269), ("寂寞邊界-張棟樑", 271)] {
            let g = crate::media::browser::classify(title, "Random Uploader", "", secs * 1000);
            match live(&g.title, &g.artist, "", secs) {
                Outcome::Found(l) => println!("{title} -> {}/{}: {} synced={}", g.artist, g.title, l.source, l.synced),
                _ => panic!("not found: {title} -> {g:?}"),
            }
        }
    }

    /// Japanese / Korean songs come back in kana / hangul, not romanized.
    #[test]
    #[ignore]
    fn live_original_script() {
        for (title, artist, secs) in [
            ("夜に駆ける", "YOASOBI", 261),
            ("アイドル", "YOASOBI", 213),
            ("Lemon", "米津玄師", 255),
            ("밤양갱", "BIBI", 146),
            // Romanized names, as some players report them.
            ("Yoru ni Kakeru", "YOASOBI", 261),
        ] {
            match live(title, artist, "", secs) {
                Outcome::Found(l) => {
                    let all: String = l.lines.iter().map(|x| x.text.clone()).collect();
                    println!("{title}: {} synced={} lines={}", l.source, l.synced, l.lines.len());
                    assert!(has_kana_hangul(&all), "{title}: romanized");
                }
                _ => panic!("not found: {title}"),
            }
        }
    }

    #[test]
    fn swap_only_without_album() {
        let q = Query { title: "周杰倫".into(), artist: "晴天".into(), album: "".into(), duration_ms: 0 };
        let s = swapped(&q).unwrap();
        assert_eq!((s.artist.as_str(), s.title.as_str()), ("周杰倫", "晴天"));
        assert!(swapped(&Query { album: "葉惠美".into(), ..q.clone() }).is_none());
        assert!(swapped(&Query { artist: "".into(), ..q }).is_none());
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

    fn timed(lines: &[(u64, &str)]) -> Vec<Line> {
        lines.iter().map(|(t, s)| Line { time_ms: Some(*t), text: s.to_string() }).collect()
    }

    fn tidy(title: &str, artist: &str, lines: &[(u64, &str)]) -> Vec<String> {
        let l = Lyrics { source: "t".into(), synced: true, instrumental: false, lines: timed(lines), offset_ms: 0 };
        let q = Query { title: title.into(), artist: artist.into(), album: "".into(), duration_ms: 0 };
        finish(l, &q).lines.into_iter().map(|x| x.text).collect()
    }

    // Made-up lines (not real lyrics).
    const JA: [&str; 3] = ["朝の光に手を伸ばして", "君と歩いた帰り道", "ずっと一緒にいてくれるかな"];
    const ROMAJI: [&str; 3] = ["Asa no hikari ni te wo nobashite", "Kimi to aruita kaerimichi", "Zutto issho ni ite kureru kana"];
    const ZH: [&str; 3] = ["我在這裡等你回來", "窗外的雨下不停", "想念你的晚上"];
    const PINYIN: [&str; 3] = ["Wo zai zhe li deng ni hui lai", "Chuang wai de yu xia bu ting", "Xiang nian ni de wan shang"];
    const KO: [&str; 3] = ["조용한 아침을 천천히 걸어", "부엌 식탁 위에 쏟아진 커피", "이 밤이 이렇게 끝날 줄 몰랐어"];
    const EN: [&str; 3] = ["Walking slowly through the quiet morning", "Coffee spilled across the kitchen table", "I never thought the night would end like this"];

    /// Pair `a[i]` and `b[i]` under the same timestamp, `a` first.
    fn paired<'a>(a: &[&'a str; 3], b: &[&'a str; 3]) -> Vec<(u64, &'a str)> {
        (0..3).flat_map(|i| [(i as u64 * 1000, a[i]), (i as u64 * 1000, b[i])]).collect()
    }

    #[test]
    fn paired_lines_keep_original_script() {
        // Japanese + romaji, either order, whether or not the names are Japanese.
        for (title, artist) in [("夜の歌", "誰か"), ("Yoru no Uta", "Someone")] {
            assert_eq!(tidy(title, artist, &paired(&JA, &ROMAJI)), JA, "{title}");
            assert_eq!(tidy(title, artist, &paired(&ROMAJI, &JA)), JA, "{title}");
        }
        // Chinese + pinyin, also under English names.
        assert_eq!(tidy("等你", "某人", &paired(&PINYIN, &ZH)), ZH);
        assert_eq!(tidy("Waiting", "Someone", &paired(&PINYIN, &ZH)), ZH);
        // Japanese + Chinese translation: kana wins, even for an all-kanji artist name.
        assert_eq!(tidy("夜の歌", "米津玄師", &paired(&ZH, &JA)), JA);
        // English + translation after each line: English stays.
        for tr in [&ZH, &JA] {
            assert_eq!(tidy("Morning", "Someone", &paired(&EN, tr)), EN);
        }
        // Chinese or Japanese song under English names, English translation after
        // each line: the original stays.
        assert_eq!(tidy("Waiting", "Someone", &paired(&ZH, &EN)), ZH);
        assert_eq!(tidy("Morning", "Someone", &paired(&JA, &EN)), JA);
        // English song by an artist with a hangul name, Korean translation after each line.
        assert_eq!(tidy("Morning", "아무개", &paired(&EN, &KO)), EN);
        // Korean song with untranslated English hooks: the pairs' order decides.
        let mut v: Vec<(u64, &str)> = paired(&KO, &EN);
        v.extend([(5000, "Oh baby"), (6000, "Yeah yeah yeah"), (7000, "Come on now")]);
        assert_eq!(tidy("Morning", "Someone", &v)[..3], KO);
        assert_eq!(tidy("밤의 노래", "아무개", &v)[..3], KO);
        // No consistent order: untranslated lines break the tie.
        let mut v = vec![(0, EN[0]), (0, JA[0]), (1000, JA[1]), (1000, EN[1]), (2000, EN[2]), (2000, JA[2])];
        v.extend([(5000, "夜の道を一人で"), (6000, "名前を呼んで"), (7000, "まだ遠い")]);
        assert_eq!(tidy("Morning", "Someone", &v)[..3], JA);
        // A blank line sharing a timestamp never replaces text.
        assert_eq!(tidy("Morning", "Someone", &[(1000, "Line"), (1000, "")]), ["Line"]);
    }

    #[test]
    fn unpaired_files_keep_ad_libs() {
        let v = [(0, KO[0]), (1000, KO[1]), (1000, "(Yeah)"), (2000, KO[2]), (3000, "하늘 아래"), (4000, "너와 함께")];
        assert_eq!(tidy("밤의 노래", "아무개", &v).len(), 6);
    }

    #[test]
    fn kanji_only_lines_of_japanese_songs_stay() {
        let v = [(0, JA[0]), (1000, "運命"), (2000, JA[1]), (3000, "永遠"), (4000, JA[2])];
        assert_eq!(tidy("Lemon", "米津玄師", &v).len(), 5);
    }

    #[test]
    fn same_script_lines_at_one_time_all_stay() {
        let v = [(1000, "Walking slowly"), (1000, "Through the morning"), (2000, "朝の光に"), (2000, "君と歩いた")];
        assert_eq!(tidy("Morning", "Someone", &v), ["Walking slowly", "Through the morning", "朝の光に", "君と歩いた"]);
    }

    #[test]
    fn finish_twice_is_finish_once() {
        for (title, artist, lines) in [
            ("Waiting", "Someone", paired(&PINYIN, &ZH)),
            ("Morning", "Someone", paired(&EN, &ZH)),
            ("Yoru no Uta", "Someone", paired(&JA, &ROMAJI)),
        ] {
            let once = tidy(title, artist, &lines);
            let refs: Vec<(u64, &str)> = (0..once.len()).map(|i| (i as u64 * 1000, once[i].as_str())).collect();
            assert_eq!(tidy(title, artist, &refs), once, "{title}");
        }
    }

    #[test]
    fn primary_artist() {
        let q = Query { title: "x".into(), artist: "Kid LAROI, Justin Bieber".into(), album: "".into(), duration_ms: 0 };
        assert_eq!(q.primary_artist(), "Kid LAROI");
    }
}
