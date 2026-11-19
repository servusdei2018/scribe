//! Rust-side doctest flag parsing and output normalization helpers (Phase 22).
//!
//! Real execution is delegated to Python's `doctest` engine (see `runner.rs`),
//! which guarantees exact `ELLIPSIS` / `NORMALIZE_WHITESPACE` /
//! `IGNORE_EXCEPTION_DETAIL` semantics. These helpers cover the type-safe
//! option parsing on the Rust side, provide a small pure implementation used
//! by unit tests and by the graceful "no interpreter" path, and expose the
//! canonical diff shape asserted by the differential tests.

use std::collections::BTreeMap;

/// Python `doctest` option flag values (from `doctest/__init__.py`).
pub mod flags {
    /// `DONT_ACCEPT_TRUE_FOR_1`.
    pub const DONT_ACCEPT_TRUE_FOR_1: u32 = 1;
    /// `DONT_ACCEPT_BLANKLINE`.
    pub const DONT_ACCEPT_BLANKLINE: u32 = 2;
    /// `NORMALIZE_WHITESPACE`.
    pub const NORMALIZE_WHITESPACE: u32 = 4;
    /// `ELLIPSIS`.
    pub const ELLIPSIS: u32 = 8;
    /// `SKIP`.
    pub const SKIP: u32 = 16;
    /// `IGNORE_EXCEPTION_DETAIL`.
    pub const IGNORE_EXCEPTION_DETAIL: u32 = 32;
    /// `REPORT_ONLY_FIRST_FAILURE`.
    pub const REPORT_ONLY_FIRST_FAILURE: u32 = 512;

    /// Upstream `doctest_default_flags` default.
    pub const DEFAULT: u32 = DONT_ACCEPT_TRUE_FOR_1 | ELLIPSIS | IGNORE_EXCEPTION_DETAIL;
}

/// Maps a `doctest.OPTIONFLAGS_BY_NAME` key to its bit value.
#[must_use]
pub fn option_flag(name: &str) -> Option<u32> {
    match name.trim().to_ascii_uppercase().as_str() {
        "DONT_ACCEPT_TRUE_FOR_1" => Some(flags::DONT_ACCEPT_TRUE_FOR_1),
        "DONT_ACCEPT_BLANKLINE" => Some(flags::DONT_ACCEPT_BLANKLINE),
        "NORMALIZE_WHITESPACE" => Some(flags::NORMALIZE_WHITESPACE),
        "ELLIPSIS" => Some(flags::ELLIPSIS),
        "SKIP" => Some(flags::SKIP),
        "IGNORE_EXCEPTION_DETAIL" => Some(flags::IGNORE_EXCEPTION_DETAIL),
        "REPORT_ONLY_FIRST_FAILURE" => Some(flags::REPORT_ONLY_FIRST_FAILURE),
        "REPORT_UDIFF" => Some(64),
        "REPORT_CDIFF" => Some(128),
        "REPORT_NDIFF" => Some(256),
        "FAIL_FAST" => Some(1024),
        _ => None,
    }
}

/// Parses a directive `:options:` string such as `+ELLIPSIS, -NORMALIZE_WHITESPACE`.
///
/// Returns the flag map and one diagnostic per malformed token, matching
/// upstream's `TestDirective.run()` warnings.
#[must_use]
pub fn parse_option_string(raw: &str) -> (BTreeMap<u32, bool>, Vec<String>) {
    let mut options = BTreeMap::new();
    let mut warnings = Vec::new();
    for token in raw.replace(',', " ").split_whitespace() {
        let Some(prefix) = token.chars().next() else { continue };
        if prefix != '+' && prefix != '-' {
            warnings.push(format!("missing '+' or '-' in '{token}' option."));
            continue;
        }
        let name = &token[1..];
        let Some(flag) = option_flag(name) else {
            warnings.push(format!("'{name}' is not a valid option."));
            continue;
        };
        options.insert(flag, prefix == '+');
    }
    (options, warnings)
}

