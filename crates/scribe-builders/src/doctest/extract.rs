//! Extraction and grouping of doctest examples (Phase 22).
//!
//! Upstream `DocTestBuilder.test_doc()` walks the doctree for `literal_block`
//! / `comment` nodes carrying a `testnodetype` attribute (emitted by the
//! five test directives) and, optionally, plain `doctest_block` nodes. This
//! module reproduces that walk over the frozen §7 AST and mirrors
//! `TestGroup.add_code()` including the special `*` "all groups" marker.

use std::collections::BTreeMap;

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::Config;
use serde::{Deserialize, Serialize};


/// One executable snippet or expected-output block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCode {
    /// Python source / expected output text.
    pub code: String,
    /// Upstream `testnodetype`: `doctest`, `testsetup`, `testcleanup`,
    /// `testcode` or `testoutput`.
    pub kind: String,
    /// Source file name reported in failures.
    pub filename: String,
    /// 1-based source line.
    pub lineno: u32,
    /// `doctest` option flag overrides (bit → enabled).
    pub options: BTreeMap<u32, bool>,
    /// Optional `:skipif:` expression.
    pub skipif: Option<String>,
    /// Optional `:pyversion:` specifier.
    pub pyversion: Option<String>,
    /// Groups this snippet belongs to (`*` means every group).
    pub groups: Vec<String>,
}

/// A named group of setup / tests / cleanup snippets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestGroup {
    /// Group name.
    pub name: String,
    /// `testsetup` snippets, in document order.
    pub setup: Vec<TestCode>,
    /// Tests: one element for `doctest`, two for `testcode` + `testoutput`.
    pub tests: Vec<Vec<TestCode>>,
    /// `testcleanup` snippets, in document order.
    pub cleanup: Vec<TestCode>,
}

/// Group name used for free-standing `doctest_block` nodes, read from
/// `doctest_test_doctest_blocks` (upstream default `"default"`).
#[must_use]
pub fn doctest_block_group(config: &Config) -> Option<String> {
    if let Some(value) = config.get_bool("doctest_test_doctest_blocks") {
        return value.then(|| "default".to_owned());
    }
    config
        .get_str("doctest_test_doctest_blocks")
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Walks `doc` in document order and collects every test snippet.
#[must_use]
pub fn extract_codes(doc: &Document, filename: &str, config: &Config) -> Vec<TestCode> {
    let mut order = Vec::new();
    walk(doc, NodeId::ROOT, &mut order);
    let default_group = doctest_block_group(config).unwrap_or_else(|| "default".to_owned());
    let mut codes = Vec::new();
    for id in order {
        let Ok(node) = doc.get(id) else { continue };
        let kind = node.attrs.get_str("testnodetype").ok().flatten().map(str::to_owned);
        let (kind, code, groups, options, skipif, pyversion) = if node.kind == NodeKind::DoctestBlock {
            let Some(default) = doctest_block_group(config) else { continue };
            (Some("doctest".to_owned()), node.astext(doc), vec![default], BTreeMap::new(), None, None)
        } else if let Some(kind) = kind {
            let test = node.attrs.get_str("test").ok().flatten().map(str::to_owned);
            let code = test.unwrap_or_else(|| node.astext(doc));
            let groups = node
                .attrs
                .get_list("groups")
                .ok()
                .flatten()
                .filter(|list| !list.is_empty())
                .unwrap_or_else(|| vec![default_group.clone()]);
            let options = node
                .attrs
                .get_str("options")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str::<BTreeMap<u32, bool>>(raw).ok())
                .unwrap_or_default();
            let skipif = node.attrs.get_str("skipif").ok().flatten().map(str::to_owned);
            let pyversion = node.attrs.get_str("pyversion").ok().flatten().map(str::to_owned);
            (Some(kind), code, groups, options, skipif, pyversion)
        } else {
            continue;
        };
        let Some(kind) = kind else { continue };
        // Upstream `get_filename_for_node` reports the source *file* relative
        // to srcdir (with its suffix), and `get_line_number` subtracts one
        // from docutils' 1-based marker line. Match both for byte-equal diffs.
        let lineno = node.source.as_ref().and_then(|source| source.line).unwrap_or(0).saturating_sub(1);
        codes.push(TestCode {
            code,
            kind,
            filename: filename.to_owned(),
            lineno,
            options,
            skipif,
            pyversion,
            groups,
        });
    }
    codes
}

fn walk(doc: &Document, id: NodeId, order: &mut Vec<NodeId>) {
    if !doc.is_live(id) {
        return;
    }
    order.push(id);
    if let Ok(children) = doc.children(id) {
        for child in children.iter().copied() {
            walk(doc, child, order);
        }
    }
}

/// Groups snippets exactly as upstream `TestGroup.add_code` does.
#[must_use]
pub fn group_codes(codes: Vec<TestCode>) -> BTreeMap<String, TestGroup> {
    let mut groups: BTreeMap<String, TestGroup> = BTreeMap::new();
    let mut all_group_codes = Vec::new();
    for code in codes {
        if code.groups.iter().any(|name| name == "*") {
            all_group_codes.push(code);
            continue;
        }
        for name in &code.groups {
            let group = groups
                .entry(name.clone())
                .or_insert_with(|| TestGroup { name: name.clone(), ..TestGroup::default() });
            add_code(group, &code);
        }
    }
    for code in all_group_codes {
        for group in groups.values_mut() {
            add_code(group, &code);
        }
    }
    groups
}

fn add_code(group: &mut TestGroup, code: &TestCode) {
    match code.kind.as_str() {
        "testsetup" => group.setup.push(code.clone()),
        "testcleanup" => group.cleanup.push(code.clone()),
        "doctest" => group.tests.push(vec![code.clone()]),
        "testcode" => group.tests.push(vec![code.clone()]),
        "testoutput" => {
            if let Some(latest) = group.tests.last_mut() {
                if latest.len() == 1 && latest[0].kind == "testcode" {
                    latest.push(code.clone());
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(kind: &str, group: &str) -> TestCode {
        TestCode {
            code: String::new(),
            kind: kind.to_owned(),
            filename: "index.rst".to_owned(),
            lineno: 1,
            options: BTreeMap::new(),
            skipif: None,
            pyversion: None,
            groups: vec![group.to_owned()],
        }
    }

    #[test]
    fn grouping_pairs_testcode_with_testoutput() {
        let groups = group_codes(vec![
            code("testsetup", "g1"),
            code("testcode", "g1"),
            code("testoutput", "g1"),
            code("testcleanup", "g1"),
        ]);
        let group = &groups["g1"];
        assert_eq!(group.setup.len(), 1);
        assert_eq!(group.tests.len(), 1);
        assert_eq!(group.tests[0].len(), 2);
        assert_eq!(group.cleanup.len(), 1);
    }

    #[test]
    fn star_group_is_added_to_every_group() {
        let groups = group_codes(vec![code("testsetup", "default"), code("testsetup", "*"), code("doctest", "g2")]);
        assert_eq!(groups["default"].setup.len(), 2);
        assert_eq!(groups["g2"].setup.len(), 1);
    }
}
