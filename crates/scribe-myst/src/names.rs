//! Docutils-compatible name normalization and identifier (slug) generation.
//!
//! This is a faithful copy of the algorithm in `scribe-rst`'s private `names`
//! module (itself transcribed from `docutils.nodes.make_id` and
//! `fully_normalize_name`, Docutils 0.23). It is duplicated here — rather than
//! made public in `scribe-rst` — to leave that crate's frozen public surface
//! untouched. Both parsers therefore produce byte-identical `ids`/`names` for
//! equivalent headings and targets.

use std::collections::HashMap;

/// Docutils `fully_normalize_name`: lowercase and collapse whitespace runs.
pub fn normalize_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Docutils `whitespace_normalize_name`: collapse whitespace, keep case.
pub fn whitespace_normalize_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Docutils `make_id`: convert a title/reference name into a CSS-safe id.
pub fn make_id(input: &str) -> String {
    let lowered = input.to_lowercase();
    let mut translated = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if let Some(replacement) = digraph(ch) {
            translated.push_str(replacement);
        } else if let Some(replacement) = single(ch) {
            translated.push_str(replacement);
        } else {
            translated.push(ch);
        }
    }

    let mut ascii = String::with_capacity(translated.len());
    for ch in translated.chars() {
        if ch.is_ascii() {
            ascii.push(ch);
        } else if let Some(replacement) = decompose_latin(ch) {
            ascii.push_str(replacement);
        }
    }

    let words = ascii.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut id = String::with_capacity(words.len());
    let mut in_bad_run = false;
    for ch in words.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if in_bad_run && !id.is_empty() {
                id.push('-');
            }
            id.push(ch);
            in_bad_run = false;
        } else {
            in_bad_run = true;
        }
    }

    let id = id.trim_start_matches(|c: char| c == '-' || c.is_ascii_digit());
    let id = id.trim_end_matches('-');
    id.to_owned()
}

/// Allocates a unique implicit id (first keeps the base, later ones `-1`, `-2`).
pub fn unique_id(base: &str, used: &mut HashMap<String, usize>) -> String {
    let count = used.entry(base.to_owned()).or_insert(0);
    let candidate = if *count == 0 {
        base.to_owned()
    } else {
        format!("{base}-{count}")
    };
    *count += 1;
    candidate
}

/// docutils `_non_id_translate_digraphs`.
fn digraph(ch: char) -> Option<&'static str> {
    match ch as u32 {
        0x00df => Some("sz"), // ß
        0x00e6 => Some("ae"), // æ
        0x0153 => Some("oe"), // œ
        0x0238 => Some("db"),
        0x0239 => Some("qp"),
        _ => None,
    }
}

/// docutils `_non_id_translate` (stroked letters and similar).
fn single(ch: char) -> Option<&'static str> {
    Some(match ch as u32 {
        0x00f8 => "o",
        0x0111 => "d",
        0x0127 => "h",
        0x0131 => "i",
        0x0142 => "l",
        0x0167 => "t",
        0x0180 => "b",
        0x0183 => "b",
        0x0188 => "c",
        0x018c => "d",
        0x0192 => "f",
        0x0199 => "k",
        0x019a => "l",
        0x019e => "n",
        0x01a5 => "p",
        0x01ab => "t",
        0x01ad => "t",
        0x01b4 => "y",
        0x01b6 => "z",
        0x01e5 => "g",
        0x0225 => "z",
        0x0234 => "l",
        0x0235 => "n",
        0x0236 => "t",
        0x0237 => "j",
        0x023c => "c",
        0x023f => "s",
        0x0240 => "z",
        0x0247 => "e",
        0x0249 => "j",
        0x024b => "q",
        0x024d => "r",
        0x024f => "y",
        _ => return None,
    })
}

/// Compact NFKD-style decomposition for the common Latin ranges.
fn decompose_latin(ch: char) -> Option<&'static str> {
    Some(match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' | 'ĸ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "a",
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => "c",
        'Ď' | 'Đ' => "d",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => "e",
        'Ğ' | 'Ġ' | 'Ģ' => "g",
        'Ĥ' | 'Ħ' => "h",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => "i",
        'Ĵ' => "j",
        'Ķ' => "k",
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' => "l",
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => "n",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' => "o",
        'Ŕ' | 'Ŗ' | 'Ř' => "r",
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => "s",
        'Ţ' | 'Ť' | 'Ŧ' => "t",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => "u",
        'Ŵ' => "w",
        'Ý' | 'Ŷ' => "y",
        'Ź' | 'Ż' | 'Ž' => "z",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_docutils_examples() {
        assert_eq!(make_id("A Section"), "a-section");
        assert_eq!(make_id("main title"), "main-title");
        assert_eq!(make_id("Hello, World!"), "hello-world");
        assert_eq!(make_id("123 start"), "start");
        assert_eq!(make_id("a_b.c"), "a-b-c");
        assert_eq!(make_id("Café"), "cafe");
        assert_eq!(make_id("straße"), "strasze");
        assert_eq!(make_id("日本語"), "");
        assert_eq!(normalize_name("A  Section"), "a section");
        assert_eq!(whitespace_normalize_name("A  Section"), "A Section");
    }
}
