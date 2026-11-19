//! Canonical Sphinx warning codes and `suppress_warnings` filtering.
//!
//! Upstream logs every warning with a `type` and an optional `subtype`, which
//! together form the `type.subtype` code shown by `show_warning_types` and
//! matched by `suppress_warnings` (see `sphinx/util/logging.py`). Scribe's
//! internal diagnostics carry stable `scribe::*` codes instead; this module is
//! the single source of truth that maps them onto upstream codes and applies
//! the same suppression rules.
//!
//! Suppression follows `sphinx.util.logging.is_suppressed_warning` exactly:
//! a pattern suppresses a warning when it equals the bare `type`, equals
//! `type.*` (the wildcard form Sphinx supports), or equals `type.subtype`.
//! Arbitrary partial globs are deliberately *not* accepted — Sphinx treats
//! them literally, and accepting them would diverge from upstream on the
//! documented `suppress_warnings` patterns.

/// A canonical Sphinx warning code (`type` plus optional `subtype`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WarningCode {
    /// Upstream `type` (for example `ref`, `toc`, `misc`).
    pub kind: String,
    /// Upstream `subtype` (for example `doc`, `not_readable`). Empty when the
    /// upstream warning has no subtype (for example bare `docutils`).
    pub subtype: String,
}

impl WarningCode {
    /// A code with no subtype (upstream `type='docutils'`).
    pub fn bare(kind: impl Into<String>) -> Self {
        Self { kind: kind.into(), subtype: String::new() }
    }

    /// A code with a subtype (upstream `type='ref', subtype='doc'`).
    pub fn typed(kind: impl Into<String>, subtype: impl Into<String>) -> Self {
        Self { kind: kind.into(), subtype: subtype.into() }
    }

    /// Renders the canonical `type.subtype` string.
    pub fn as_str(&self) -> String {
        if self.subtype.is_empty() {
            self.kind.clone()
        } else {
            format!("{}.{}", self.kind, self.subtype)
        }
    }

    /// Parses a canonical `type.subtype` string (or a bare `type`) back into a
    /// structured code. Unknown shapes are treated as a bare type.
    pub fn from_canonical(canonical: &str) -> Self {
        match canonical.split_once('.') {
            Some((kind, subtype)) => Self::typed(kind, subtype),
            None => Self::bare(canonical),
        }
    }
}

/// Stable scribe diagnostic code → canonical Sphinx warning code.
///
/// Each entry is `(scribe_code, upstream_type, upstream_subtype)`. The
/// left-hand side strings are the codes already emitted by `scribe-domains`,
/// `scribe-core` and `scribe-cli`; they are never renamed, so the conformance
/// baseline keeps using them unchanged.
pub static WARNING_CODE_MAP: &[(&str, &str, &str)] = &[
    // Cross-reference / domain diagnostics. `missing_reference` is
    // data-dependent (see `missing_reference_code`) and maps to bare `ref`.
    ("scribe::domains::missing_reference", "ref", ""),
    ("scribe::domains::duplicate_object", "duplicate_declaration", ""),
    ("scribe::domains::malformed_object", "misc", "malformed_object"),
    ("scribe::domains::inconsistency", "misc", "inconsistent_references"),
    ("scribe::domains::intersphinx", "ref", ""),
    ("scribe::domains::registration", "misc", ""),
    ("scribe::xref::unresolved", "ref", ""),
    // Toctree diagnostics.
    ("scribe::toc::circular", "toc", "circular"),
    ("scribe::toc::not_included", "toc", "not_included"),
    ("scribe::toc::not_readable", "toc", "not_readable"),
    ("scribe::toc::empty_glob", "toc", "empty_glob"),
    ("scribe::toc::duplicate_entry", "toc", "duplicate_entry"),
    ("scribe::toc::excluded", "toc", "excluded"),
    // RST / config / discovery diagnostics.
    ("scribe::rst::parse", "docutils", ""),
    ("scribe::rst::directive", "docutils", ""),
    ("scribe::config::load_failed", "config", "load_failed"),
    ("scribe::discovery::warning", "misc", ""),
    ("scribe::check::no_sources", "misc", ""),
    ("scribe::check::read_failed", "misc", ""),
];

