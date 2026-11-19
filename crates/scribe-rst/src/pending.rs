//! The `NodeKind::Pending` attribute contract consumed by Phase 06.
//!
//! Phase 04 does **not** interpret directives. For every `.. name:: ...`
//! explicit markup construct it emits a `pending` node carrying the raw,
//! structured pieces of the construct. Phase 06 resolves these into concrete
//! Sphinx/Docutils nodes. The key names below are a **frozen contract** between
//! the two phases.
//!
//! | Key | `AttributeValue` | Meaning |
//! |---|---|---|
//! | `directive_name` | `Str` | lower-cased directive name (`"note"`, `"code"`, ...) |
//! | `arguments` | `Str` | raw argument string (whitespace not normalized) |
//! | `options` | `Str` | JSON object mapping option name → option value string |
//! | `content` | `Str` | raw, dedented directive content (may be empty) |
//! | `directive_line` | `Int` | 1-based source line of the `.. name::` marker |
//!
//! `options` is JSON because the frozen AST's [`AttributeValue`] has no map
//! variant. [`PendingDirective::options()`] decodes it back into a
//! `BTreeMap<String, String>` for Phase 06.

use std::collections::BTreeMap;

use scribe_ast::{AstResult, AttributeValue, Node, NodeId};

/// Attribute key holding the directive name.
pub const KEY_DIRECTIVE_NAME: &str = "directive_name";
/// Attribute key holding the raw argument string.
pub const KEY_ARGUMENTS: &str = "arguments";
/// Attribute key holding the JSON-encoded option map.
pub const KEY_OPTIONS: &str = "options";
/// Attribute key holding the raw content string.
pub const KEY_CONTENT: &str = "content";
/// Attribute key holding the directive's 1-based source line.
pub const KEY_DIRECTIVE_LINE: &str = "directive_line";
/// Additive key holding the option-free body lines before argument promotion.
///
/// Phase 04 cannot know a directive's arity, so it promotes the first dedented
/// body line to `arguments` when there is no blank after the marker. Phase 06
/// restores those lines for zero-argument directives (e.g. `toctree`).
pub const KEY_BODY_LINES: &str = "body_lines";

/// Additive key on a `pending` (and, after resolution, `only`) node recording
/// how many ancestor sections Sphinx's `Only.run` raises its content through.
/// Computed by the block parser from the surrounding title styles.
pub const KEY_ONLY_RAISE: &str = "only_raise";

/// Typed, read-only view over a `pending` node's directive attributes.
#[derive(Clone, Copy, Debug)]
pub struct PendingDirective<'a> {
    node: &'a Node,
}

impl<'a> PendingDirective<'a> {
    /// Returns a view if `node` is a `pending` node with directive attributes.
    pub fn new(node: &'a Node) -> Option<Self> {
        if node.attrs.contains_key(KEY_DIRECTIVE_NAME) {
            Some(Self { node })
        } else {
            None
        }
    }

    /// The lower-cased directive name.
    pub fn name(&self) -> AstResult<Option<&str>> {
        self.node.get_str(KEY_DIRECTIVE_NAME)
    }

    /// The raw argument string.
    pub fn arguments(&self) -> AstResult<Option<&str>> {
        self.node.get_str(KEY_ARGUMENTS)
    }

    /// The raw content string.
    pub fn content(&self) -> AstResult<Option<&str>> {
        self.node.get_str(KEY_CONTENT)
    }

    /// The 1-based source line of the directive marker.
    pub fn line(&self) -> AstResult<Option<i64>> {
        self.node.attrs.get_int(KEY_DIRECTIVE_LINE)
    }

    /// The original option-free body lines, when Phase 04 recorded them.
    pub fn body_lines(&self) -> Vec<String> {
        let Some(AttributeValue::Str(raw)) = self.node.attrs.get(KEY_BODY_LINES) else {
            return Vec::new();
        };
        serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
    }

