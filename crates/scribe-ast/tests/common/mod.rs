//! Shared helpers for the pseudo-XML differential tests.
//!
//! The parser here is deliberately small: it round-trips the subset of
//! Docutils pseudo-XML that our fixtures use — element start tags, text lines,
//! and string/bool/list attributes. Attribute *types* are inferred from a
//! fixed name table (Docutils pseudo-XML is lossy: it does not record types).
//! Node tags used by the fixtures are mapped to `NodeKind` via `tag_name()`.

#![allow(dead_code)]

use std::path::PathBuf;

use scribe_ast::{AttributeValue, Document, NodeId, NodeKind};

/// Workspace root, derived from the test crate's manifest directory.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root exists")
}

/// Path to the oracle Python interpreter.
pub fn oracle_python() -> PathBuf {
    workspace_root().join(".oracle-venv").join("bin").join("python")
}

/// Path to `sphinx-build` in the oracle venv.
pub fn oracle_sphinx_build() -> PathBuf {
    workspace_root()
        .join(".oracle-venv")
        .join("bin")
        .join("sphinx-build")
}

/// Parse a pseudo-XML string into a `Document` (the supported subset is
/// documented at the top of this module).
pub fn parse_pseudoxml(input: &str) -> Document {
    let mut doc = Document::new(None);
    let mut stack: Vec<NodeId> = vec![doc.root];

    for line in input.lines() {
        let leading = line.len() - line.trim_start_matches(' ').len();
        let level = leading / 4;
        // Strip exactly `level * 4` columns. The remaining leading spaces are
        // part of the text content (Docutils pseudo-XML is ambiguous here; we
        // assume content never starts with 4+ spaces, true for our fixtures).
        let content = &line[level * 4..];
        if content.is_empty() {
            continue;
        }
        while stack.len() <= level + 1 {
            stack.push(doc.root);
        }
        let parent = stack[level];

        if let Some(tag_content) = content.strip_prefix('<') {
            let tag_end = tag_content.find('>').expect("element line has '>'");
            let inside = &tag_content[..tag_end];
            let (tag, attr_str) = match inside.find(char::is_whitespace) {
                Some(pos) => (&inside[..pos], &inside[pos..]),
                None => (inside, ""),
            };
            let attrs = parse_attributes(attr_str);
            if level == 0 && tag == "document" {
                for (key, value) in attrs {
                    doc.root_mut().attrs.insert(key, value);
                }
                stack[1] = doc.root;
                continue;
            }
            let kind = kind_from_tag(tag).unwrap_or(NodeKind::Container);
            let id = doc.add(kind, Some(parent)).expect("valid parent");
            for (key, value) in attrs {
                doc.get_mut(id).expect("just added").attrs.insert(key, value);
            }
            stack[level + 1] = id;
        } else {
            doc.new_text(content, Some(parent)).expect("valid parent");
        }
    }
    doc
}

fn parse_attributes(attr_str: &str) -> Vec<(String, AttributeValue)> {
    let mut out = Vec::new();
    let mut rest = attr_str;
    while let Some(eq) = rest.find("=\"") {
        let name = rest[..eq].trim().to_owned();
        let after = &rest[eq + 2..];
        let end = after.find('"').expect("closing quote");
        let raw = &after[..end];
        out.push((name.clone(), infer_value(&name, raw)));
        rest = &after[end + 1..];
    }
    out
}

fn infer_value(name: &str, raw: &str) -> AttributeValue {
    if matches!(name, "ids" | "classes" | "names" | "dupnames") {
        return AttributeValue::List(unescape_list(raw));
    }
    if matches!(
        name,
        "refexplicit"
            | "refspecific"
            | "refwarn"
            | "translatable"
            | "anonymous"
            | "is_multiline"
            | "add_permalink"
    ) {
        return AttributeValue::Bool(raw == "1");
    }
    AttributeValue::Str(raw.to_owned())
}

fn unescape_list(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some(' ') => {
                    current.push(' ');
                    chars.next();
                }
                Some('\\') => {
                    current.push('\\');
                    chars.next();
                }
                _ => current.push('\\'),
            }
        } else if ch == ' ' {
            result.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    result.push(current);
    result
}

fn kind_from_tag(tag: &str) -> Option<NodeKind> {
    let all = [
        NodeKind::Document, NodeKind::Section, NodeKind::Title, NodeKind::Subtitle,
        NodeKind::Paragraph, NodeKind::LiteralBlock, NodeKind::BulletList,
        NodeKind::EnumeratedList, NodeKind::ListItem, NodeKind::DefinitionList,
        NodeKind::DefinitionListItem, NodeKind::Term, NodeKind::Definition,
        NodeKind::FieldList, NodeKind::Field, NodeKind::FieldName, NodeKind::FieldBody,
        NodeKind::OptionList, NodeKind::OptionListItem, NodeKind::Option,
        NodeKind::OptionString, NodeKind::OptionArgument, NodeKind::Description,
        NodeKind::LineBlock, NodeKind::Line, NodeKind::BlockQuote, NodeKind::Attribution,
        NodeKind::DoctestBlock, NodeKind::Table, NodeKind::Tgroup, NodeKind::Colspec,
        NodeKind::Thead, NodeKind::Tbody, NodeKind::Row, NodeKind::Entry,
        NodeKind::Admonition, NodeKind::Attention, NodeKind::Caution, NodeKind::Danger,
        NodeKind::Error, NodeKind::Hint, NodeKind::Important, NodeKind::Note,
        NodeKind::Tip, NodeKind::Warning, NodeKind::Sidebar, NodeKind::Topic,
        NodeKind::Rubric, NodeKind::Epigraph, NodeKind::Highlights, NodeKind::PullQuote,
        NodeKind::Compound, NodeKind::Container, NodeKind::Transition, NodeKind::Target,
        NodeKind::Reference, NodeKind::Footnote, NodeKind::FootnoteReference,
        NodeKind::Citation, NodeKind::CitationReference, NodeKind::Label, NodeKind::Math,
        NodeKind::MathBlock, NodeKind::Raw, NodeKind::Image, NodeKind::Figure,
        NodeKind::Caption, NodeKind::Legend, NodeKind::SubstitutionDefinition,
        NodeKind::SubstitutionReference, NodeKind::Comment, NodeKind::SystemMessage,
        NodeKind::Pending, NodeKind::Emphasis, NodeKind::Strong, NodeKind::Literal,
        NodeKind::Inline, NodeKind::Problematic, NodeKind::Generated, NodeKind::Toctree,
        NodeKind::PendingXref, NodeKind::PendingXrefCondition, NodeKind::Index,
        NodeKind::Desc, NodeKind::DescSignature, NodeKind::DescContent,
        NodeKind::DescName, NodeKind::DescAddname, NodeKind::DescType,
        NodeKind::DescAnnotation, NodeKind::DescReturns, NodeKind::DescParameterlist,
        NodeKind::DescParameter, NodeKind::VersionModified, NodeKind::Seealso,
        NodeKind::Highlightlang, NodeKind::Only, NodeKind::Glossary,
        NodeKind::Productionlist, NodeKind::Production, NodeKind::CompactParagraph,
        NodeKind::Centered, NodeKind::Acks, NodeKind::Hlist, NodeKind::Hlistcol,
        NodeKind::TabularColSpec, NodeKind::NumberReference, NodeKind::DownloadReference,
        NodeKind::LiteralEmphasis, NodeKind::LiteralStrong, NodeKind::Manpage,
        NodeKind::TitleReference, NodeKind::Classifier, NodeKind::OptionGroup,
    ];
    all.into_iter().find(|k| k.tag_name() == tag)
}
