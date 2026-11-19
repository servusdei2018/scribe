//! LaTeX text escaping and Unicode transliteration.
//!
//! This module mirrors upstream's `sphinx/util/texescape.py` closely enough
//! that the body text emitted by [`super::LaTeXTranslator`] is compilable by
//! `pdflatex`, `xelatex` and `lualatex`. Two escape modes are provided:
//!
//! * [`escape_latex`] — the full `pdflatex` mode: ASCII special characters are
//!   escaped and non-ASCII code points that have no standard LaTeX command are
//!   transliterated to ASCII.
//! * [`escape_latex_unicode`] — the Unicode-engine mode (`xelatex` /
//!   `lualatex`): only the structural ASCII special characters are escaped;
//!   Unicode code points pass through unchanged (the engine's font handles
//!   them).
//!
//! Escaping is *idempotent-safe* in the sense that the translator only ever
//! calls it on raw text values read from the doctree, never on already escaped
//! markup.

/// The LaTeX engine whose escaping rules should be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LatexEngine {
    /// `pdflatex` — ASCII-only output with Unicode transliteration.
    #[default]
    PdfLatex,
    /// `xelatex` — Unicode-aware output.
    XeLatex,
    /// `lualatex` — Unicode-aware output.
    LuaLatex,
}

impl LatexEngine {
    /// Parse an engine name, defaulting to `pdflatex` for unknown values.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "xelatex" => Self::XeLatex,
            "lualatex" => Self::LuaLatex,
            _ => Self::PdfLatex,
        }
    }

    /// `true` when the engine consumes UTF-8 input directly.
    #[must_use]
    pub const fn is_unicode(self) -> bool {
        matches!(self, Self::XeLatex | Self::LuaLatex)
    }

    /// Canonical engine name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PdfLatex => "pdflatex",
            Self::XeLatex => "xelatex",
            Self::LuaLatex => "lualatex",
        }
    }
}

/// Escape a string of plain text for inclusion in a LaTeX document.
///
/// The `pdflatex` mode follows Sphinx's `texescape.escape`: TeX's ten special
/// characters are escaped, a handful of common Unicode symbols are mapped to
/// their LaTeX equivalents, and remaining non-ASCII code points are dropped or
/// approximated by a printable ASCII character. This never panics and produces
/// ASCII-only output for `pdflatex`.
#[must_use]
pub fn escape_latex(text: &str, engine: LatexEngine) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        // Structural TeX specials (escaped for every engine).
        match ch {
            '$' => { out.push_str("\\$"); continue; }
            '%' => { out.push_str("\\%"); continue; }
            '&' => { out.push_str("\\&"); continue; }
            '#' => { out.push_str("\\#"); continue; }
            '_' => { out.push_str("\\_"); continue; }
            '{' => { out.push_str("\\{"); continue; }
            '}' => { out.push_str("\\}"); continue; }
            '\\' => { out.push_str("\\textbackslash{}"); continue; }
            '~' => { out.push_str("\\textasciitilde{}"); continue; }
            '^' => { out.push_str("\\textasciicircum{}"); continue; }
            '[' => { out.push_str("{["); continue; }
            ']' => { out.push_str("]}"); continue; }
            _ => {}
        }
        if let Some(replacement) = symbol_replacement(ch) {
            out.push_str(replacement);
            continue;
        }
        if engine.is_unicode() || ch.is_ascii() {
            out.push(ch);
        } else {
            out.push_str(&ascii_approximation(ch));
        }
    }
    out
}

/// Escape text using the default (`pdflatex`) engine.
#[must_use]
pub fn escape_latex_default(text: &str) -> String {
    escape_latex(text, LatexEngine::PdfLatex)
}

/// Escape text for Unicode-capable engines (`xelatex`/`lualatex`).
#[must_use]
pub fn escape_latex_unicode(text: &str) -> String {
    escape_latex(text, LatexEngine::XeLatex)
}

