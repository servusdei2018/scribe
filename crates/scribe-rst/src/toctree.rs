//! The `toctree` directive handler (Phase 09).
//!
//! Upstream `sphinx/directives/other.py::TocTree.run()` builds an
//! `addnodes.toctree` node whose `entries` are `(title, ref)` pairs and whose
//! `includefiles` are the resolved child docnames. Scribe's frozen AST (§7 of
//! `ARCHITECTURE.md`) has no tuple attribute, so the raw entry list is stored
//! as three parallel string lists on the `NodeKind::Toctree` node:
//!
//! | attribute | meaning |
//! |---|---|
//! | `entries` | resolved docnames (same as `includefiles`); consumed by the frozen metadata extractor |
//! | `includefiles` | resolved docnames (URLs / `self` / glob patterns excluded) |
//! | `entry_refs` | every ref exactly as written in the content, in order |
//! | `entry_titles` | parallel explicit titles; `""` means "use the document title" |
//! | `entry_kinds` | parallel classifier: `doc`, `external`, `self`, `glob` |
//!
//! The Phase 09 collector in `scribe-core` expands glob patterns against the
//! build environment's discovered docs and writes the final, ordered
//! `entries` / `includefiles` back into the stored doctree. This mirrors
//! upstream's division of labour (the directive has access to `env.found_docs`;
//! our parser does not) while keeping the frozen AST unchanged.
//!
//! External entries additionally receive a child `reference` node carrying
//! `refuri`, so a writer can render them without re-reading the attribute
//! encoding. `self` entries likewise receive a `reference` with `refuri=""`.

use std::collections::BTreeSet;

use scribe_ast::{NodeId, NodeKind};

use crate::directive_registry::{DirectiveContext, DirectiveError};
use crate::option_spec::{OptionKind, OptionSpec, OptionValue};

/// Attribute holding the raw refs as written in the directive body.
pub const ATTR_ENTRY_REFS: &str = "entry_refs";
/// Attribute holding the parallel explicit titles (`""` = none).
pub const ATTR_ENTRY_TITLES: &str = "entry_titles";
/// Attribute holding the parallel entry classifiers.
pub const ATTR_ENTRY_KINDS: &str = "entry_kinds";
/// Attribute holding resolved child docnames (also used by metadata extraction).
pub const ATTR_ENTRIES: &str = "entries";
/// Attribute holding resolved child docnames (upstream name).
pub const ATTR_INCLUDEFIES: &str = "includefiles";
/// Attribute placed on the `comment` that replaces a non-matching `only`
/// node: the raw refs of any `toctree` directives that lived inside it.
///
/// Upstream's `TocTreeCollector` notes those directives while building the
/// environment TOCs, before `OnlyNodeTransform` strips the branch at write
/// time. Scribe applies the `only` transform earlier, so without this side
/// channel the collector would never see the directive and would emit a
/// spurious `toc.not_included` warning for its targets.
pub const ATTR_ONLY_TOC_REFS: &str = "only_toctree_refs";
/// Parallel classifier list for [`ATTR_ONLY_TOC_REFS`].
pub const ATTR_ONLY_TOC_KINDS: &str = "only_toctree_kinds";
/// Classifier value for an ordinary document reference.
pub const KIND_DOC: &str = "doc";
/// Classifier value for an external URL.
pub const KIND_EXTERNAL: &str = "external";
/// Classifier value for a `self` reference.
pub const KIND_SELF: &str = "self";
/// Classifier value for a `:glob:` pattern.
pub const KIND_GLOB: &str = "glob";

/// Classifier of one raw toctree entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawEntryKind {
    /// An ordinary (possibly globbed) document name.
    Doc,
    /// An external URL (`<schema>://...`).
    External,
    /// The literal `self` reference.
    SelfRef,
    /// A `:glob:` pattern (contains `*`, `?` or `[`).
    Glob,
}

impl RawEntryKind {
    /// Stable string written to `entry_kinds`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Doc => KIND_DOC,
            Self::External => KIND_EXTERNAL,
            Self::SelfRef => KIND_SELF,
            Self::Glob => KIND_GLOB,
        }
    }

}

/// Whether `value` matches upstream's `url_re = re.compile(r'(?P<schema>.+)://.*')`.
pub fn is_url(value: &str) -> bool {
    value
        .split_once("://")
        .is_some_and(|(scheme, _)| !scheme.is_empty())
}

/// Whether `value` matches upstream's `glob_re = re.compile(r'.*[*?\[].*')`.
pub fn is_glob(value: &str) -> bool {
    value.contains(['*', '?', '['])
}

