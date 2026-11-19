//! `sphinx.ext.extlinks` role specification and `%`-format expansion.
//!
//! Upstream registers one role per `extlinks` config entry. A role is a pair
//! `(base_url, caption)`: the URI is `base_url % payload`, and the implicit
//! caption is `caption % payload` (or the full URI when `caption` is `None`).
//! Python's `%`-formatting is used upstream; only `%s` and `%%` are exposed to
//! users. This module implements that small subset without invoking Python,
//! and never panics on a malformed template (the offending template is left
//! verbatim and reported through [`ExtLinkSpec::validate`]).

use std::collections::BTreeMap;
use std::sync::Arc;

/// One configured external-link role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtLinkSpec {
    /// Name of the generated role (the key in the `extlinks` config dict).
    pub name: String,
    /// URL template, using `%s` for the payload and `%%` for a literal `%`.
    pub base_url: String,
    /// Optional caption template. `None` means "use the full URL".
    pub caption: Option<String>,
}

impl ExtLinkSpec {
    /// Build a specification and validate its templates.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        caption: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            caption,
        }
    }

    /// Human-readable problems with this specification (malformed `%` escapes).
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if let Some(problem) = invalid_format(&self.base_url) {
            issues.push(format!(
                "extlinks entry '{}': invalid base URL format: {problem}",
                self.name
            ));
        }
        if let Some(caption) = &self.caption {
            if let Some(problem) = invalid_format(caption) {
                issues.push(format!(
                    "extlinks entry '{}': invalid caption format: {problem}",
                    self.name
                ));
            }
        }
        issues
    }

    /// Expand `%s` / `%%` in `template`, substituting `value` for `%s`.
    ///
    /// An unknown `%x` sequence is left as-is (and flagged by [`Self::validate`]).
    #[must_use]
    pub fn expand(template: &str, value: &str) -> String {
        let mut out = String::with_capacity(template.len() + value.len());
        let bytes = template.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' && index + 1 < bytes.len() {
                match bytes[index + 1] {
                    b'%' => {
                        out.push('%');
                        index += 2;
                        continue;
                    }
                    b's' => {
                        out.push_str(value);
                        index += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            let ch = template[index..].chars().next().unwrap_or('\0');
            out.push(ch);
            index += ch.len_utf8();
        }
        out
    }

    /// Compute the `(uri, implicit_title)` pair for a role payload.
    #[must_use]
    pub fn resolve(&self, payload: &str) -> (String, String) {
        let uri = Self::expand(&self.base_url, payload);
        let title = match &self.caption {
            Some(template) => Self::expand(template, payload),
            None => uri.clone(),
        };
        (uri, title)
    }
}

fn invalid_format(template: &str) -> Option<String> {
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 1 >= bytes.len() {
                return Some("incomplete format specifier at end of template".to_owned());
            }
            match bytes[index + 1] {
                b'%' | b's' => index += 2,
                other => {
                    return Some(format!("unsupported format specifier '%{}'", other as char));
                }
            }
        } else {
            index += 1;
        }
    }
    None
}

/// Ordered collection of extlink roles, shared by the parser across a build.
pub type ExtLinkRoles = Arc<BTreeMap<String, ExtLinkSpec>>;

/// Build a shared, empty role map.
#[must_use]
pub fn empty_roles() -> ExtLinkRoles {
    Arc::new(BTreeMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_handles_repeated_placeholders_and_literal_percent() {
        assert_eq!(ExtLinkSpec::expand("https://x/%s/%s", "a"), "https://x/a/a");
        assert_eq!(ExtLinkSpec::expand("100%% %s", "ok"), "100% ok");
    }

    #[test]
    fn resolve_uses_caption_or_full_uri() {
        let spec = ExtLinkSpec::new("issue", "https://example/%s.html", Some("issue %s".to_owned()));
        assert_eq!(spec.resolve("42"), ("https://example/42.html".to_owned(), "issue 42".to_owned()));
        let spec = ExtLinkSpec::new("url", "https://example/%s", None);
        assert_eq!(spec.resolve("a"), ("https://example/a".to_owned(), "https://example/a".to_owned()));
    }

    #[test]
    fn malformed_templates_are_reported_without_panics() {
        assert!(ExtLinkSpec::new("bad", "https://x/%", None).validate()[0].contains("incomplete"));
        assert!(ExtLinkSpec::new("bad", "https://x/%d", None).validate()[0].contains("unsupported"));
        assert_eq!(ExtLinkSpec::expand("https://x/%", "v"), "https://x/%");
    }
}
