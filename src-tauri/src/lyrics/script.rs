//! Which writing system a song's lyrics are in, so the original script wins over a
//! romanization (romaji, Korean romanization, pinyin) or a translation.
//!
//! The player's names only help when they are in a CJK script. Spotify and YouTube
//! often give Japanese, Korean and Chinese songs Latin names ("Yoru ni Kakeru",
//! "Sunny Day"), so Latin lyrics are also checked for whether they *read* like a
//! romanization: nearly every word is built from CJK-style syllables
//! ("shi-zu-mu", "sa-rang-hae", "zhong-guo"), which ordinary English isn't
//! ("the", "love", "night", "never" are not), and some words carry spellings
//! typical of romaji / Korean / pinyin ("tsu", "kyo", "eo", "zh", "xi"), which
//! syllable-built languages such as Indonesian or Hawaiian rarely do.

use super::{Line, Query};
use regex::Regex;
use std::sync::OnceLock;

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

fn is_kana_hangul(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F)
}

/// Japanese kana or Korean hangul anywhere in `s`.
pub fn has_kana_hangul(s: &str) -> bool {
    s.chars().any(is_kana_hangul)
}

/// Any CJK script: Han, kana or hangul.
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| is_han(c) || is_kana_hangul(c))
}

/// Which writing system one line is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Class {
    /// No letters at all (blank, "♪", "...").
    Blank,
    Latin,
    /// Han characters only: Chinese, or Japanese written without kana.
    Han,
    KanaHangul,
}

pub fn class(t: &str) -> Class {
    if t.chars().any(is_kana_hangul) {
        Class::KanaHangul
    } else if t.chars().any(is_han) {
        Class::Han
    } else if t.chars().any(|c| c.is_alphabetic()) {
        Class::Latin
    } else {
        Class::Blank
    }
}

/// Same-timestamp groups of lines, in file order (each unsynced line on its own).
pub fn groups(lines: &[Line]) -> Vec<&[Line]> {
    let mut out: Vec<&[Line]> = Vec::new();
    let mut start = 0;
    for i in 1..=lines.len() {
        if i == lines.len() || lines[i].time_ms.is_none() || lines[i].time_ms != lines[start].time_ms {
            out.push(&lines[start..i]);
            start = i;
        }
    }
    out
}

/// Classes of a group's lines, blank ones left out.
pub fn classes(g: &[Line]) -> Vec<Class> {
    g.iter().map(|x| class(&x.text)).filter(|c| *c != Class::Blank).collect()
}

/// A group whose lines are in more than one script: a line with its translation
/// or romanization.
pub fn is_mixed(g: &[Line]) -> bool {
    classes(g).windows(2).any(|w| w[0] != w[1])
}

/// A file that gives a quarter or more of its timestamps a line in another script
/// alongside: a translation or romanization throughout, not the odd ad-lib.
pub fn is_paired(lines: &[Line]) -> bool {
    let gs: Vec<&[Line]> = groups(lines).into_iter().filter(|g| !classes(g).is_empty()).collect();
    let mixed = gs.iter().filter(|g| is_mixed(g)).count();
    !gs.is_empty() && mixed * 4 >= gs.len()
}

/// The player's title or artist is written in a CJK script.
pub fn cjk_names(q: &Query) -> bool {
    has_cjk(&q.title) || has_cjk(&q.artist)
}

/// One word made only of CJK-style syllables: an optional onset (with y / w
/// glide), one to three vowels, an optional final consonant.
fn syllables() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"^(?:(?:ssh|tch|cch|ch|sh|ts|zh|kk|tt|pp|ss|jj|[bcdfghjklmnpqrstwxyz])?[yw]?[aeiouüāáǎàēéěèīíǐìōóǒòūúǔùǖǘǚǜ]{1,3}(?:ng|n|m|r|l|k|t|p)?)+$",
        )
        .unwrap()
    })
}

/// Spellings typical of romaji, Korean romanization or pinyin.
fn markers() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"shi|chi|tsu|ky|ry|gy|hy|ou|uu|eo|eu|ae|zh|x[iu]|q[iu]|[üōū]|kk|tt|pp|ss|jj").unwrap())
}

/// Latin-letter text that reads as a romanization of Japanese, Korean or Chinese
/// (so lyrics that are a romanization rather than the original).
pub fn looks_romanized(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_lowercase())
        .collect();
    if words.len() < 5 || has_cjk(text) {
        return false;
    }
    let fit = words.iter().filter(|w| syllables().is_match(w)).count();
    let marked = words.iter().filter(|w| markers().is_match(w)).count();
    fit * 100 >= words.len() * 85 && marked * 100 >= words.len() * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made-up lines in the style of each language (not real lyrics).
    const ROMAJI: &str = "Asa no hikari ni te wo nobashite\nKimi to aruita kaerimichi\nZutto issho ni ite kureru kana";
    const KOREAN: &str = "Haneul arae neowa hamkke georeosseo\nGeu nare uri saranghaesseo\nNunmul nado gwaenchana";
    const PINYIN: &str = "Wo zai zhe li deng ni hui lai\nChuang wai de yu xia bu ting\nXiang nian ni de wan shang";
    const ENGLISH: &str = "Walking slowly through the quiet morning\nCoffee spilled across the kitchen table\nI never thought the night would end like this";
    // Languages built from simple syllables, which mustn't pass for romanizations.
    const INDONESIAN: &str = "Aku selalu menunggu kamu di sini\nSetiap malam bintang menemani\nJangan pergi lagi sayang";
    const HAWAIIAN_STYLE: &str = "Aloha nui loa i ka la nani\nE hele mai ke kai malie\nKa makani pa i ka lani";
    const ITALIAN: &str = "Ti amo sempre come la prima volta\nCammino sola sotto la pioggia\nNon dimenticare mai questa notte";

    #[test]
    fn spots_romanizations() {
        assert!(looks_romanized(ROMAJI));
        assert!(looks_romanized(KOREAN));
        assert!(looks_romanized(PINYIN));
        assert!(!looks_romanized(ENGLISH));
        assert!(!looks_romanized(INDONESIAN));
        assert!(!looks_romanized(HAWAIIAN_STYLE));
        assert!(!looks_romanized(ITALIAN));
        assert!(!looks_romanized("Na na na"), "too few words to tell");
    }

    #[test]
    fn classes() {
        assert_eq!(class("朝の光"), Class::KanaHangul);
        assert_eq!(class("하늘"), Class::KanaHangul);
        assert_eq!(class("等你"), Class::Han);
        assert_eq!(class("Morning"), Class::Latin);
        assert_eq!(class("♪ ..."), Class::Blank);
    }

    #[test]
    fn romanized_needs_latin_only_text() {
        assert!(!looks_romanized("朝の光に\nAsa no hikari ni te wo nobashite kimi to aruita"));
        assert!(!looks_romanized(""));
    }
}