/// Map a Unicode symbol to a LaTeX command shared by all engines.
///
/// The mapping is intentionally a small, high-value subset of upstream's
/// `unicode_tex_replacements` (punctuation, arrows, super/subscript digits).
/// Returning `None` falls through to the engine-specific handling.
fn symbol_replacement(ch: char) -> Option<&'static str> {
    Some(match ch {
        '✓' => "\\(\\checkmark\\)",
        '✔' => "\\(\\pmb{\\checkmark}\\)",
        '✕' => "\\(\\times\\)",
        '✖' => "\\(\\pmb{\\times}\\)",
        '\u{feff}' => "{}",
        '⎽' => "\\_",
        'ℯ' => "e",
        'ⅈ' => "i",
        '¶' => "\\P{}",
        '§' => "\\S{}",
        '€' => "\\texteuro{}",
        '∞' => "\\(\\infty\\)",
        '±' => "\\(\\pm\\)",
        '→' | '‣' => "\\(\\rightarrow\\)",
        '–' => "\\textendash{}",
        '⁰' => "\\(\\sp{\\text{0}}\\)",
        '¹' => "\\(\\sp{\\text{1}}\\)",
        '²' => "\\(\\sp{\\text{2}}\\)",
        '³' => "\\(\\sp{\\text{3}}\\)",
        '⁴' => "\\(\\sp{\\text{4}}\\)",
        '⁵' => "\\(\\sp{\\text{5}}\\)",
        '⁶' => "\\(\\sp{\\text{6}}\\)",
        '⁷' => "\\(\\sp{\\text{7}}\\)",
        '⁸' => "\\(\\sp{\\text{8}}\\)",
        '⁹' => "\\(\\sp{\\text{9}}\\)",
        '₀' => "\\(\\sb{\\text{0}}\\)",
        '₁' => "\\(\\sb{\\text{1}}\\)",
        '₂' => "\\(\\sb{\\text{2}}\\)",
        '₃' => "\\(\\sb{\\text{3}}\\)",
        '₄' => "\\(\\sb{\\text{4}}\\)",
        '₅' => "\\(\\sb{\\text{5}}\\)",
        '₆' => "\\(\\sb{\\text{6}}\\)",
        '₇' => "\\(\\sb{\\text{7}}\\)",
        '₈' => "\\(\\sb{\\text{8}}\\)",
        '₉' => "\\(\\sb{\\text{9}}\\)",
        _ => return None,
    })
}

/// Approximate a non-ASCII code point with printable ASCII for `pdflatex`.
///
/// Latin-1 letters are mapped to their TeX accent commands (so accented
/// European text stays readable); Greek letters use math mode; everything
/// else is dropped. This guarantees that `escape_latex` returns ASCII.
fn ascii_approximation(ch: char) -> String {
    // Latin-1 accented letters → TeX accents.
    let accented = match ch {
        'à' | 'À' => Some(("\\`", 'a')), 'á' | 'Á' => Some(("\\'", 'a')),
        'â' | 'Â' => Some(("\\^", 'a')), 'ã' | 'Ã' => Some(("\\~", 'a')),
        'ä' | 'Ä' => Some(("\\\"", 'a')), 'å' | 'Å' => Some(("\\r", 'a')),
        'æ' | 'Æ' => Some(("AE", '\0')),
        'ç' | 'Ç' => Some(("\\c", 'c')),
        'è' | 'È' => Some(("\\`", 'e')), 'é' | 'É' => Some(("\\'", 'e')),
        'ê' | 'Ê' => Some(("\\^", 'e')), 'ë' | 'Ë' => Some(("\\\"", 'e')),
        'ì' | 'Ì' => Some(("\\`", 'i')), 'í' | 'Í' => Some(("\\'", 'i')),
        'î' | 'Î' => Some(("\\^", 'i')), 'ï' | 'Ï' => Some(("\\\"", 'i')),
        'ñ' | 'Ñ' => Some(("\\~", 'n')),
        'ò' | 'Ò' => Some(("\\`", 'o')), 'ó' | 'Ó' => Some(("\\'", 'o')),
        'ô' | 'Ô' => Some(("\\^", 'o')), 'õ' | 'Õ' => Some(("\\~", 'o')),
        'ö' | 'Ö' => Some(("\\\"", 'o')), 'ø' | 'Ø' => Some(("\\o", '\0')),
        'ù' | 'Ù' => Some(("\\`", 'u')), 'ú' | 'Ú' => Some(("\\'", 'u')),
        'û' | 'Û' => Some(("\\^", 'u')), 'ü' | 'Ü' => Some(("\\\"", 'u')),
        'ý' | 'Ý' => Some(("\\'", 'y')), 'ÿ' => Some(("\\\"", 'y')),
        'ß' => Some(("ss", '\0')),
        _ => None,
    };
    if let Some((accent, base)) = accented {
        let upper = ch.is_uppercase();
        let mut s = String::from(accent);
        if base != '\0' {
            if upper {
                s.push('{');
                s.push(base.to_ascii_uppercase());
                s.push('}');
            } else {
                s.push(base);
            }
        } else if upper {
            // `AE` and `ss` have no separate uppercase form here.
        }
        return s;
    }
    // Greek letters → math-mode commands.
    let greek = match ch {
        'α' => "alpha", 'β' => "beta", 'γ' => "gamma", 'δ' => "delta",
        'ε' => "epsilon", 'ζ' => "zeta", 'η' => "eta", 'θ' => "theta",
        'ι' => "iota", 'κ' => "kappa", 'λ' => "lambda", 'μ' => "mu",
        'ν' => "nu", 'ξ' => "xi", 'π' => "pi", 'ρ' => "rho", 'σ' => "sigma",
        'τ' => "tau", 'υ' => "upsilon", 'φ' => "phi", 'χ' => "chi",
        'ψ' => "psi", 'ω' => "omega",
        'Α' => "Alpha", 'Β' => "Beta", 'Γ' => "Gamma", 'Δ' => "Delta",
        'Θ' => "Theta", 'Λ' => "Lambda", 'Ξ' => "Xi", 'Π' => "Pi",
        'Σ' => "Sigma", 'Φ' => "Phi", 'Ψ' => "Psi", 'Ω' => "Omega",
        _ => "",
    };
    if !greek.is_empty() {
        return format!("\\({greek}\\)");
    }
    // Common CJK punctuation has printable ASCII approximations.
    match ch {
        '。' => ".".to_owned(), '，' => ",".to_owned(), '：' => ":".to_owned(),
        '；' => ";".to_owned(), '！' => "!".to_owned(), '？' => "?".to_owned(),
        '（' => "(".to_owned(), '）' => ")".to_owned(),
        _ => String::new(),
    }
}

