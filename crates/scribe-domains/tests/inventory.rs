//! Inventory reader/writer tests, including byte-exact oracle comparison.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use scribe_domains::{DumpConfig, Inventory, ObjectEntry};
use scribe_domains::intersphinx::inventory::posixpath_join;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hex_decode(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex byte"))
        .collect()
}

fn inventory_v2(header_version: &str, body_hex: &str) -> Vec<u8> {
    let mut bytes = format!(
        "# Sphinx inventory version 2\n# Project: foo\n# Version: {header_version}\n\
         # The remainder of this file is compressed with zlib.\n"
    )
    .into_bytes();
    bytes.extend_from_slice(&hex_decode(body_hex));
    bytes
}

const V2_BODY: &str = "789c8d924f4fc3300cc5eff91496c6d51de5847204b403821bf72a4dd22d531a972605c6a7474ed6f247dbc4a9a9dfefbd38967b3293b7350c07d9e7235c434754ed52ef57a582fd9179a2b085e742191bb528c2cd45f315e091abab6e0a9a61fe2647016a8853bb5e2c3fd90d51d5aa3167dbb4230335b860ec4741d973a7c6aa559f80e27ec3c9fa3bf806349f0b5b54143119d0c320d361b0bfd358c9ba94930be9b649e7c1662650744452dea931c3daab187fd37a189a8ea8e1777cd35272d36cf93187d3aea63caf230af14c47453b321c3d66e602dccc0c0a05c98e3df0c3f3016bd87a8a518d8762e02a2a2ca2f0b1429f69dd1b1a4eb43e97d147f480c2900eaab7d96348f305c7523694be611fe7fdf9db6d01f222ece3a911cfeac2e58de0c0334bd32e4b33ffbd4e1f6c302aa9d33803cbac5cd07e322e6ca5264fe17fb3c3c585c585e26567f171f24ee103f5ca9520af5aeb39e97d74c9362ebcd990683cac5757f0b2b3900d500ce20b59a74af8";

const AMBIGUOUS_BODY: &str = "789c4b5428492dca55282e49b10233740d15d273f28b8b138b2af5324a72739441a2ba89ba10492e47d29427612a4fc94f2ecd4dcd2b8128cf4c3155d0e57222a4cc4941970b0083943947";

#[test]
fn reads_v2_inventory_like_upstream() {
    let bytes = inventory_v2("2.0", V2_BODY);
    let inventory = Inventory::loads(&bytes, "/util").expect("parse v2");
    assert_eq!(inventory.project_name, "foo");
    assert_eq!(inventory.project_version, "2.0");
    assert_eq!(inventory.get("py:module", "module1").unwrap().uri, "/util/foo.html#module-module1");
    assert_eq!(inventory.get("py:module", "module1").unwrap().display_name, "Long Module desc");
    // `$` suffix expands to the object name, then joins to the base URI.
    assert_eq!(inventory.get("py:module", "module2").unwrap().uri, "/util/foo.html#module-module2");
    assert_eq!(inventory.get("py:function", "module1.func").unwrap().uri, "/util/sub/foo.html#module1.func");
    assert_eq!(inventory.get("c:function", "CFunc").unwrap().uri, "/util/cfunc.html#CFunc");
    assert_eq!(inventory.get("std:term", "a term").unwrap().uri, "/util/glossary.html#term-a-term");
    assert_eq!(
        inventory.get("std:term", "a term including:colon").unwrap().uri,
        "/util/glossary.html#term-a-term-including-colon"
    );
    assert_eq!(inventory.get("std:label", "The-Julia-Domain").unwrap().uri, "/util/write_inventory/#The-Julia-Domain");
}

#[test]
fn case_insensitive_lookup_and_ambiguity() {
    let bytes = inventory_v2("2.0", AMBIGUOUS_BODY);
    let inventory = Inventory::loads(&bytes, "/util").expect("parse ambiguous terms");
    // `a` / `A` point to the same URI: duplicate, not a real ambiguity.
    // No exact `a TeRm` key, so the case-insensitive fallback finds the
    // first sorted key (`A term` sorts before `a term`).
    assert_eq!(
        inventory.lookup("std:term", "a TeRm").map(|(name, _)| name),
        Some("A term")
    );
    // `b term` / `B term` point to different URIs: genuine ambiguity.
    assert!(inventory
        .ambiguities
        .iter()
        .any(|entry| entry.to_lowercase().contains("std:term:b")));
    // Non-std types are case-sensitive.
    let bytes = inventory_v2("2.0", V2_BODY);
    let normal = Inventory::loads(&bytes, "/util").expect("parse");
    assert!(normal.get("py:module", "MODULE1").is_none());
}