/// Parse `Title <docname>`, mirroring `sphinx.util.nodes.explicit_title_re`
/// (`^(.+?)\s*(?<!\x00)<([^<]*?)>$`, DOTALL). Rust's regex crate has no
/// look-behind, so the single NUL guard is implemented directly.
pub fn explicit_title(entry: &str) -> Option<(String, String)> {
    if !entry.ends_with('>') {
        return None;
    }
    let inner = &entry[..entry.len() - 1];
    let lt = inner.rfind('<')?;
    let raw_title = &inner[..lt];
    if raw_title.trim().is_empty() || raw_title.contains('<') {
        return None;
    }
    let raw_ref = &inner[lt + 1..];
    if raw_ref.contains('<') || raw_ref.contains('>') {
        return None;
    }
    // The upstream negative look-behind only rejects a NUL immediately before
    // the opening angle bracket.
    if raw_title.ends_with('\0') {
        return None;
    }
    Some((raw_title.trim_end().to_owned(), raw_ref.to_owned()))
}

/// Upstream `sphinx.util.docname_join`: resolve `docname` relative to the
/// directory containing `basedocname` with POSIX path semantics.
pub fn docname_join(basedocname: &str, docname: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    let base = basedocname.trim_end_matches('/');
    if let Some((parent, _)) = base.rsplit_once('/') {
        stack.extend(parent.split('/').filter(|part| !part.is_empty() && *part != "."));
    }
    for part in docname.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

/// Docutils' `int_or_nothing`: a bare `:numbered:` means 999.
pub(crate) fn int_or_nothing(raw: &str) -> Result<OptionValue, String> {
    if raw.trim().is_empty() {
        return Ok(OptionValue::Int(999));
    }
    raw.trim()
        .parse::<i64>()
        .map(OptionValue::Int)
        .map_err(|error| format!("invalid integer '{raw}': {error}"))
}

/// Option specification for the directive, transcribed from upstream.
pub(crate) fn option_spec() -> OptionSpec {
    OptionSpec::empty()
        .with("maxdepth", OptionKind::Int)
        .with("name", OptionKind::Unchanged)
        .with("class", OptionKind::ClassOption)
        .with("caption", OptionKind::UnchangedRequired)
        .with("glob", OptionKind::Flag)
        .with("hidden", OptionKind::Flag)
        .with("includehidden", OptionKind::Flag)
        .with("numbered", OptionKind::Custom("int_or_nothing", int_or_nothing))
        .with("titlesonly", OptionKind::Flag)
        .with("reversed", OptionKind::Flag)
}

/// Run the directive: emit a `compound` wrapper containing a `toctree` node.
pub(crate) fn run(ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
    let parent_doc = ctx.global.source_name.clone().unwrap_or_default();
    let maxdepth = ctx.option_int("maxdepth").unwrap_or(-1);
    let caption = ctx.option_str("caption").map(str::to_owned);
    let name = ctx.option_str("name").map(str::to_owned);
    let glob = ctx.flag("glob");
    let hidden = ctx.flag("hidden");
    let includehidden = ctx.flag("includehidden");
    let numbered = ctx.option_int("numbered").unwrap_or(0);
    let titlesonly = ctx.flag("titlesonly");
    let reversed = ctx.flag("reversed");

    let mut suffixes: Vec<String> = ctx
        .global
        .config
        .as_ref()
        .and_then(|config| config.get_dict("source_suffix"))
        .map(|dict| dict.keys().cloned().collect())
        .unwrap_or_default();
    suffixes.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut refs: Vec<String> = Vec::new();
    let mut titles: Vec<String> = Vec::new();
    let mut kinds: Vec<String> = Vec::new();
    let mut includefiles: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for line in ctx.content.lines() {
        let entry = line.trim();
        if entry.is_empty() {
            continue;
        }
        let (title, raw_ref) = explicit_title(entry).unwrap_or_else(|| (String::new(), entry.to_owned()));
        let mut kind = if is_url(&raw_ref) {
            RawEntryKind::External
        } else if raw_ref == "self" {
            RawEntryKind::SelfRef
        } else {
            RawEntryKind::Doc
        };
        if glob && kind == RawEntryKind::Doc && is_glob(&raw_ref) {
            kind = RawEntryKind::Glob;
        }

        let resolved = match kind {
            RawEntryKind::External | RawEntryKind::SelfRef => raw_ref.clone(),
            RawEntryKind::Glob => docname_join(&parent_doc, &raw_ref),
            RawEntryKind::Doc => {
                let mut docname = raw_ref.clone();
                for suffix in &suffixes {
                    if docname.ends_with(suffix) {
                        docname.truncate(docname.len() - suffix.len());
                        break;
                    }
                }
                docname_join(&parent_doc, &docname)
            }
        };

        if kind == RawEntryKind::Doc {
            // Upstream keeps duplicate entries but warns; the collector makes
            // the final dedup decision when it builds the hierarchy.
            let _ = seen.insert(resolved.clone());
            includefiles.push(resolved.clone());
        }
        // Keep the ref as written. The collector owns suffix stripping,
        // docname_join and `:glob:` expansion because only it knows the
        // discovered document set.
        refs.push(raw_ref);
        titles.push(title);
        kinds.push(kind.as_str().to_owned());
    }

    let wrapper = ctx.add(NodeKind::Compound, ctx.parent)?;
    {
        let mut classes = vec!["toctree-wrapper".to_owned()];
        classes.extend(ctx.classes());
        let names = ctx.names();
        let node = ctx.doc.get_mut(wrapper)?;
        node.set_list("classes", classes);
        if !names.is_empty() {
            node.set_list(
                "ids",
                names.iter().map(|name| crate::names::make_id(name)).collect(),
            );
            node.set_list("names", names);
        }
    }

    let toc = ctx.add(NodeKind::Toctree, wrapper)?;
    {
        let node = ctx.doc.get_mut(toc)?;
        node.set_str("parent", parent_doc);
        node.set_list(ATTR_ENTRIES, includefiles.clone());
        node.set_list(ATTR_INCLUDEFIES, includefiles);
        node.set_list(ATTR_ENTRY_REFS, refs.clone());
        node.set_list(ATTR_ENTRY_TITLES, titles.clone());
        node.set_list(ATTR_ENTRY_KINDS, kinds.clone());
        node.attrs.set_int("maxdepth", maxdepth);
        if let Some(caption) = &caption {
            node.set_str("caption", caption);
            node.set_str("rawcaption", caption);
        }
        if let Some(value) = &name {
            node.set_str("name", value);
        }
        node.attrs.set_bool("glob", glob);
        node.attrs.set_bool("hidden", hidden);
        node.attrs.set_bool("includehidden", includehidden);
        node.attrs.set_int("numbered", numbered);
        node.attrs.set_bool("titlesonly", titlesonly);
    }

    // External and `self` references are materialised as `reference` children
    // carrying `refuri`, exactly as upstream's resolved toctree does. Ordinary
    // document entries stay unresolved until the writer/Phase 10 pipeline.
    for (index, (reference, kind)) in refs.iter().zip(kinds.iter()).enumerate() {
        if kind != KIND_EXTERNAL && kind != KIND_SELF {
            continue;
        }
        let title = titles
            .get(index)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| reference.clone());
        let line = ctx.line + index as u32 + 1;
        let ref_id = ctx.add(NodeKind::Reference, toc)?;
        {
            let node = ctx.doc.get_mut(ref_id)?;
            node.set_str("refuri", if kind == KIND_SELF { "" } else { reference });
            node.attrs.set_bool("internal", kind == KIND_SELF);
        }
        ctx.add_text(title, ref_id, line)?;
    }

    // Upstream reverses the *expanded* entry list, so the collector applies
    // `:reversed:` after glob expansion; keep the flag for introspection.
    if reversed {
        ctx.doc.get_mut(toc)?.attrs.set_bool("reversed", true);
    }

    Ok(vec![wrapper])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_and_glob_classifiers_match_upstream_regexes() {
        assert!(is_url("http://example.com"));
        assert!(is_url("https://sphinx-doc.org/"));
        assert!(!is_url("foo"));
        assert!(!is_url("://bad"));
        assert!(is_glob("bar/*"));
        assert!(is_glob("bar?"));
        assert!(is_glob("ba[rR]"));
        assert!(!is_glob("bar"));
    }

    #[test]
    fn explicit_titles_follow_upstream_regex() {
        assert_eq!(
            explicit_title("Some Title <doc>"),
            Some(("Some Title".to_owned(), "doc".to_owned()))
        );
        assert_eq!(
            explicit_title("A < b >"),
            Some(("A".to_owned(), " b ".to_owned()))
        );
        assert_eq!(explicit_title("doc"), None);
        assert_eq!(explicit_title("<doc>"), None);
    }

    #[test]
    fn docname_join_matches_posixpath() {
        assert_eq!(docname_join("index", "foo"), "foo");
        assert_eq!(docname_join("a/b", "c"), "a/c");
        assert_eq!(docname_join("foo", "../bar"), "bar");
        assert_eq!(docname_join("a/b/c", "../../d"), "d");
    }

    #[test]
    fn bare_numbered_is_999() {
        assert_eq!(int_or_nothing(""), Ok(OptionValue::Int(999)));
        assert_eq!(int_or_nothing("2"), Ok(OptionValue::Int(2)));
        assert!(int_or_nothing("abc").is_err());
    }
}