/// Canonical Sphinx warning codes understood by `suppress_warnings`.
///
/// This is the list published in `sphinx/doc/usage/configuration.rst` plus the
/// documented first-party extension codes. It is intentionally data, not an
/// enum, so third-party codes can coexist without breaking compatibility.
pub static SUPPORTED_SUPPRESSION_CODES: &[&str] = &[
    "app.add_directive", "app.add_generic_role", "app.add_node", "app.add_role",
    "app.add_source_parser", "autodoc", "autodoc.import_object", "config.cache",
    "docutils", "download.not_readable", "duplicate_declaration.c",
    "duplicate_declaration.cpp", "epub.duplicated_toc_entry", "epub.unknown_project_files",
    "i18n.babel", "i18n.inconsistent_references", "i18n.not_readable",
    "i18n.not_writeable", "image.not_readable", "index", "misc.copy_overwrite",
    "misc.highlighting_failure", "ref.any", "ref.citation", "ref.doc", "ref.equation",
    "ref.footnote", "ref.keyword", "ref.numref", "ref.option", "ref.python", "ref.ref",
    "ref.term", "source_code_parser.c", "source_code_parser.cpp", "toc.circular",
    "toc.duplicate_entry", "toc.empty_glob", "toc.excluded", "toc.no_title",
    "toc.not_included", "toc.not_readable", "toc.secnum",
];

/// Maps a stable scribe code to a canonical Sphinx warning code.
pub fn canonical_code(scribe_code: &str) -> Option<WarningCode> {
    WARNING_CODE_MAP
        .iter()
        .find(|(code, _, _)| *code == scribe_code)
        .map(|(_, kind, subtype)| WarningCode::typed(*kind, *subtype))
}

/// Canonical code for a `missing_reference` diagnostic of reference type `typ`.
///
/// Sphinx logs most missing references as `ref.<reftype>`; Python-domain
/// object references are published as `ref.python` (see the documented
/// `suppress_warnings` codes).
pub fn missing_reference_code(typ: &str) -> WarningCode {
    const PY_TYPES: &[&str] = &[
        "func", "class", "meth", "mod", "attr", "data", "exc", "obj", "deco",
        "const", "classmethod", "staticmethod", "property", "type", "exception",
        "attribute", "function", "method", "module",
    ];
    if PY_TYPES.contains(&typ) {
        WarningCode::typed("ref", "python")
    } else if typ.is_empty() {
        WarningCode::bare("ref")
    } else {
        WarningCode::typed("ref", typ)
    }
}

/// Classifies a plain (string) build warning by its message content.
///
/// Builder warnings are historically unstructured strings, so this function
/// supplies the canonical code used for `suppress_warnings` filtering without
/// changing the displayed text (which the conformance harness compares).
pub fn classify_warning_message(message: &str) -> WarningCode {
    let lower = message.to_ascii_lowercase();
    if lower.contains("highlight") || lower.contains("lexer name") || lower.contains("pygments") {
        WarningCode::typed("misc", "highlighting_failure")
    } else if lower.contains("circular") || lower.contains("toctree contains reference to itself") {
        WarningCode::typed("toc", "circular")
    } else if lower.contains("not included in any toctree")
        || lower.contains("isn't included in any toctree")
    {
        WarningCode::typed("toc", "not_included")
    } else if lower.contains("glob") && (lower.contains("empty") || lower.contains("no files")) {
        WarningCode::typed("toc", "empty_glob")
    } else if lower.contains("duplicate object description") || lower.contains("duplicate declaration") {
        WarningCode::bare("duplicate_declaration")
    } else if lower.contains("import") && (lower.contains("autodoc") || lower.contains("module")) {
        WarningCode::typed("autodoc", "import_object")
    } else if lower.contains("reference target not found")
        || lower.contains("undefined label")
        || lower.contains("unknown target")
        || lower.contains("cross-reference")
    {
        WarningCode::bare("ref")
    } else {
        WarningCode::bare("misc")
    }
}

