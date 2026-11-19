//! Sphinx-compatible shell-style glob matching.
//!
//! This is a direct port of `sphinx/util/matching.py::_translate_pattern` and
//! `Matcher` from the pinned oracle. The important differences from Python's
//! `fnmatch` are:
//!
//! * a single `*` does **not** cross `/`;
//! * `?` does not cross `/`;
//! * `**` matches across `/`;
//! * a negated character class (`[!abc]`) also excludes `/`;
//! * `Matcher` additionally expands `**/foo` to `foo` so that an exclude
//!   pattern matches a file at the root of the source tree.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Canonicalize a native path to Sphinx's `/`-separated form.
pub fn canon_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Canonicalize a path and normalize it to Unicode NFC.
///
/// This is the exact behavior of upstream `path_stabilize()`: `canon_path()`
/// followed by `unicodedata.normalize('NFC', ...)`.
pub fn path_stabilize(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    canon_path(path).nfc().collect()
}

/// Translate one Sphinx glob pattern to an anchored regular expression.
///
/// The result is anchored at both ends, matching Python `re.match(...)` with
/// the trailing `$` that upstream appends.
pub fn translate_pattern(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let length = chars.len();
    let mut index = 0usize;
    let mut result = String::from("^");

    while index < length {
        let character = chars[index];
        index += 1;
        match character {
            '*' => {
                if index < length && chars[index] == '*' {
                    index += 1;
                    result.push_str(".*");
                } else {
                    result.push_str("[^/]*");
                }
            }
            '?' => result.push_str("[^/]"),
            '[' => {
                let mut end = index;
                if end < length && chars[end] == '!' {
                    end += 1;
                }
                if end < length && chars[end] == ']' {
                    end += 1;
                }
                while end < length && chars[end] != ']' {
                    end += 1;
                }
                if end >= length {
                    result.push_str("\\[");
                } else {
                    let mut stuff: String = chars[index..end].iter().collect();
                    index = end + 1;
                    let negative = stuff.starts_with('!');
                    stuff = escape_class_contents(&stuff);
                    if negative {
                        // A negated class must not match a slash either.
                        result.push_str("[^/");
                        result.push_str(&stuff[1..]);
                        result.push(']');
                    } else {
                        result.push('[');
                        result.push_str(&stuff);
                        result.push(']');
                    }
                }
            }
            other => result.push_str(&regex::escape(&other.to_string())),
        }
    }
    result.push('$');
    result
}

fn escape_class_contents(contents: &str) -> String {
    let mut out = String::with_capacity(contents.len());
    for character in contents.chars() {
        match character {
            '\\' | ']' | '[' => {
                out.push('\\');
                out.push(character);
            }
            '-' => out.push_str("\\-"),
            _ => out.push(character),
        }
    }
    out
}

/// Compile raw patterns without the `**/` expansion used by [`Matcher`].
///
/// This mirrors `compile_matchers()` in upstream `util/matching.py`, which is
/// what `get_matching_files()` uses for both inclusion and exclusion.
pub fn compile_matchers(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| Regex::new(&translate_pattern(pattern)).ok())
        .collect()
}

/// A compiled set of shell-style patterns with Sphinx's `Matcher` semantics.
#[derive(Clone, Debug)]
pub struct Matcher {
    patterns: Vec<Regex>,
}

impl Matcher {
    /// Build a matcher.
    ///
    /// In addition to the supplied patterns, every pattern beginning with
    /// `**/` is also compiled without that prefix, matching upstream's
    /// `Matcher.__init__`.
    pub fn new<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let supplied: Vec<String> = patterns
            .into_iter()
            .map(|pattern| pattern.as_ref().to_owned())
            .collect();
        let mut all = supplied.clone();
        all.extend(
            supplied
                .iter()
                .filter(|pattern| pattern.starts_with("**/"))
                .map(|pattern| pattern[3..].to_owned()),
        );
        Self {
            patterns: compile_matchers(&all),
        }
    }

    /// Build a matcher that does **not** expand `**/` prefixes.
    pub fn raw(patterns: &[String]) -> Self {
        Self {
            patterns: compile_matchers(patterns),
        }
    }

    /// Return whether `path` matches any pattern.
    pub fn is_match(&self, path: &str) -> bool {
        let canonical = canon_path(path);
        self.patterns.iter().any(|pattern| pattern.is_match(&canonical))
    }

    /// Check a filesystem path (using its UTF-8 lossy representation).
    pub fn is_match_path(&self, path: &Path) -> bool {
        self.is_match(&path.to_string_lossy())
    }

    /// Number of successfully compiled patterns.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Whether no patterns were compiled.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// Test a single path against a set of patterns with [`Matcher`] semantics.
