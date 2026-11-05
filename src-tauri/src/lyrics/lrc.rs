//! Minimal LRC parser: "[mm:ss.xx]text", multiple tags per line allowed.

use super::Line;
use regex::Regex;
use std::sync::OnceLock;

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]").unwrap())
}

fn credit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*(作词|作詞|作曲|编曲|編曲|制作人|製作人|监制|監製|混音|母带|母帶|和声|和聲|吉他|贝斯|貝斯|鼓|弦乐|弦樂|录音|錄音|出品|发行|發行|OP|SP|Lyrics by|Composed by|Produced by|Producer|Arranger)\s*[:：]").unwrap()
    })
}

pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(c) = tag_re().captures(rest) {
            let m: u64 = c[1].parse().unwrap_or(0);
            let s: u64 = c[2].parse().unwrap_or(0);
            let frac = c.get(3).map(|f| {
                let v: u64 = f.as_str().parse().unwrap_or(0);
                match f.as_str().len() {
                    1 => v * 100,
                    2 => v * 10,
                    _ => v,
                }
            });
            times.push(m * 60_000 + s * 1000 + frac.unwrap_or(0));
            rest = &rest[c.get(0).unwrap().end()..];
        }
        let text = rest.trim();
        if times.is_empty() || credit_re().is_match(text) {
            continue;
        }
        for t in times {
            out.push(Line { time_ms: Some(t), text: text.to_string() });
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags() {
        let l = parse_lrc("[ar:Someone]\n[00:01.50]Hello\n[00:03.2][00:10.123]Again\n[00:00.00] 作词 : X\n");
        let v: Vec<_> = l.iter().map(|x| (x.time_ms.unwrap(), x.text.as_str())).collect();
        assert_eq!(v, vec![(1500, "Hello"), (3200, "Again"), (10123, "Again")]);
    }
}