/// True when `patterns` suppresses `code`.
///
/// Implements `sphinx.util.logging.is_suppressed_warning` verbatim:
/// * an exact bare `type` suppresses the whole family;
/// * `type.*` suppresses every subtype of that family;
/// * an exact `type.subtype` suppresses just that warning.
pub fn is_suppressed(code: &WarningCode, patterns: &[String]) -> bool {
    let full = code.as_str();
    let wildcard = format!("{}.*", code.kind);
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        pattern == code.kind || pattern == wildcard || pattern == full
    })
}

/// Convenience form: suppress a stable scribe code directly.
pub fn is_code_suppressed(scribe_code: &str, patterns: &[String]) -> bool {
    match canonical_code(scribe_code) {
        Some(code) => is_suppressed(&code, patterns),
        None => false,
    }
}

/// Alias kept explicit for call sites that want the name to match upstream.
pub fn is_supported(code: &WarningCode, patterns: &[String]) -> bool {
    is_suppressed(code, patterns)
}

/// Returns the `suppress_warnings` list from a config value, defaulting to
/// empty. Kept here so builders and the CLI share one extraction path.
pub fn suppression_patterns(extract: impl Fn(&str) -> Option<Vec<String>>) -> Vec<String> {
    extract("suppress_warnings").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_codes_cover_upstream_families() {
        assert_eq!(canonical_code("scribe::toc::circular").unwrap().as_str(), "toc.circular");
        assert_eq!(canonical_code("scribe::toc::not_included").unwrap().as_str(), "toc.not_included");
        assert_eq!(canonical_code("scribe::rst::parse").unwrap().as_str(), "docutils");
        assert_eq!(canonical_code("scribe::xref::unresolved").unwrap().as_str(), "ref");
        assert_eq!(missing_reference_code("doc").as_str(), "ref.doc");
        assert_eq!(missing_reference_code("term").as_str(), "ref.term");
        assert_eq!(missing_reference_code("option").as_str(), "ref.option");
        assert_eq!(missing_reference_code("func").as_str(), "ref.python");
        assert_eq!(classify_warning_message("unknown lexer name 'x'").as_str(), "misc.highlighting_failure");
    }

    #[test]
    fn suppression_matches_upstream_semantics() {
        let code = WarningCode::typed("ref", "doc");
        assert!(is_suppressed(&code, &["ref".to_owned()]));
        assert!(is_suppressed(&code, &["ref.*".to_owned()]));
        assert!(is_suppressed(&code, &["ref.doc".to_owned()]));
        assert!(!is_suppressed(&code, &["ref.term".to_owned()]));
        assert!(!is_suppressed(&code, &["toc".to_owned()]));
        // Upstream treats partial globs literally: these must NOT suppress.
        assert!(!is_suppressed(&code, &["ref.d*".to_owned()]));
        assert!(!is_suppressed(&code, &["*.doc".to_owned()]));
        assert!(!is_suppressed(&code, &["*".to_owned()]));
    }

    #[test]
    fn bare_codes_suppress_whole_family() {
        let docutils = WarningCode::bare("docutils");
        assert!(is_suppressed(&docutils, &["docutils".to_owned()]));
        assert!(is_suppressed(&docutils, &["docutils.*".to_owned()]));
        assert!(!is_suppressed(&docutils, &["doc*".to_owned()]));
        assert!(!is_suppressed(&docutils, &["docutils.other".to_owned()]));
    }

    #[test]
    fn scribe_codes_can_be_suppressed_by_canonical_patterns() {
        assert!(is_code_suppressed("scribe::toc::circular", &["toc.circular".to_owned()]));
        assert!(is_code_suppressed("scribe::rst::parse", &["docutils".to_owned()]));
        assert!(!is_code_suppressed("scribe::toc::circular", &["toc.not_included".to_owned()]));
    }

    #[test]
    fn all_documented_suppression_codes_are_listed() {
        for code in ["ref.ref", "ref.doc", "ref.term", "ref.option", "ref.python",
                     "toc.not_readable", "toc.empty_glob", "misc.highlighting_failure",
                     "autodoc.import_object"] {
            assert!(SUPPORTED_SUPPRESSION_CODES.contains(&code), "missing {code}");
        }
    }
}