/// Escape a LaTeX label identifier.
///
/// Sphinx applies `tex_replace_map` then encodes to ASCII with
/// `backslashreplace`; the backslashes are replaced by underscores. We follow
/// the same approach with a deterministic ASCII mapping so labels are stable
/// and never contain a raw `%`, `{`, `}` or `#`.
#[must_use]
pub fn escape_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for ch in id.chars() {
        match ch {
            '%' | '{' | '}' | '\\' | '#' | '~' | '&' | '$' | '^' | '_' => {
                out.push('_');
            }
            c if c.is_ascii() => out.push(c),
            c => {
                // Deterministic ASCII fallback for non-ASCII code points.
                out.push_str(&format!("u{:04x}_", c as u32));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_characters_are_escaped() {
        let s = escape_latex_default("100% $x_y#1{a}~b^c\\d");
        assert!(s.contains("\\%"), "{s}");
        assert!(s.contains("\\$"), "{s}");
        assert!(s.contains("\\_"), "{s}");
        assert!(s.contains("\\#"), "{s}");
        assert!(s.contains("\\{"), "{s}");
        assert!(s.contains("\\textasciitilde{}"), "{s}");
        assert!(s.contains("\\textasciicircum{}"), "{s}");
        assert!(s.contains("\\textbackslash{}"), "{s}");
    }

    #[test]
    fn unicode_symbols_map_to_latex() {
        assert_eq!(escape_latex_default("ℯ"), "e");
        assert_eq!(escape_latex_default("ⅈ"), "i");
        assert_eq!(escape_latex_default("⁰"), "\\(\\sp{\\text{0}}\\)");
        assert_eq!(escape_latex_default("₀"), "\\(\\sb{\\text{0}}\\)");
        assert!(escape_latex_default("✅").is_empty());
    }

    #[test]
    fn accents_and_greek_map_to_ascii() {
        assert_eq!(escape_latex_default("é"), "\\'e");
        assert_eq!(escape_latex_default("É"), "\\'{E}");
        assert_eq!(escape_latex_default("ß"), "ss");
        assert_eq!(escape_latex_default("α"), "\\(alpha\\)");
    }

    #[test]
    fn unicode_engine_keeps_unicode() {
        assert_eq!(escape_latex_unicode("café"), "café");
        // Structural specials are still escaped.
        assert_eq!(escape_latex_unicode("a_b"), "a\\_b");
    }

    #[test]
    fn id_escape_is_deterministic_and_ascii() {
        assert_eq!(escape_id("a:b-c"), "a:b-c");
        assert_eq!(escape_id("a%b{c}"), "a_b_c_");
        assert_eq!(escape_id("é"), "u00e9_");
    }
}
