//! Byte-length-preserving preprocessing of MyST-specific block syntax.
//!
//! CommonMark recognises backtick (` ``` `) fenced code blocks but not MyST's
//! colon (`:::`) fences. Because a colon and a backtick are both a single
//! ASCII byte, we can rewrite a colon fence's delimiter run into a backtick
//! run of identical length. The rewritten source has exactly the same byte
//! length and the same UTF-8 boundaries as the input, so every byte offset
//! reported by `pulldown-cmark::Parser::into_offset_iter()` is already a valid,
//! exact span into the *original* `.md` source — no offset map is needed.
//!
//! Only lines that clearly look like a fence delimiter are rewritten: up to
//! three leading spaces, a run of three or more colons, then either the end of
//! the line, whitespace, or a `{directive}` info string. Everything else is
//! left untouched.

/// Rewrites MyST colon fence delimiters into CommonMark backtick fences.
///
/// The returned string has the same byte length as `source`.
pub fn preprocess_colon_fences(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(index) => (&rest[..=index], &rest[index + 1..]),
            None => (rest, ""),
        };
        out.push_str(&rewrite_line(line));
        rest = next;
    }
    debug_assert_eq!(out.len(), source.len(), "colon-fence rewrite changed byte length");
    out
}

fn rewrite_line(line: &str) -> String {
    // Split off a trailing line ending (`\n` or `\r\n`).
    let without_eol = line.trim_end_matches(['\n', '\r']);
    let eol = &line[without_eol.len()..];
    let leading = without_eol.len() - without_eol.trim_start_matches(' ').len();
    if leading > 3 {
        return line.to_owned();
    }
    let after_indent = &without_eol[leading..];
    let colons = after_indent.bytes().take_while(|&b| b == b':').count();
    if colons < 3 {
        return line.to_owned();
    }
    let tail = &after_indent[colons..];
    let tail_ok = tail.is_empty()
        || tail.starts_with('{')
        || tail.chars().next().is_some_and(char::is_whitespace);
    if !tail_ok {
        return line.to_owned();
    }

    let mut rewritten = String::with_capacity(line.len());
    rewritten.push_str(&without_eol[..leading]);
    rewritten.extend(std::iter::repeat_n('`', colons));
    rewritten.push_str(tail);
    rewritten.push_str(eol);
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_opening_and_closing_fences() {
        let src = ":::{note}\nbody\n:::\n";
        let out = preprocess_colon_fences(src);
        assert_eq!(out, "```{note}\nbody\n```\n");
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn handles_longer_and_indented_fences() {
        let src = "  ::::{admonition} Title\n  ::::{note} body\n   ::::\n";
        let out = preprocess_colon_fences(src);
        assert_eq!(out, "  ````{admonition} Title\n  ````{note} body\n   ````\n");
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn leaves_normal_text_alone() {
        let src = "a : b\n:x: value\nterm\n: definition\nhttp://x\n";
        assert_eq!(preprocess_colon_fences(src), src);
    }
}
