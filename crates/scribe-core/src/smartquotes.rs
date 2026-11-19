//! Docutils-compatible smart-quote / dash education.
//!
//! The behaviour transcribed here comes from the pinned oracle's
//! `docutils/utils/smartquotes.py` (`educate_tokens`, `educateQuotes`,
//! `educateDashesOldSchool`, `educateDashes`, `educateEllipses`) and
//! `sphinx/util/nodes.py::is_smartquotable`. Sphinx's `SphinxSmartQuotes`
//! transform delegates the actual text rewriting to docutils; this module is
//! the Rust equivalent of that delegation.
//!
//! Scope: quote pairs (`"` and `'`), en/em dashes (`--`, `---`) and ellipses
//! (`...`). Backtick quotes are intentionally not rewritten: the default
//! `smartquotes_action` (`qDe`) does not request them, and the frozen AST stores
//! inline literals as dedicated `Literal` nodes that are skipped by the
//! `not_smartquotable` filter in `transforms.rs`.

/// A language's four quote glyphs: primary open/close then secondary open/close.
type QuotePairs = (&'static str, &'static str, &'static str, &'static str);

const EN: QuotePairs = ("\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}");
const DE: QuotePairs = ("\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}");
const FR: QuotePairs = ("\u{ab}\u{a0}", "\u{a0}\u{bb}", "\u{201c}", "\u{201d}");
const ES: QuotePairs = ("\u{ab}", "\u{bb}", "\u{201c}", "\u{201d}");
const RU: QuotePairs = ("\u{ab}", "\u{bb}", "\u{201e}", "\u{201c}");
const JA: QuotePairs = ("\u{300c}", "\u{300d}", "\u{300e}", "\u{300f}");

/// Normalizes a BCP 47 / Python locale tag to the form used by docutils'
/// `normalize_language_tag`, e.g. `en_US` → `en-us` → `en`.
pub fn normalize_language_tag(language: &str) -> String {
    language.trim().replace('_', "-").to_lowercase()
}

/// Returns the quote pairs for `language`, falling back to English.
///
/// The table is the subset of docutils' `smartchars.quotes` needed by the
/// documented language variations; unknown languages degrade to English
/// exactly as `SphinxSmartQuotes::is_available` falls back when no tag matches.
pub fn quote_pairs(language: &str) -> QuotePairs {
    let tag = normalize_language_tag(language);
    match tag.as_str() {
        "de" | "de-at" | "de-x-altquot" => DE,
        "de-ch" => ("\u{ab}", "\u{bb}", "\u{2039}", "\u{203a}"),
        "fr" | "fr-ca" => FR,
        "fr-ch" => ("\u{ab}\u{202f}", "\u{202f}\u{bb}", "\u{2039}\u{202f}", "\u{202f}\u{203a}"),
        "es" | "ca" | "gl" | "pt" | "it" => ES,
        "pt-br" | "en" | "en-us" | "en-gb" | "nl" | "ko" | "eo" => EN,
        "ru" | "uk" | "be" => RU,
        "ja" => JA,
        "zh" | "zh-cn" | "zh-tw" => ("\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"),
        "cs" | "sk" | "pl" | "lt" | "lv" | "et" | "hu" | "is" => {
            ("\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}")
        }
        "da" | "nb" | "nn" | "no" | "sv" => ("\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"),
        "fi" => ("\u{201d}", "\u{201d}", "\u{2019}", "\u{2019}"),
        "ar" | "fa" | "he" => ("\u{201d}", "\u{201c}", "\u{2019}", "\u{2018}"),
        _ => {
            // Try the primary subtag (`en-us` → `en`, `de-ch` handled above).
            let primary = tag.split('-').next().unwrap_or("en");
            match primary {
                "de" => DE,
                "fr" => FR,
                "es" | "ca" | "it" | "pt" => ES,
                "ru" | "uk" | "be" => RU,
                "ja" => JA,
                _ => EN,
            }
        }
    }
}

/// `true` when docutils has a quote table for this language tag.
///
/// Used by `SphinxSmartQuotes::is_available`: a language without a table is a
/// no-op rather than an English fallback at the transform level.
pub fn language_supports_smartquotes(language: &str) -> bool {
    let tag = normalize_language_tag(language);
    const KNOWN: &[&str] = &[
        "af", "bg", "ca", "cs", "da", "de", "el", "en", "eo", "es", "et", "eu", "fi", "fr",
        "gl", "he", "hr", "hsb", "hu", "is", "it", "ja", "ko", "lt", "lv", "mk", "nb", "nl",
        "nn", "no", "pl", "pt", "ro", "ru", "sk", "sl", "sr", "sv", "tr", "uk", "zh", "ar",
        "fa",
    ];
    if KNOWN.contains(&tag.as_str()) {
        return true;
    }
    KNOWN.contains(&tag.split('-').next().unwrap_or(""))
}

