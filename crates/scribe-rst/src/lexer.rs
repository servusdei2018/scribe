//! Span-tracking, zero-copy line scanner.
//!
//! This is the Phase 04 lexical layer. It mirrors Docutils'
//! `statemachine.string2lines()` (see `docutils/statemachine.py`):
//!
//! * line breaks are `\n`, `\r\n` or `\r` (Python `str.splitlines()`);
//! * form feeds and vertical tabs are converted to spaces *before* splitting
//!   (Sphinx passes `convert_whitespace=True` in `sphinx/parsers.py`);
//! * tabs are expanded to the next multiple of `tab_width` (default 8);
//! * trailing whitespace is stripped from every logical line.
//!
//! Each [`SourceLine`] keeps byte offsets into the **original** source string,
//! so downstream nodes and miette diagnostics always point at real source
//! bytes even when tab expansion produced an owned, normalized line. The
//! normalized text is a [`Cow`](std::borrow::Cow): it borrows the source when no
//! normalization was needed and allocates only for lines containing tabs or
//! trailing whitespace.

use std::borrow::Cow;

use scribe_ast::Span;

/// A single logical source line with source-relative byte offsets.
#[derive(Clone, Debug)]
pub struct SourceLine<'a> {
    /// 1-based line number.
    pub number: usize,
    /// Original line bytes without the end-of-line sequence.
    pub raw: &'a str,
    /// Normalized line: tabs expanded, trailing whitespace removed.
    pub text: Cow<'a, str>,
    /// Byte offset of `raw` start in the original source.
    pub start: u32,
    /// Byte offset just past `raw` (before the EOL) in the original source.
    pub end: u32,
    /// Byte offset just past the EOL in the original source.
    pub next: u32,
    /// End-of-line sequence (`"\r\n"`, `"\n"`, `"\r"` or `""`).
    pub eol: &'a str,
    /// Number of leading whitespace columns after tab expansion.
    pub indent_cols: usize,
    /// Byte index into `text` where non-whitespace content begins.
    pub indent_bytes: usize,
    /// `true` when the normalized line contains only whitespace.
    pub blank: bool,
}

impl<'a> SourceLine<'a> {
    /// The full normalized line (including leading indentation).
    pub fn text(&self) -> &str {
        self.text.as_ref()
    }

    /// The normalized line with leading indentation removed.
    pub fn content(&self) -> &str {
        &self.text[self.indent_bytes..]
    }

    /// Span covering the whole original line (excluding EOL).
    pub fn span(&self) -> Span {
        Span::new(self.start, self.end)
    }

    /// Span covering only the non-indented content of the line.
    pub fn content_span(&self) -> Span {
        let start = self.start.saturating_add(self.indent_bytes as u32);
        Span::new(start.min(self.end), self.end)
    }

    /// Span covering the leading indentation of the line.
    pub fn indent_span(&self) -> Span {
        Span::new(self.start, self.start.saturating_add(self.indent_bytes as u32).min(self.end))
    }

    /// `true` when this line is a uniform ASCII-punctuation adornment line.
    ///
    /// Mirrors Docutils' `Body.patterns['line']`:
    /// `([!-/:-@[-`{-~])\1* *$` — one repeated 7-bit non-alphanumeric
    /// character, optional trailing spaces. Returns the character and the
    /// number of adornment characters (trailing spaces excluded).
    pub fn adornment(&self) -> Option<(char, usize)> {
        adornment_of(self.text.as_ref())
    }
}

/// Detects a uniform ASCII-punctuation adornment line.
///
/// Returns `(character, visible_width)` or `None`. The character set is
/// exactly Docutils' `nonalphanum7bit` pattern: `!-/`, `:-@`, `[-`` and `{-~`.
pub fn adornment_of(text: &str) -> Option<(char, usize)> {
    let trimmed = text.trim_end_matches(' ');
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !is_adornment_char(first) {
        return None;
    }
    let mut width = 1usize;
    for ch in chars {
        if ch != first {
            return None;
        }
        width += 1;
    }
    Some((first, width))
}

/// Docutils `nonalphanum7bit`: `[!-/:-@[-`{-~]`.
pub fn is_adornment_char(ch: char) -> bool {
    matches!(ch as u32, 0x21..=0x2f | 0x3a..=0x40 | 0x5b..=0x60 | 0x7b..=0x7e)
}

