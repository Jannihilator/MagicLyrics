//! lyrics.ovh: keyless plain-text lyrics, used as the English fallback.

use super::{Lyrics, Query};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

#[derive(Deserialize)]
struct Resp {
    lyrics: Option<String>,
}

pub async fn fetch(client: &Client, q: &Query) -> Result<Option<Lyrics>, String> {
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        urlencoding::encode(&q.primary_artist()),
        urlencoding::encode(&q.clean_title())
    );
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let r: Resp = resp.json().await.map_err(|e| e.to_string())?;
    let Some(text) = r.lyrics else { return Ok(None) };
    let text = text.replace("\r\n", "\n");
    // The API sometimes prefixes a French header line.
    let text = match text.split_once('\n') {
        Some((first, rest)) if first.starts_with("Paroles de la chanson") => rest.to_string(),
        _ => text,
    };
    Ok(Lyrics::plain("lyrics.ovh", &text))
}