#[test]
fn reads_v1_gracefully() {
    let bytes = concat!(
        "# Sphinx inventory version 1\n# Project: foo\n# Version: 1.0\n",
        "module mod foo.html\nmodule.cls class foo.html\n",
    );
    let inventory = Inventory::loads(bytes.as_bytes(), "/util").expect("parse v1");
    assert_eq!(inventory.project_name, "foo");
    assert_eq!(inventory.get("py:module", "module").unwrap().uri, "/util/foo.html#module-module");
    assert_eq!(inventory.get("py:class", "module.cls").unwrap().uri, "/util/foo.html#module.cls");
}

#[test]
fn invalid_inputs_are_errors_not_panics() {
    assert!(Inventory::loads(b"garbage\n", "/util").is_err());
    assert!(Inventory::loads(b"# Sphinx inventory version 42\nx\n", "/util").is_err());
    // Valid header but missing body lines.
    assert!(Inventory::loads(b"# Sphinx inventory version 2\n# Project: foo\n", "/util").is_err());
    // Header says "not compressed".
    assert!(Inventory::loads(
        b"# Sphinx inventory version 2\n# Project: foo\n# Version: 1\n# plain\nbody",
        "/util",
    ).is_err());
    // Corrupt compressed stream.
    let mut bytes = inventory_v2("1", V2_BODY);
    let len = bytes.len();
    bytes[len - 5] ^= 0xff;
    assert!(Inventory::loads(&bytes, "/util").is_err());
}

fn oracle_objects() -> BTreeMap<String, Vec<ObjectEntry>> {
    let mut grouped = BTreeMap::new();
    grouped.insert("py".to_owned(), vec![
        ObjectEntry::new("module.func", "module.func", "function", "api", "module.func", 1),
        ObjectEntry::new("Class", "Custom Display", "class", "api", "Class", 1),
    ]);
    grouped.insert("c".to_owned(), vec![
        ObjectEntry::new("CFunc", "CFunc", "function", "c-api", "c.CFunc", 1),
    ]);
    grouped.insert("std".to_owned(), vec![
        ObjectEntry::new("label-one", "label-one", "label", "index", "label-one", -1),
        ObjectEntry::new("a term", "a term", "term", "glossary", "term-a-term", -1),
        ObjectEntry::new("index", "Hello", "doc", "index", "", -1),
    ]);
    grouped
}

#[test]
fn dump_is_byte_exact_against_oracle_zlib() {
    let root = workspace_root();
    let output = root.join("target").join(format!("oracle-inventory-{}.inv", std::process::id()));
    let script = root.join("crates/scribe-domains/tests/fixtures/gen_inventory.py");
    let python = root.join(".oracle-venv/bin/python");
    let status = Command::new(python)
        .arg(script)
        .arg(&output)
        .status()
        .expect("run oracle inventory generator");
    assert!(status.success(), "oracle generator failed");
    let expected = std::fs::read(&output).expect("read oracle bytes");

    let actual = Inventory::dump(
        &oracle_objects(),
        &DumpConfig { project: "My  Project", version: "v1.0\nbeta" },
        |docname| format!("{docname}.html"),
    );
    assert_eq!(
        actual, expected,
        "inventory bytes differ from oracle ({} vs {} bytes)",
        actual.len(), expected.len(),
    );
    let _ = std::fs::remove_file(output);
}

#[test]
fn dump_roundtrips_through_reader() {
    let bytes = Inventory::dump(
        &oracle_objects(),
        &DumpConfig { project: "Project", version: "1.0" },
        |docname| format!("{docname}.html"),
    );
    let inventory = Inventory::loads(&bytes, "https://example.com/docs/").expect("parse our dump");
    assert_eq!(inventory.project_name, "Project");
    assert_eq!(inventory.get("py:function", "module.func").unwrap().uri, "https://example.com/docs/api.html#module.func");
    assert_eq!(inventory.get("std:term", "a term").unwrap().uri, "https://example.com/docs/glossary.html#term-a-term");
    assert_eq!(inventory.len(), 6);
    assert_eq!(posixpath_join("https://example.com/docs", "api.html"), "https://example.com/docs/api.html");
}