/// Collapses all whitespace runs to single spaces and trims the result, as
/// Python's `doctest` does under `NORMALIZE_WHITESPACE`.
#[must_use]
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compares `actual` to `want` with `ELLIPSIS` semantics: every `...` in
/// `want` matches any (possibly empty) substring of `actual`, including
/// newlines. Anchored at both ends, like `doctest.OutputChecker`.
#[must_use]
pub fn ellipsis_match(want: &str, actual: &str) -> bool {
    let pieces: Vec<&str> = want.split("...").collect();
    if pieces.len() == 1 {
        return want == actual;
    }
    let mut rest = actual;
    for (index, piece) in pieces.iter().enumerate() {
        if index == 0 {
            if !rest.starts_with(piece) {
                return false;
            }
            rest = &rest[piece.len()..];
        } else if index + 1 == pieces.len() {
            if !rest.ends_with(piece) || rest.len() < piece.len() {
                return false;
            }
            let consumed = rest.len() - piece.len();
            rest = &rest[..consumed];
        } else if let Some(found) = rest.find(piece) {
            rest = &rest[found + piece.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Compares a captured output to the expected text under a flag bitmask.
#[must_use]
pub fn outputs_match(want: &str, actual: &str, flags: u32) -> bool {
    let mut want = want.to_owned();
    let mut actual = actual.to_owned();
    if flags & flags::NORMALIZE_WHITESPACE != 0 {
        want = normalize_whitespace(&want);
        actual = normalize_whitespace(&actual);
    }
    if flags & flags::ELLIPSIS != 0 {
        return ellipsis_match(&want, &actual);
    }
    want == actual
}

/// A minimal upstream-shaped failure diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureDiff {
    /// Expected output as written in the directive / doctest block.
    pub expected: String,
    /// Captured output.
    pub got: String,
    /// Optional exception message extracted from the expected output.
    pub exception: Option<String>,
}

impl std::fmt::Display for FailureDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Expected:")?;
        for line in self.expected.lines() {
            writeln!(f, "    {line}")?;
        }
        writeln!(f, "Got:")?;
        for line in self.got.lines() {
            writeln!(f, "    {line}")?;
        }
        Ok(())
    }
}

/// Builds a failure diff, extracting a `Traceback ... Exception: msg` tail in
/// the same way as `doctest.DocTestParser._EXCEPTION_RE`.
#[must_use]
pub fn failure_diff(expected: &str, got: &str) -> FailureDiff {
    let exception = expected
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.split_once(':'))
        .map(|(kind, message)| format!("{}: {}", kind.trim(), message.trim()));
    FailureDiff { expected: expected.to_owned(), got: got.to_owned(), exception }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_match_python_doctest() {
        assert_eq!(flags::DEFAULT, 41);
        assert_eq!(option_flag("ELLIPSIS"), Some(8));
        assert_eq!(option_flag("normalize_whitespace"), Some(4));
        assert_eq!(option_flag("BOGUS"), None);
    }

    #[test]
    fn option_parsing_reports_malformed_tokens() {
        let (options, warnings) = parse_option_string("+ELLIPSIS, -NORMALIZE_WHITESPACE, SKIP");
        assert_eq!(options.get(&8), Some(&true));
        assert_eq!(options.get(&4), Some(&false));
        assert_eq!(warnings, vec![
            "missing '+' or '-' in 'SKIP' option.".to_owned(),
        ]);
    }

    #[test]
    fn ellipsis_and_whitespace_normalization() {
        assert!(ellipsis_match("a...z", "abcdefz"));
        assert!(ellipsis_match("a...z", "az"));
        assert!(!ellipsis_match("a...z", "zabc"));
        assert!(outputs_match("one   two", "one two", flags::NORMALIZE_WHITESPACE));
        assert!(outputs_match("value: ...", "value: 42", flags::ELLIPSIS));
    }

    #[test]
    fn failure_diff_extracts_exception() {
        let diff = failure_diff("Traceback (most recent call last):\nZeroDivisionError: division by zero", "boom");
        assert_eq!(diff.exception.as_deref(), Some("ZeroDivisionError: division by zero"));
        assert!(diff.to_string().contains("Expected:"));
    }
}