/// The lexical scanner.
#[derive(Clone, Debug)]
pub struct Lexer<'a> {
    source: &'a str,
    tab_width: usize,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer with Docutils' default tab width of 8.
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            tab_width: 8,
        }
    }

    /// Creates a lexer with an explicit tab width.
    pub fn with_tab_width(source: &'a str, tab_width: usize) -> Self {
        Self {
            source,
            tab_width: tab_width.max(1),
        }
    }

    /// The source being scanned.
    pub fn source(&self) -> &'a str {
        self.source
    }

    /// Configured tab width.
    pub fn tab_width(&self) -> usize {
        self.tab_width
    }

    /// Scans the whole source into logical lines.
    pub fn scan(&self) -> Vec<SourceLine<'a>> {
        let mut lines = Vec::new();
        let bytes = self.source.as_bytes();
        let mut pos = 0usize;
        let mut number = 1usize;

        while pos < bytes.len() {
            let line_start = pos;
            // Consume bytes until a recognized line break. `\v` and `\f` are
            // treated as spaces (Sphinx `convert_whitespace=True`).
            while pos < bytes.len() {
                match bytes[pos] {
                    b'\r' | b'\n' => break,
                    _ => pos += 1,
                }
            }
            let raw_end = pos;
            let eol = if pos < bytes.len() {
                if bytes[pos] == b'\r' {
                    pos += 1;
                    if pos < bytes.len() && bytes[pos] == b'\n' {
                        pos += 1;
                        "\r\n"
                    } else {
                        "\r"
                    }
                } else {
                    pos += 1;
                    "\n"
                }
            } else {
                ""
            };

            // `raw` must be a valid &str boundary: we stop only at ASCII CR/LF,
            // which cannot appear inside a multi-byte UTF-8 code point.
            let raw = &self.source[line_start..raw_end];
            let text = self.normalize(raw);
            let indent_bytes = leading_space_bytes(text.as_ref());
            let indent_cols = text.as_ref()[..indent_bytes].chars().count();
            let blank = text.as_ref()[indent_bytes..].is_empty();

            lines.push(SourceLine {
                number,
                raw,
                text,
                start: line_start as u32,
                end: raw_end as u32,
                next: pos as u32,
                eol,
                indent_cols,
                indent_bytes,
                blank,
            });

            number += 1;
        }

        // Docutils `splitlines()` on an empty string yields no lines; on a
        // string consisting only of newlines it yields one empty line per
        // break, which the loop above already produces.
        lines
    }

    /// Expands tabs and strips trailing whitespace, borrowing when unchanged.
    fn normalize(&self, raw: &'a str) -> Cow<'a, str> {
        let changed = raw
            .bytes()
            .any(|b| b == b'\t' || b == b'\x0b' || b == b'\x0c')
            || raw
                .chars()
                .last()
                .is_some_and(char::is_whitespace);
        if !changed {
            return Cow::Borrowed(raw);
        }

        let width = self.tab_width;
        let mut out = String::with_capacity(raw.len());
        let mut column = 0usize;
        for ch in raw.chars() {
            match ch {
                '\t' => {
                    let spaces = width - (column % width);
                    for _ in 0..spaces {
                        out.push(' ');
                    }
                    column += spaces;
                }
                '\x0b' | '\x0c' => {
                    out.push(' ');
                    column += 1;
                }
                _ => {
                    out.push(ch);
                    column += 1;
                }
            }
        }
        let trimmed = out.trim_end();
        if trimmed.len() != out.len() {
            out.truncate(trimmed.len());
        }
        Cow::Owned(out)
    }
}

/// Byte index where leading spaces end in `text` (tabs already expanded).
fn leading_space_bytes(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .take_while(|&&b| b == b' ')
        .count()
}

/// Computes the display column width of a string, approximating Docutils'
/// `column_width`: East Asian wide/full-width characters count as two and
/// combining marks as zero. ASCII text (the common case) is exact.
pub fn column_width(text: &str) -> usize {
    text.chars()
        .map(|ch| {
            let cp = ch as u32;
            if is_combining(cp) {
                0
            } else if is_wide(cp) {
                2
            } else {
                1
            }
        })
        .sum()
}

fn is_combining(cp: u32) -> bool {
    matches!(cp,
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff |
        0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}

fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 |
        0xf900..=0xfaff | 0xfe10..=0xfe19 | 0xfe30..=0xfe6f |
        0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff |
        0x20000..=0x3fffd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_trailing_newline() {
        let lexer = Lexer::new("a\r\nbb\nccc");
        let lines = lexer.scan();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "a");
        assert_eq!(lines[0].eol, "\r\n");
        assert_eq!(lines[0].number, 1);
        assert_eq!(lines[1].raw, "bb");
        assert_eq!(lines[2].text(), "ccc");
        assert_eq!(lines[2].eol, "");
    }

    #[test]
    fn trailing_newline_does_not_create_phantom_line() {
        assert_eq!(Lexer::new("a\n").scan().len(), 1);
        assert!(Lexer::new("").scan().is_empty());
    }

    #[test]
    fn tabs_expand_to_tab_stops_and_rstrip() {
        let lines = Lexer::new("a\tb   \n\tc\t\n").scan();
        assert_eq!(lines[0].text(), "a       b"); // 'a' at col 0 -> 7 spaces
        assert_eq!(lines[0].indent_cols, 0);
        assert_eq!(lines[1].text(), "        c"); // tab -> 8 spaces, trailing tab stripped
        assert!(!lines[1].blank);
        let wide = Lexer::with_tab_width("\tx", 4).scan();
        assert_eq!(wide[0].text(), "    x");
        assert_eq!(wide[0].indent_cols, 4);
    }

    #[test]
    fn spans_point_into_original_source() {
        let source = "  hello  \nworld";
        let lines = Lexer::new(source).scan();
        assert_eq!(lines[0].text(), "  hello");
        assert_eq!(lines[0].content(), "hello");
        assert_eq!(lines[0].indent_cols, 2);
        assert_eq!(lines[0].span(), Span::new(0, 9));
        assert_eq!(lines[0].content_span(), Span::new(2, 9));
        assert_eq!(&source[lines[1].start as usize..lines[1].end as usize], "world");
    }

    #[test]
    fn adornment_detection() {
        assert_eq!(adornment_of("-----"), Some(('-', 5)));
        assert_eq!(adornment_of("====   "), Some(('=', 4)));
        assert_eq!(adornment_of("***"), Some(('*', 3)));
        assert_eq!(adornment_of("-=-"), None);
        assert_eq!(adornment_of("abc"), None);
        assert_eq!(adornment_of("::"), Some((':', 2)));
        assert_eq!(adornment_of("————"), None); // em-dash is not 7-bit punctuation
    }

    #[test]
    fn unicode_column_width() {
        assert_eq!(column_width("abc"), 3);
        assert_eq!(column_width("日本"), 4);
        assert_eq!(column_width("e\u{0301}"), 1); // e + combining acute
    }
}
