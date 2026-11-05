//! Media playing in a web browser (YouTube, YouTube Music, ...).
//!
//! Browsers pass on whatever the page reports: YouTube Music gives clean
//! title / artist / album, but plain YouTube gives the *video* title and the
//! *channel* name ("Taylor Swift - Anti-Hero (Official Music Video)" by
//! "TaylorSwiftVEVO"), and most browser media isn't music at all.
//!
//! [`refine`] decides whether a browser track looks like a song and, if so,
//! recovers artist and title. Tracks from music players pass through untouched.

use super::Track;
use regex::Regex;
use std::sync::OnceLock;

/// Substrings of the source app id (Windows AppUserModelId / macOS bundle id).
const BROWSERS: &[&str] = &[
    "chrome", "chromium", "msedge", "firefox", "308046b0af4a39cb", "brave", "opera", "vivaldi",
    "thebrowsercompany", "safari", "zen", "waterfox", "librewolf", "floorp", "yandex", "webkit",
];

pub fn is_browser(source_app: &str) -> bool {
    let s = source_app.to_lowercase();
    BROWSERS.iter().any(|b| s.contains(b))
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

/// Bracketed decorations that mark a music upload: "(Official Video)", "[MV]", "【歌詞】"…
fn tag_group() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\s*[\(\[（【［][^\)\]）】］]*?(official|video|audio|lyric|visuali[sz]er|\bmv\b|m/v|\bhd\b|\bhq\b|\b4k\b|1080p|720p|color.?coded|\bsub\b|subs|subtitle|歌詞|动态歌词|動態歌詞|完整版|官方|高音質|高音质|中字|字幕|中英|韓中|日中|\bcc\b)[^\)\]）】］]*[\)\]）】］]")
}

/// The same decorations without brackets, at the end: "Song Official Music Video".
fn tag_tail() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)[\s\-|｜/]*(official\s+(music\s+)?(video|audio|mv|lyric\s+video|visuali[sz]er)|music\s+video|lyric\s+video|lyrics?|\bm/?v\b|官方MV|官方版|動態歌詞|歌詞版?)\s*$")
}

/// Things that are clearly not a single song.
fn not_song() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)podcast|episode|\bep\.?\s*\d|reaction|reacts?\s+to|tutorial|how\s+to|review|trailer|teaser|vlog|unboxing|gameplay|walkthrough|let'?s\s+play|highlights|interview|live\s?stream|#shorts|full\s+album|playlist|compilation|直播|開箱|开箱|教學|教学|實況|实况|預告|预告|訪問|访问|新聞|新闻|合輯|合辑|歌單|歌单|精選|精选|串燒|串烧")
}

/// "Taylor Swift - Anti-Hero" separators (spaced, so "Anti-Hero" stays whole).
const DASHES: &[&str] = &[" - ", " – ", " — ", " ‒ ", " － ", "－"];

/// Channel name -> artist: drop " - Topic", "VEVO", "Official…", split CamelCase.
pub fn clean_channel(c: &str) -> String {
    let mut s = c.trim().to_string();
    for suf in [" - Topic", "VEVO", "Vevo", " Official YouTube Channel", " Official Channel", " Official", "Official", "官方頻道", "官方频道", " 官方"] {
        if s.len() > suf.len() && s.ends_with(suf) {
            s.truncate(s.len() - suf.len());
            s = s.trim().to_string();
        }
    }
    if c.contains("VEVO") && !s.contains(' ') {
        // "TaylorSwift" -> "Taylor Swift"
        let mut out = String::new();
        let mut prev_lower = false;
        for ch in s.chars() {
            if ch.is_uppercase() && prev_lower {
                out.push(' ');
            }
            prev_lower = ch.is_lowercase();
            out.push(ch);
        }
        s = out;
    }
    s
}

