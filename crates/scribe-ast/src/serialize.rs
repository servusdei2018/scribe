//! Deterministic serialization: JSON round-trips and Docutils-style pseudo-XML.
//!
//! # Pseudo-XML format (oracle fidelity)
//!
//! `sphinx-build -b pseudoxml` ends in `document.pformat()` from Docutils. That
//! writer emits, for every element node, an indented **start tag only** (no end
//! tags); hierarchy is conveyed purely by 4-space indentation. Text nodes are
//! emitted as their text content, one line per `str.splitlines()` result, with
//! the same indentation. Attributes are sorted alphabetically and values are
//! formatted exactly like `Element.starttag()`:
//!
//! * strings → `"value"` (no entity escaping in pseudo-XML),
//! * booleans → `"1"` / `"0"`, `None` → `"True"`,
//! * lists → members joined by a space after `serial_escape` (backslash →
//!   `\\`, space → `\ `),
//! * empty `ids` / `classes` / `names` / `dupnames` lists are omitted.
//!
//! Scribe-internal node-reference attributes ([`AttributeValue::NodeId`] /
//! [`AttributeValue::NodeIds`]) are never emitted — they are arena bookkeeping,
//! not doctree data.

use std::fmt::Write as _;

use crate::error::AstResult;
use crate::node::{AttributeValue, Node, NodeKind};
use crate::{Document, NodeId};

const INDENT: &str = "    ";

impl Document {
    /// Serializes the document to pretty-printed JSON via serde.
    pub fn to_json(&self) -> AstResult<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Deserializes a document produced by [`Document::to_json`].
    pub fn from_json(json: &str) -> AstResult<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Renders Docutils/Sphinx-style pseudo-XML for the whole document.
    pub fn to_pseudoxml(&self) -> AstResult<String> {
        let mut out = String::new();
        emit_pseudoxml(self, self.root, 0, &mut out)?;
        Ok(out)
    }

    /// Renders deterministic XML with explicit start/end tags and entity
    /// escaping. (Optional format; pseudo-XML is the differential surface.)
    pub fn to_xml(&self) -> AstResult<String> {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        emit_xml(self, self.root, 0, &mut out)?;
        out.push('\n');
        Ok(out)
    }
}

fn emit_pseudoxml(doc: &Document, id: NodeId, level: usize, out: &mut String) -> AstResult<()> {
    let node = doc.get(id)?;
    let indent = INDENT.repeat(level);

    if node.kind == NodeKind::Text {
        let text = node.text_content().unwrap_or("");
        // Docutils uses str.splitlines(); Rust's lines() matches it for the
        // newline-separated text our parsers produce (no trailing empty line).
        for line in text.lines() {
            out.push_str(&indent);
            out.push_str(line);
            out.push('\n');
        }
        return Ok(());
    }

    out.push_str(&indent);
    out.push('<');
    out.push_str(node.kind.tag_name());
    emit_pseudo_attrs(node, out);
    out.push_str(">\n");

    for &child in &node.children {
        if doc.is_live(child) {
            emit_pseudoxml(doc, child, level + 1, out)?;
        }
    }
    Ok(())
}

fn emit_pseudo_attrs(node: &Node, out: &mut String) {
    // Attributes is a BTreeMap, so iteration is already sorted — matching
    // Docutils' sorted `attlist()`.
    for (key, value) in node.attrs.iter() {
        if is_default_list(key, value) || is_internal(value) {
            continue;
        }
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        write_pseudo_value(value, out);
        out.push('"');
    }
}

fn is_default_list(key: &str, value: &AttributeValue) -> bool {
    matches!(value, AttributeValue::List(v) if v.is_empty())
        && matches!(key, "ids" | "classes" | "names" | "dupnames")
}

fn is_internal(value: &AttributeValue) -> bool {
    matches!(value, AttributeValue::NodeId(_) | AttributeValue::NodeIds(_))
}

fn write_pseudo_value(value: &AttributeValue, out: &mut String) {
    match value {
        AttributeValue::Str(s) => out.push_str(s),
        AttributeValue::Bool(b) => out.push_str(if *b { "1" } else { "0" }),
        AttributeValue::Int(i) => {
            let _ = write!(out, "{i}");
        }
        AttributeValue::Float(f) => {
            let _ = write!(out, "{f}");
        }
        AttributeValue::List(values) => {
            let joined = values
                .iter()
                .map(|v| serial_escape(v))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&joined);
        }
        AttributeValue::NodeId(_) | AttributeValue::NodeIds(_) => {}
        AttributeValue::Null => out.push_str("True"),
    }
}

/// Docutils `serial_escape`: double backslashes and escape internal spaces.
fn serial_escape(value: &str) -> String {
    value.replace('\\', r"\\").replace(' ', r"\ ")
}

fn emit_xml(doc: &Document, id: NodeId, level: usize, out: &mut String) -> AstResult<()> {
    let node = doc.get(id)?;
    let indent = INDENT.repeat(level);

    if node.kind == NodeKind::Text {
        let text = node.text_content().unwrap_or("");
        out.push_str(&indent);
        out.push_str(&xml_escape(text));
        out.push('\n');
        return Ok(());
    }

    out.push_str(&indent);
    out.push('<');
    out.push_str(node.kind.tag_name());
    for (key, value) in node.attrs.iter() {
        if is_default_list(key, value) || is_internal(value) {
            continue;
        }
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        out.push_str(&xml_escape(&pseudo_value_to_string(value)));
        out.push('"');
    }
    if node.children.iter().all(|c| !doc.is_live(*c)) {
        out.push_str("/>\n");
        return Ok(());
    }
    out.push_str(">\n");
    for &child in &node.children {
        if doc.is_live(child) {
            emit_xml(doc, child, level + 1, out)?;
        }
    }
    out.push_str(&indent);
    out.push_str("</");
    out.push_str(node.kind.tag_name());
    out.push_str(">\n");
    Ok(())
}

fn pseudo_value_to_string(value: &AttributeValue) -> String {
    let mut s = String::new();
    write_pseudo_value(value, &mut s);
    s
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Attributes;

    #[test]
    fn json_round_trip_is_exact() {
        let doc = Document::fixture_document();
        let json = doc.to_json().unwrap();
        let back = Document::from_json(&json).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.to_pseudoxml().unwrap(), doc.to_pseudoxml().unwrap());
    }

    #[test]
    fn pseudo_value_formatting_matches_docutils() {
        let mut node = Node::new(NodeKind::Target);
        node.attrs = Attributes::from_iter([
            ("flag".into(), AttributeValue::Bool(true)),
            ("off".into(), AttributeValue::Bool(false)),
            ("none".into(), AttributeValue::Null),
            ("n".into(), AttributeValue::Int(3)),
            ("ids".into(), AttributeValue::List(vec![])),
            ("names".into(), AttributeValue::List(vec!["two words".into(), "a\\b".into()])),
        ]);
        let mut out = String::new();
        emit_pseudo_attrs(&node, &mut out);
        assert_eq!(
            out,
            " flag=\"1\" n=\"3\" names=\"two\\ words a\\\\b\" none=\"True\" off=\"0\""
        );
    }

    #[test]
    fn xml_is_deterministic_and_escaped() {
        let mut doc = Document::new(Some("a&b.rst".into()));
        doc.new_text("a < b & c", Some(doc.root)).unwrap();
        let xml1 = doc.to_xml().unwrap();
        assert_eq!(xml1, doc.to_xml().unwrap());
        assert!(xml1.contains("source=\"a&amp;b.rst\""));
        assert!(xml1.contains("a &lt; b &amp; c"));
    }
}