/// Educates one plain-text run according to `action` (the
/// `smartquotes_action` config value, default `"qDe"`).
///
/// Action characters mirror docutils: `q` quotes, `d` new-school dashes,
/// `D` old-school dashes, `e` ellipses; `0` disables everything. Backtick
/// actions (`b`/`B`) are accepted and ignored because the frozen AST keeps
/// backtick literals as dedicated nodes that never reach this function.
pub fn educate(text: &str, language: &str, action: &str) -> String {
    if action.is_empty() || action == "0" {
        return text.to_owned();
    }
    let quotes = action.contains('q');
    let dashes_new = action.contains('d');
    let dashes_old = action.contains('D');
    let ellipses = action.contains('e');
    if !quotes && !dashes_new && !dashes_old && !ellipses {
        return text.to_owned();
    }

    let (opq, cpq, osq, csq) = quote_pairs(language);
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len() + 16);
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        // Backslash escapes: `\"` and friends stay ASCII (docutils'
        // processEscapes/restore cycle).
        if chars[i] == '\\'
            && i + 1 < chars.len()
            && matches!(chars[i + 1], '-' | '\\' | '\'' | '"' | '.' | '`')
        {
            result.push(chars[i + 1]);
            prev = Some(chars[i + 1]);
            i += 2;
            continue;
        }
        // Ellipsis (`. . .` and `...`).
        if ellipses
            && chars[i] == '.'
            && i + 4 < chars.len()
            && chars[i + 1] == ' '
            && chars[i + 2] == '.'
            && chars[i + 3] == ' '
            && chars[i + 4] == '.'
        {
            result.push('\u{2026}');
            prev = Some('.');
            i += 5;
            continue;
        }
        if ellipses
            && chars[i] == '.'
            && i + 2 < chars.len()
            && chars[i + 1] == '.'
            && chars[i + 2] == '.'
        {
            result.push('\u{2026}');
            prev = Some('.');
            i += 3;
            continue;
        }
        // Dashes: docutils' `educateDashesOldSchool` maps --- → em, -- → en;
        // `educateDashes` maps --- → en, -- → em. The default action `D` uses
        // old-school.
        if (dashes_old || dashes_new)
            && chars[i] == '-'
            && i + 1 < chars.len()
            && chars[i + 1] == '-'
        {
            let three = i + 2 < chars.len() && chars[i + 2] == '-';
            result.push(if three == dashes_old {
                '\u{2014}' // old-school: --- → em; new-school: -- → em
            } else {
                '\u{2013}'
            });
            prev = Some('-');
            i += if three { 3 } else { 2 };
            continue;
        }
        let ch = chars[i];
        if quotes && ch == '"' {
            let opening = prev.is_none()
                || prev.is_some_and(|p| {
                    p.is_whitespace()
                        || matches!(p, '(' | '[' | '{' | '<' | '-' | '\u{2013}' | '\u{2014}' | '\u{2018}' | '\u{201c}')
                });
            result.push_str(if opening { opq } else { cpq });
            prev = Some('"');
            i += 1;
            continue;
        }
        if quotes && ch == '\'' {
            let next = chars.get(i + 1).copied();
            let between_letters = prev.is_some_and(char::is_alphanumeric)
                && next.is_some_and(char::is_alphanumeric);
            if between_letters || matches!(prev, Some('s') | Some('S')) {
                result.push_str(csq);
            } else {
                let opening = prev.is_none()
                    || prev.is_some_and(|p| {
                        p.is_whitespace()
                            || matches!(p, '(' | '[' | '{' | '<' | '-' | '\u{2013}' | '\u{2014}' | '"')
                    });
                result.push_str(if opening { osq } else { csq });
            }
            prev = Some('\'');
            i += 1;
            continue;
        }
        result.push(ch);
        prev = Some(ch);
        i += 1;
    }
    result
}

/// Convenience for tests and callers that only need the default action.
pub fn educate_default(text: &str, language: &str) -> String {
    educate(text, language, "qDe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_quotes_and_dashes() {
        assert_eq!(educate_default("\"Hello\"", "en"), "\u{201c}Hello\u{201d}");
        assert_eq!(educate_default("it's", "en"), "it\u{2019}s");
        assert_eq!(educate_default("a -- b --- c", "en"), "a \u{2013} b \u{2014} c");
        assert_eq!(educate_default("wait...", "en"), "wait\u{2026}");
    }

    #[test]
    fn language_variations() {
        assert_eq!(educate_default("\"Hi\"", "de"), "\u{201e}Hi\u{201c}");
        assert_eq!(educate_default("\"Hi\"", "ja"), "\u{300c}Hi\u{300d}");
        assert_eq!(quote_pairs("en_US"), EN);
    }

    #[test]
    fn escapes_are_preserved() {
        assert_eq!(educate_default("6\\'2\\\" tall", "en"), "6'2\" tall");
    }

    #[test]
    fn disabled_action_is_noop() {
        assert_eq!(educate("\"Hi\" -- x", "en", "0"), "\"Hi\" -- x");
    }
}
