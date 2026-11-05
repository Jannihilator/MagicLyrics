//! Mojim 魔鏡歌詞網 (https://mojim.com): Taiwanese lyrics site, Traditional Chinese.
//!
//! Scrapes the public song-title search page and the song page. Many Mojim pages
//! append LRC timestamps after the lyrics; those are used for sync when present.
//! NOTE: written against Mojim's known page layout; as of 2026-11 the domain's DNS
//! points at a private address, so this provider fails fast and the chain moves on.

use super::{normalize, Line, Lyrics, Query};
use regex::Regex;
use reqwest::Client;

const BASE: &str = "https://mojim.com";
const SOURCE: &str = "Mojim 魔鏡歌詞網";

pub async fn fetch(client: &Client, q: &Query) -> Result<Option<Lyrics>, String> {
    let title = q.clean_title();
    // "?t3" = search by song title.
    let url = format!("{BASE}/{}.html?t3", urlencoding::encode(&title));
    let html = get(client, &url).await?;

    let Some(path) = pick_song(&html, &title, &q.primary_artist()) else {
        return Ok(None);
    };
    let page = get(client, &format!("{BASE}{path}")).await?;
    Ok(parse_song_page(&page, &title))
}

async fn get(client: &Client, url: &str) -> Result<String, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

/// Find the song-page link whose title and artist match, from the search results.
fn pick_song(html: &str, title: &str, artist: &str) -> Option<String> {
    let row_re = Regex::new(r#"(?s)<dd class="mxsh_dd\d"[^>]*>(.*?)</dd>"#).unwrap();
    let artist_re = Regex::new(r#"href="/\w{2}h\d+\.htm"[^>]*>([^<]+)<"#).unwrap();
    let song_re = Regex::new(r#"href="(/\w{2}y[\dx]+\.htm)"[^>]*>([^<]+)<"#).unwrap();
    let want_t = normalize(title);
    let want_a = normalize(artist);

    let title_ok = |text: &str| {
        let text = text.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
        let t = normalize(&super::clean_title(&html_unescape(text)));
        !t.is_empty() && (t == want_t || t.contains(&want_t))
    };

    let mut title_only: Option<String> = None;
    for row in row_re.captures_iter(html) {
        let row = &row[1];
        let a = artist_re.captures(row).map(|c| normalize(&html_unescape(&c[1]))).unwrap_or_default();
        for s in song_re.captures_iter(row) {
            if !title_ok(&s[2]) {
                continue;
            }
            if !want_a.is_empty() && !a.is_empty() && (a.contains(&want_a) || want_a.contains(&a)) {
                return Some(s[1].to_string());
            }
            title_only.get_or_insert_with(|| s[1].to_string());
        }
    }
    // Only accept a title-only match when we don't know the artist.
    if want_a.is_empty() { title_only } else { None }
}

fn parse_song_page(html: &str, title: &str) -> Option<Lyrics> {
    let body_re = Regex::new(r#"(?s)<dd id="fsZx3"[^>]*>(.*?)</dd>"#).unwrap();
    let body = body_re.captures(html)?.get(1)?.as_str();
    let br = Regex::new(r"(?i)<br\s*/?>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]+>").unwrap();
    let text = html_unescape(&tags.replace_all(&br.replace_all(body, "\n"), ""));

    let header = Regex::new(r"^\s*(作詞|作曲|編曲|監製|製作人|演唱|原唱|主唱|OP|SP)\s*[:：]").unwrap();
    let want_t = normalize(title);
    let mut plain = Vec::new();
    let mut lrc = String::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.contains("Mojim") || line.contains("魔鏡歌詞網") || line.contains("更多更詳盡歌詞") {
            continue;
        }
        if line.starts_with('[') {
            lrc.push_str(line);
            lrc.push('\n');
            continue;
        }
        if header.is_match(line) || (i < 2 && normalize(line) == want_t) {
            continue;
        }
        plain.push(Line { time_ms: None, text: line.to_string() });
    }
    if let Some(synced) = Lyrics::synced(SOURCE, &lrc) {
        return Some(synced);
    }
    let joined: Vec<String> = plain.into_iter().map(|l| l.text).collect();
    Lyrics::plain(SOURCE, &joined.join("\n"))
}

fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_matching_row() {
        let html = r#"<dd class="mxsh_dd1"><span class="mxsh_ss2"><a href="/twh100004.htm" title="田馥甄">田馥甄</a></span><span class="mxsh_ss4"><a href="/twy100004x26x1.htm" title="小幸運 歌詞">1.小幸運</a></span></dd>
        <dd class="mxsh_dd2"><span class="mxsh_ss2"><a href="/twh999.htm">Other</a></span><span class="mxsh_ss4"><a href="/twy999x1x1.htm">2.小幸運</a></span></dd>"#;
        assert_eq!(pick_song(html, "小幸運", "田馥甄").as_deref(), Some("/twy100004x26x1.htm"));
        assert_eq!(pick_song(html, "小幸運", "Nobody"), None);
    }

    #[test]
    fn parses_page_with_lrc() {
        let html = r#"<dd id="fsZx3" class="fsZx3">小幸運<br />作詞：徐世珍<br />我聽見雨滴<br />更多更詳盡歌詞 在 <a href="http://mojim.com">※ Mojim.com　魔鏡歌詞網 </a><br />[00:10.00]我聽見雨滴<br />[00:15.00]落在青青草地<br />[00:20.00]我聽見遠方<br /></dd>"#;
        let l = parse_song_page(html, "小幸運").unwrap();
        assert!(l.synced);
        assert_eq!(l.lines[0].text, "我聽見雨滴");
        assert_eq!(l.lines[1].time_ms, Some(15000));
    }
}
