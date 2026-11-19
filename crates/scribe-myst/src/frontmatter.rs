//! YAML frontmatter parsing (MyST `---` metadata blocks).
//!
//! `pulldown-cmark`'s `ENABLE_YAML_STYLE_METADATA_BLOCKS` recognises a leading
//! `---` fenced metadata block and emits its raw YAML body as `Text` events
//! between a `Tag::MetadataBlock` start/end pair. This module turns that raw
//! body into a `serde_json::Value` so callers (and later build phases) can read
//! fields such as `title`, `author`, and arbitrary project metadata.

use scribe_ast::Span;
use serde::{Deserialize, Serialize};

use crate::error::MystError;

/// A parsed MyST YAML frontmatter block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Raw YAML text between the `---` delimiters.
    pub raw: String,
    /// Parsed value; `Null` when the YAML was empty or could not be parsed.
    pub value: serde_json::Value,
    /// Byte span of the raw YAML body in the original source.
    pub span: Span,
}

impl Frontmatter {
    /// Reads a top-level mapping field as a string (if present and a string).
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.value.get(key).and_then(serde_json::Value::as_str)
    }

    /// Returns `true` when no metadata was supplied.
    pub fn is_empty(&self) -> bool {
        self.value.is_null() || self.value.as_object().is_some_and(|m| m.is_empty())
    }
}

/// Parses a raw YAML frontmatter body.
///
/// Returns the structured frontmatter plus an optional recoverable diagnostic
/// for malformed YAML. Parsing never panics or fails outright: an invalid block
/// yields a `Null` value and a diagnostic that the caller records.
pub fn parse_yaml(raw: &str, span: Span) -> (Frontmatter, Option<MystError>) {
    let source_span = miette::SourceSpan::new(
        usize::try_from(span.start).unwrap_or(0).into(),
        usize::try_from(span.end.saturating_sub(span.start)).unwrap_or(0),
    );
    let (value, error) = if raw.trim().is_empty() {
        (serde_json::Value::Null, None)
    } else {
        match serde_yaml::from_str::<serde_json::Value>(raw) {
            Ok(value) => (value, None),
            Err(err) => (
                serde_json::Value::Null,
                Some(MystError::Frontmatter {
                    message: err.to_string(),
                    span: source_span,
                }),
            ),
        }
    };
    (
        Frontmatter {
            raw: raw.to_owned(),
            value,
            span,
        },
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mapping_and_scalars() {
        let (fm, err) = parse_yaml("title: Hello\nauthor: Jane\ndraft: true\n", Span::new(4, 45));
        assert!(err.is_none());
        assert_eq!(fm.get_str("title"), Some("Hello"));
        assert_eq!(fm.value["draft"], serde_json::Value::Bool(true));
        assert_eq!(fm.span, Span::new(4, 45));
    }

    #[test]
    fn malformed_yield_diagnostic_and_null() {
        let (fm, err) = parse_yaml("a: [1, 2\n", Span::new(0, 9));
        assert!(err.is_some());
        assert!(fm.value.is_null());
    }

    #[test]
    fn empty_yaml_is_not_an_error() {
        let (fm, err) = parse_yaml("\n", Span::new(0, 1));
        assert!(err.is_none());
        assert!(fm.is_empty());
    }
}