fn tidy(s: &str) -> String {
    s.trim_matches(|c: char| c.is_whitespace() || "-–—|｜/:：,".contains(c)).to_string()
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

#[derive(Debug, PartialEq)]
pub struct Guess {
    pub music: bool,
    pub artist: String,
    pub title: String,
}

/// Classify browser media and pull artist / title out of a video-style title.
pub fn classify(title: &str, channel: &str, album: &str, duration_ms: u64) -> Guess {
    let raw = title.trim();
    let mut t = raw.to_string();
    for suf in [" - YouTube Music", " - YouTube"] {
        if let Some(stripped) = t.strip_suffix(suf) {
            t = stripped.to_string();
        }
    }
    let channel = channel.trim();
    let no = |t: &str| Guess { music: false, artist: channel.to_string(), title: t.to_string() };

    let secs = duration_ms / 1000;
    if duration_ms > 0 && !(30..=15 * 60).contains(&secs) {
        return no(&t);
    }
    if not_song().is_match(&t) {
        return no(&t);
    }

    // YouTube Music songs and auto-generated "Artist - Topic" uploads are already clean.
    if (!album.trim().is_empty() || channel.ends_with(" - Topic")) && !channel.is_empty() {
        return Guess { music: true, artist: clean_channel(channel), title: tidy(&t) };
    }

    // Strip "(Official Video)" etc.; finding one is a strong hint this is music.
    let mut tagged = false;
    let before = t.clone();
    t = tag_group().replace_all(&t, "").to_string();
    loop {
        let next = tag_tail().replace(&t, "").to_string();
        if next == t {
            break;
        }
        t = next;
    }
    tagged |= t != before;
    if let Some(i) = t.find(" | ").or_else(|| t.find('｜')).or_else(|| t.find(" // ")) {
        t.truncate(i);
    }
    let t = tidy(&t);

    let channel_artist = clean_channel(channel);
    let (artist, song, structured) = if let Some((title, outside)) = cjk_bracketed(&t) {
        // 周杰倫 Jay Chou【告白氣球 Love Confession】
        let a = if outside.is_empty() { channel_artist.clone() } else { outside };
        (a, title, true)
    } else if let Some((left, right)) = DASHES.iter().find_map(|d| t.split_once(d)) {
        let (l, r) = (tidy(left), tidy(right));
        // Usually "Artist - Song"; swap when the right side is the channel's name.
        let ch = norm(&channel_artist);
        let r_is_artist = !ch.is_empty() && norm(&r) == ch && norm(&l) != ch;
        if r_is_artist { (r, l, true) } else { (l, r, true) }
    } else {
        (channel_artist.clone(), t.clone(), false)
    };

    if song.is_empty() || artist.is_empty() {
        return no(raw);
    }
    // No artist/title structure and no music tags: only plausible-length tracks.
    let strong = structured || tagged || channel.contains("VEVO") || channel.ends_with(" - Topic");
    if !strong && duration_ms > 0 && !(60..=8 * 60).contains(&secs) {
        return no(raw);
    }
    Guess { music: true, artist, title: song }
}

/// Text inside 【】「」『』《》 and the text around it.
fn cjk_bracketed(t: &str) -> Option<(String, String)> {
    for (open, close) in [('【', '】'), ('「', '」'), ('『', '』'), ('《', '》')] {
        let (Some(a), Some(b)) = (t.find(open), t.find(close)) else { continue };
        if b <= a {
            continue;
        }
        let inner = tidy(&t[a + open.len_utf8()..b]);
        if inner.is_empty() {
            continue;
        }
        let outside = tidy(&format!("{} {}", &t[..a], &t[b + close.len_utf8()..]));
        return Some((inner, outside));
    }
    None
}

/// Mark the track as music or not, rewriting browser video metadata into artist / title.
pub fn refine(mut t: Track) -> Track {
    if !is_browser(&t.source_app) {
        t.music = true;
        return t;
    }
    let g = classify(&t.title, &t.artist, &t.album, t.duration_ms);
    t.music = g.music;
    if g.music {
        t.artist = g.artist;
        t.title = g.title;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, channel: &str, secs: u64) -> (String, String) {
        let g = classify(title, channel, "", secs * 1000);
        assert!(g.music, "expected a song: {title:?} / {channel:?} -> {g:?}");
        (g.artist, g.title)
    }

    fn not(title: &str, channel: &str, secs: u64) {
        let g = classify(title, channel, "", secs * 1000);
        assert!(!g.music, "expected not a song: {title:?} / {channel:?}");
    }

    #[test]
    fn browsers() {
        assert!(is_browser("Chrome"));
        assert!(is_browser("MSEdge"));
        assert!(is_browser("308046B0AF4A39CB"));
        assert!(is_browser("com.google.Chrome"));
        assert!(!is_browser("Spotify.exe"));
        assert!(!is_browser("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"));
    }

    #[test]
    fn vevo_and_dash() {
        assert_eq!(
            song("Taylor Swift - Anti-Hero (Official Music Video)", "TaylorSwiftVEVO", 290),
            ("Taylor Swift".into(), "Anti-Hero".into())
        );
        assert_eq!(
            song("The Weeknd - Blinding Lights (Official Audio)", "TheWeekndVEVO", 203),
            ("The Weeknd".into(), "Blinding Lights".into())
        );
        assert_eq!(
            song("Queen – Bohemian Rhapsody [Official Video Remastered]", "Queen Official", 360),
            ("Queen".into(), "Bohemian Rhapsody".into())
        );
    }

    #[test]
    fn song_first_order_is_swapped() {
        assert_eq!(song("Shape of You - Ed Sheeran", "Ed Sheeran", 263), ("Ed Sheeran".into(), "Shape of You".into()));
    }

    #[test]
    fn channel_as_artist() {
        assert_eq!(song("Anti-Hero (Official Lyric Video)", "Taylor Swift", 200), ("Taylor Swift".into(), "Anti-Hero".into()));
        assert_eq!(song("drivers license", "Olivia Rodrigo", 242), ("Olivia Rodrigo".into(), "drivers license".into()));
        assert_eq!(song("Someone Like You", "Adele - Topic", 285), ("Adele".into(), "Someone Like You".into()));
    }

    #[test]
    fn youtube_music_passes_through() {
        let g = classify("Anti-Hero", "Taylor Swift", "Midnights", 200_000);
        assert_eq!(g, Guess { music: true, artist: "Taylor Swift".into(), title: "Anti-Hero".into() });
    }

    #[test]
    fn chinese_mv() {
        assert_eq!(
            song("周杰倫 Jay Chou【告白氣球 Love Confession】-Official Music Video", "周杰倫 Jay Chou", 215),
            ("周杰倫 Jay Chou".into(), "告白氣球 Love Confession".into())
        );
        assert_eq!(
            song("田馥甄 Hebe Tien《小幸運 A Little Happiness》官方MV", "華研國際", 270),
            ("田馥甄 Hebe Tien".into(), "小幸運 A Little Happiness".into())
        );
        assert_eq!(song("【MV】告白氣球", "周杰倫 Jay Chou", 215), ("周杰倫 Jay Chou".into(), "告白氣球".into()));
        assert_eq!(song("鄧紫棋 G.E.M. - 光年之外 LIGHT YEARS AWAY (動態歌詞)", "G.E.M. 鄧紫棋", 235).1, "光年之外 LIGHT YEARS AWAY");
    }

    #[test]
    fn rejects_non_music() {
        not("I tried every fast food burger | Review", "Some Foodie", 900);
        not("Building a PC - Full Tutorial", "Tech Guy", 1500);
        not("Morning routine vlog", "Daily Me", 420);
        not("Lo-fi beats to study to - 24/7 live stream", "Lofi Girl", 0);
        not("Best of 2024 Playlist", "Music Hits", 3600);
        not("Funny cat", "Cats", 20);
        not("Game Trailer - Official", "Studio", 120);
        not("Some podcast talk", "Pod", 0);
    }

    #[test]
    fn unstructured_long_video_is_not_a_song() {
        // No "Artist - Song", no tags, and 12 minutes long.
        not("What I learned this year", "Jane Doe", 720);
    }

    #[test]
    fn players_pass_through() {
        let t = Track {
            title: "Song - Remastered 2011".into(),
            artist: "Band".into(),
            album: "".into(),
            duration_ms: 1000,
            source_app: "Spotify.exe".into(),
            music: false,
        };
        let r = refine(t);
        assert!(r.music);
        assert_eq!(r.title, "Song - Remastered 2011");
    }
}