pub fn match_any(path: &str, patterns: &[String]) -> bool {
    Matcher::new(patterns.iter()).is_match(path)
}

/// Effective include/exclude glob specification used by source discovery.
///
/// This is the serializable form of the patterns held by the build
/// environment; it combines configuration values with upstream's built-in
/// exclusions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobSpec {
    /// Patterns a path must match to be considered a source (`**` by default).
    pub include: Vec<String>,
    /// Patterns that exclude a path from discovery.
    pub exclude: Vec<String>,
}

impl GlobSpec {
    /// Assemble a spec from Scribe configuration.
    pub fn from_config(config: &scribe_config::Config) -> Self {
        let mut exclude = config.get_strings("exclude_patterns").unwrap_or_default();
        exclude.extend(config.get_strings("templates_path").unwrap_or_default());
        exclude.extend(
            crate::discovery::EXCLUDE_PATHS
                .iter()
                .map(|pattern| (*pattern).to_owned()),
        );
        Self {
            include: config.get_strings("include_patterns").unwrap_or_default(),
            exclude,
        }
    }

    /// Matcher for the include patterns (empty means "**").
    pub fn include_matcher(&self) -> Matcher {
        let patterns = if self.include.is_empty() {
            vec!["**".to_owned()]
        } else {
            self.include.clone()
        };
        Matcher::raw(&patterns)
    }

    /// Matcher for the exclude patterns (with `**/` expansion).
    pub fn exclude_matcher(&self) -> Matcher {
        Matcher::new(self.exclude.iter())
    }

    /// Whether `path` is excluded by this spec.
    pub fn is_excluded(&self, path: &str) -> bool {
        self.exclude_matcher().is_match(path)
    }

    /// Whether `path` is included (and not excluded) by this spec.
    pub fn is_included(&self, path: &str) -> bool {
        self.include_matcher().is_match(path) && !self.is_excluded(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{match_any, translate_pattern, Matcher};

    #[test]
    fn translation_matches_upstream_examples() {
        assert_eq!(translate_pattern("**/index.rst"), r"^.*/index\.rst$");
        assert_eq!(translate_pattern("*.rst"), r"^[^/]*\.rst$");
        assert_eq!(translate_pattern("a?c"), r"^a[^/]c$");
        assert_eq!(translate_pattern("[!a]bc"), r"^[^/a]bc$");
    }

    #[test]
    fn single_star_does_not_cross_slashes_but_double_star_does() {
        assert!(!match_any("sub/index.rst", &["*.rst".to_owned()]));
        assert!(match_any("index.rst", &["*.rst".to_owned()]));
        assert!(match_any("sub/index.rst", &["**/*.rst".to_owned()]));
    }

    #[test]
    fn matcher_expands_double_star_slash_prefix() {
        let matcher = Matcher::new(["**/_sources".to_owned()]);
        assert!(matcher.is_match("_sources"));
        assert!(matcher.is_match("foo/_sources"));
        assert!(!matcher.is_match("foo/_sources/x.rst"));

        let raw = Matcher::raw(&["**/_sources".to_owned()]);
        assert!(!raw.is_match("_sources"));
        assert!(raw.is_match("foo/_sources"));
    }

    #[test]
    fn path_stabilize_canonicalizes_separators_and_nfc() {
        use super::path_stabilize;
        assert_eq!(path_stabilize(r"docs\sub\file.rst"), "docs/sub/file.rst");
        // "Cafe\u0301" (decomposed) composes to the single-codepoint "Café".
        assert_eq!(path_stabilize("Cafe\u{0301}.rst"), "Caf\u{00e9}.rst");
    }

    #[test]
    fn character_classes_and_windows_separators() {
        assert!(match_any("a.txt", &["[!b]*.txt".to_owned()]));
        assert!(!match_any("b.txt", &["[!b]*.txt".to_owned()]));
        assert!(match_any(r"docs\index.rst", &["docs/*.rst".to_owned()]));
    }
}
