//! Incremental rebuild decisions, mirroring
//! `sphinx.environment.BuildEnvironment.get_outdated_files()`.

mod common;

use common::{default_config, Fixture};
use scribe_core::{
    BuildEnvironment, DocumentSnapshot, UpdateOptions, UpdateReport, ENV_CACHE_KEY,
};
use scribe_config::{Config, ConfigValue};
use std::collections::BTreeSet;

fn options(fixture: &Fixture) -> UpdateOptions {
    UpdateOptions {
        cache_dir: Some(fixture.cache().to_path_buf()),
        persist: true,
        ..UpdateOptions::default()
    }
}

fn update(
    environment: &mut BuildEnvironment,
    config: &Config,
    fixture: &Fixture,
    options: &UpdateOptions,
) -> UpdateReport {
    environment
        .update(config, fixture.src(), options)
        .expect("environment update")
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn unchanged_files_are_skipped_on_the_second_build() {
    let fixture = Fixture::new("nochange");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("other.rst", "Other\n=====\n");

    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    let first = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(first.added, set(&["index", "other"]));
    assert!(first.changed.is_empty());
    assert_eq!(first.removed.len(), 0);

    let second = update(&mut environment, &config, &fixture, &options(&fixture));
    assert!(second.is_empty(), "no change must yield an empty rebuild set");
    assert!(second.files_to_rebuild().is_empty());
    assert_eq!(second.counts(), (0, 0, 0));
}

#[test]
fn editing_one_source_rebuilds_only_that_document() {
    let fixture = Fixture::new("edit-source");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("other.rst", "Other\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    fixture.append("index.rst", "\nA new paragraph.\n");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.changed, set(&["index"]));
    assert!(report.added.is_empty());
    assert_eq!(report.files_to_rebuild(), set(&["index"]));
    assert_eq!(report.docs_to_read(), vec!["index".to_owned()]);
}

#[test]
fn editing_an_included_file_rebuilds_its_includers_only() {
    let fixture = Fixture::new("edit-include");
    fixture.write("index.rst", ".. include:: common.inc\n");
    fixture.write("other.rst", "Other\n=====\n");
    fixture.write("common.inc", "Shared content\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    // Simulate the parser recording an include edge, then persist it.
    let common = fixture.src().join("common.inc");
    environment.note_included("index", &common);
    environment.note_dependency("index", &common);
    environment.save(fixture.cache()).expect("save env with edges");

    fixture.append("common.inc", "Changed!\n");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.changed, set(&["index"]));
    assert!(!report.changed.contains("other"));
    assert_eq!(environment.includers_of("common.inc"), set(&["index"]));
    assert_eq!(environment.docs_depending_on("common.inc"), set(&["index"]));
}

#[test]
fn relevant_config_changes_force_a_full_rebuild_but_irrelevant_ones_do_not() {
    let fixture = Fixture::new("config");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("other.rst", "Other\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    // An unrelated setting must not invalidate anything.
    let mut unrelated = config.clone();
    unrelated.set_explicit("html_theme", ConfigValue::Str("basic".to_owned())).unwrap();
    let report = update(&mut environment, &unrelated, &fixture, &options(&fixture));
    assert!(report.is_empty(), "html_theme is not an env config value");
    assert!(!report.config_changed);

    // `rst_prolog` is an env config value: every document is re-read.
    let mut changed = config.clone();
    changed
        .set_explicit("rst_prolog", ConfigValue::Str(":orphan:\n".to_owned()))
        .unwrap();
    let report = update(&mut environment, &changed, &fixture, &options(&fixture));
    assert!(report.config_changed);
    assert!(report.config_changed_reason.contains(&"rst_prolog".to_owned()));
    assert_eq!(report.added, set(&["index", "other"]));
    assert!(report.changed.is_empty(), "upstream puts everything in `added`");
}

#[test]
fn exclude_pattern_change_invalidates_discovery() {
    let fixture = Fixture::new("config-exclude");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("secret.rst", "Secret\n======\n");
    let mut config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(environment.found_docnames(), vec!["index", "secret"]);

    config
        .set_explicit(
            "exclude_patterns",
            ConfigValue::List(vec![ConfigValue::Str("secret.rst".to_owned())]),
        )
        .unwrap();
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert!(report.config_changed);
    assert_eq!(report.removed, set(&["secret"]));
    assert_eq!(environment.found_docnames(), vec!["index"]);
    assert!(!environment.all_docs.contains_key("secret"));
    assert!(!environment.documents.contains_key("secret"));
}

#[test]
fn adding_and_removing_files_are_reflected_in_discovery() {
    let fixture = Fixture::new("add-remove");
    fixture.write("index.rst", "Index\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    fixture.write("new.rst", "New\n===\n");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.added, set(&["new"]));
    assert!(report.removed.is_empty());
    assert_eq!(environment.found_docnames(), vec!["index", "new"]);

    fixture.remove("new.rst");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.removed, set(&["new"]));
    assert!(report.added.is_empty());
    assert!(report.changed.is_empty());
    assert_eq!(environment.found_docnames(), vec!["index"]);
}

#[test]
fn toctree_edges_invalidate_parent_documents() {
    let fixture = Fixture::new("toctree");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("child.rst", "Child\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    environment.note_toctree_edge("index", "child");
    environment.save(fixture.cache()).expect("save env");

    assert_eq!(environment.toctree_parents_of("child"), set(&["index"]));
    fixture.append("child.rst", "\nUpdated.\n");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.changed, set(&["child", "index"]));
    assert_eq!(environment.dependents_of("child"), set(&["index"]));
}

#[test]
fn reverse_dependency_closure_is_transitive_and_cycle_safe() {
    let fixture = Fixture::new("closure");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("middle.rst", "Middle\n======\n");
    fixture.write("leaf.rst", "Leaf\n====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    let middle = fixture.src().join("middle.rst");
    let leaf = fixture.src().join("leaf.rst");
    environment.note_included("index", &middle);
    environment.note_included("middle", &leaf);
    // A cycle must not hang the closure walk.
    environment.note_file_to_rebuild("leaf", "index");
    environment.save(fixture.cache()).expect("save env");

    fixture.append("leaf.rst", "\nTouched.\n");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert!(report.changed.contains("leaf"));
    assert!(report.changed.contains("middle"));
    assert!(report.changed.contains("index"));
    let closure = environment.dependent_closure(["leaf"]);
    assert_eq!(closure, set(&["leaf", "middle", "index"]));
}

#[test]
fn fresh_env_and_write_all_force_full_rebuilds() {
    let fixture = Fixture::new("flags");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("other.rst", "Other\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    let mut fresh = options(&fixture);
    fresh.freshenv = true;
    let report = update(&mut environment, &config, &fixture, &fresh);
    assert!(report.forced_full);
    assert_eq!(report.added, set(&["index", "other"]));

    let mut all = options(&fixture);
    all.write_all = true;
    let report = update(&mut environment, &config, &fixture, &all);
    assert!(report.forced_full);
    assert_eq!(report.files_to_rebuild(), set(&["index", "other"]));
}

#[test]
fn corrupt_cache_is_a_cold_rebuild_without_panicking() {
    let fixture = Fixture::new("corrupt");
    fixture.write("index.rst", "Index\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    let cache_file = fixture.cache().join(format!("cache-{ENV_CACHE_KEY}.bin"));
    std::fs::write(&cache_file, b"not a valid cache artifact").expect("corrupt cache");

    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.added, set(&["index"]), "corrupt cache must be cold");
    assert_eq!(environment.found_docnames(), vec!["index"]);
}

#[test]
fn save_load_round_trip_preserves_rebuild_decisions() {
    let fixture = Fixture::new("roundtrip");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("child.rst", "Child\n=====\n");
    let config = default_config();
    let mut original = BuildEnvironment::new(fixture.src());
    update(&mut original, &config, &fixture, &options(&fixture));
    original.note_toctree_edge("index", "child");
    original.note_reread("child");
    original.save(fixture.cache()).expect("save");

    let mut restored = BuildEnvironment::load(fixture.cache())
        .expect("load")
        .expect("environment present");
    assert_eq!(restored.found_docnames(), original.found_docnames());
    assert_eq!(restored.toctree_parents_of("child"), set(&["index"]));
    assert!(restored.reread_always.contains("child"));

    // Both environments make the same decision after the same source edit.
    fixture.append("child.rst", "\nChanged.\n");
    let expected = update(&mut original, &config, &fixture, &options(&fixture));
    let actual = update(&mut restored, &config, &fixture, &options(&fixture));
    assert_eq!(expected.files_to_rebuild(), actual.files_to_rebuild());
    assert_eq!(expected.files_to_rebuild(), set(&["child", "index"]));
}

#[test]
fn doctree_snapshots_round_trip_through_the_cache() {
    use scribe_ast::{Document, Node, NodeKind};

    let fixture = Fixture::new("doctree");
    fixture.write("index.rst", "Index\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));

    let mut document = Document::new(Some("index.rst".to_owned()));
    let section = document
        .add(NodeKind::Section, Some(scribe_ast::NodeId::ROOT))
        .unwrap();
    document
        .get_mut(section)
        .unwrap()
        .attrs
        .insert("ids", scribe_ast::AttributeValue::List(vec!["index".to_owned()]));
    let title = document.add(NodeKind::Title, Some(section)).unwrap();
    let text = document.add_node(Node::text("Index"), Some(title)).unwrap();
    let _ = text;
    environment.record_document("index", &document, Default::default());
    assert_eq!(environment.get_doctree("index"), Some(&document));
    let snapshot = environment.document("index").expect("snapshot");
    assert_eq!(snapshot.sections, vec!["index".to_owned()]);
    assert_eq!(environment.all_docs["index"].title.as_deref(), Some("Index"));

    environment.save(fixture.cache()).expect("save with doctree");
    let restored = BuildEnvironment::load(fixture.cache())
        .expect("load")
        .expect("environment");
    let loaded = restored.get_doctree("index").expect("restored doctree");
    assert_eq!(loaded.to_json().unwrap(), document.to_json().unwrap());

    // The whole snapshot type is serde-serializable and versioned.
    let serialized = serde_json::to_string(&original_snapshot(&restored)).expect("serialize");
    let _: DocumentSnapshot = serde_json::from_str(&serialized).expect("deserialize");
}

fn original_snapshot(environment: &BuildEnvironment) -> DocumentSnapshot {
    environment.document("index").expect("index snapshot").clone()
}

#[test]
fn extension_seam_adds_extra_outdated_documents() {
    let fixture = Fixture::new("extension-seam");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("other.rst", "Other\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    let mut with_hook = options(&fixture);
    with_hook.extra_outdated = vec!["other".to_owned()];
    let report = update(&mut environment, &config, &fixture, &with_hook);
    assert_eq!(report.changed, set(&["other"]), "extra_outdated is a seam for Phase 27");
    assert!(environment.extra_outdated.is_empty(), "the seam is drained each update");
}

#[test]
fn reread_always_docs_are_always_rebuilt() {
    let fixture = Fixture::new("reread");
    fixture.write("index.rst", "Index\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    environment.note_reread("index");
    environment.save(fixture.cache()).expect("save");
    let report = update(&mut environment, &config, &fixture, &options(&fixture));
    assert_eq!(report.changed, set(&["index"]));
}

#[test]
fn doctree_file_missing_forces_reread_when_doctree_dir_is_supplied() {
    let fixture = Fixture::new("doctree-dir");
    fixture.write("index.rst", "Index\n=====\n");
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    update(&mut environment, &config, &fixture, &options(&fixture));
    let mut with_dir = options(&fixture);
    with_dir.doctree_dir = Some(fixture.root().join("doctrees"));
    let report = update(&mut environment, &config, &fixture, &with_dir);
    assert_eq!(report.changed, set(&["index"]), "a missing doctree forces a read");
}