    /// Decodes the JSON `options` attribute into a map.
    ///
    /// Missing or malformed JSON yields an empty map: Phase 04 never invents
    /// option values, it only preserves what was on the source lines.
    pub fn options(&self) -> BTreeMap<String, String> {
        let raw = self.node.attrs.get(KEY_OPTIONS).and_then(|v| match v {
            AttributeValue::Str(s) => Some(s.as_str()),
            _ => None,
        });
        let Some(raw) = raw else {
            return BTreeMap::new();
        };
        serde_json::from_str::<BTreeMap<String, String>>(raw).unwrap_or_default()
    }
}

/// Encodes an option map into the JSON string stored on `pending` nodes.
pub fn encode_options(options: &BTreeMap<String, String>) -> String {
    serde_json::to_string(options).unwrap_or_else(|_| "{}".to_owned())
}

/// Sets all directive attributes on a freshly created `pending` node.
pub fn fill_pending(
    node: &mut Node,
    name: &str,
    arguments: &str,
    options: &BTreeMap<String, String>,
    content: &str,
    line: u32,
) {
    node.attrs.set_str(KEY_DIRECTIVE_NAME, name);
    node.attrs.set_str(KEY_ARGUMENTS, arguments);
    node.attrs.set_str(KEY_OPTIONS, encode_options(options));
    node.attrs.set_str(KEY_CONTENT, content);
    node.attrs.set_int(KEY_DIRECTIVE_LINE, i64::from(line));
}

/// Like [`fill_pending`] but also preserves the raw body lines before argument
/// promotion, so zero-argument directives can recover multi-line content.
pub fn fill_pending_with_body(
    node: &mut Node,
    name: &str,
    arguments: &str,
    options: &BTreeMap<String, String>,
    content: &str,
    line: u32,
    body_lines: &[String],
) {
    fill_pending(node, name, arguments, options, content, line);
    node.attrs.set_str(
        KEY_BODY_LINES,
        serde_json::to_string(body_lines).unwrap_or_else(|_| "[]".to_owned()),
    );
}

/// Returns a typed view of `pending` directive attributes on `node`.
pub fn as_pending_directive(node: &Node) -> Option<PendingDirective<'_>> {
    PendingDirective::new(node)
}

/// Returns `true` when `node` is a directive-carrying `pending` node.
pub fn is_directive_pending(node: &Node) -> bool {
    node.attrs.contains_key(KEY_DIRECTIVE_NAME)
}

/// Convenience: extracts the directive name from a `pending` node id.
pub fn directive_name(doc: &scribe_ast::Document, id: NodeId) -> Option<String> {
    doc.get(id)
        .ok()
        .and_then(PendingDirective::new)
        .and_then(|view| view.name().ok().flatten().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_ast::NodeKind;

    fn pending() -> Node {
        let mut node = Node::new(NodeKind::Pending);
        let options = BTreeMap::from([
            ("class".to_owned(), "foo".to_owned()),
            ("name".to_owned(), "bar".to_owned()),
        ]);
        fill_pending(&mut node, "note", "an arg", &options, "body\n", 7);
        node
    }

    #[test]
    fn pending_contract_round_trips() {
        let node = pending();
        let view = PendingDirective::new(&node).expect("pending directive");
        assert_eq!(view.name().unwrap(), Some("note"));
        assert_eq!(view.arguments().unwrap(), Some("an arg"));
        assert_eq!(view.content().unwrap(), Some("body\n"));
        assert_eq!(view.line().unwrap(), Some(7));
        assert_eq!(view.options().get("class").map(String::as_str), Some("foo"));
        assert!(is_directive_pending(&node));
    }

    #[test]
    fn malformed_options_are_empty() {
        let mut node = Node::new(NodeKind::Pending);
        node.attrs.set_str(KEY_DIRECTIVE_NAME, "x");
        node.attrs.set_str(KEY_OPTIONS, "not json");
        let view = PendingDirective::new(&node).expect("pending directive");
        assert!(view.options().is_empty());
    }
}
