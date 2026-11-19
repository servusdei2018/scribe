//! Source discovery integration tests against real Sphinx fixtures.

mod common;

use common::Fixture;
use scribe_core::{
    discover_sources, match_any, path2doc, DiscoveryOptions, Matcher, EXCLUDE_PATHS,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn test_basic_fixture_is_discovered() {
    let root = workspace().join("sphinx/tests/roots/test-basic");
    let options = DiscoveryOptions::from_config(&common::default_config(), &root);
    let result = discover_sources(&options).expect("discover test-basic");
    assert_eq!(
        result.docnames().collect::<Vec<_>>(),
        vec!["index"],
        "test-basic must contain exactly the index document"
    );
    let index = &result.docs["index"];
    assert_eq!(index.relative_path, "index.rst");
    assert_eq!(index.source_suffix, ".rst");
    assert_eq!(index.parser, "restructuredtext");
    assert!(index.absolute_path.is_file());
    assert!(!index.digest.is_zero());
}

#[test]
fn toctree_fixture_discovers_all_documents_in_sorted_order() {
    let root = workspace().join("sphinx/tests/roots/test-toctree");
    let options = DiscoveryOptions::from_config(&common::default_config(), &root);
    let result = discover_sources(&options).expect("discover test-toctree");
    let names: Vec<&str> = result.docnames().collect();
    assert_eq!(
        names,
        vec!["bar", "baz", "foo", "index", "quux", "qux", "tocdepth"],
        "discovery must be deterministic and suffix-free"
    );
    assert!(result.docs.values().all(|doc| doc.source_suffix == ".rst"));
}

#[test]
fn test_root_fixture_honours_custom_suffixes_and_excludes() {
    let root = workspace().join("sphinx/tests/roots/test-root");
    let config = common::load_conf(&root);
    let options = DiscoveryOptions::from_config(&config, &root);
    let result = discover_sources(&options).expect("discover test-root");

    assert!(result.docs.contains_key("index"));
    assert!(result.docs.contains_key("subdir/includes"));
    assert!(result.docs.contains_key("otherext"), "`.foo` suffix must be recognized");
    assert!(!result.docs.contains_key("subdir/excluded"), "exclude_patterns must apply");
    assert!(
        !result.docs.keys().any(|name| name.starts_with("_templates")),
        "templates_path must be excluded from discovery"
    );
    assert!(
        !result.docs.keys().any(|name| name.ends_with(".inc")),
        "included files are not documents"
    );
    assert!(
        result.docs.get("otherext").map(|doc| doc.source_suffix.as_str()) == Some(".foo")
    );
    assert!(
        result.docs.get("index").map(|doc| doc.source_suffix.as_str()) == Some(".txt")
    );
}

#[test]
fn exclude_patterns_follow_sphinx_glob_semantics() {
    assert!(EXCLUDE_PATHS.contains(&"**/_sources"));
    // `*` does not cross a slash; `**` does.
    assert!(match_any("index.rst", &["*.rst".to_owned()]));
    assert!(!match_any("nested/index.rst", &["*.rst".to_owned()]));
    assert!(match_any("nested/index.rst", &["**/*.rst".to_owned()]));
    // `?` does not cross a slash.
    assert!(match_any("a.rst", &["?.rst".to_owned()]));
    assert!(!match_any("ab.rst", &["?.rst".to_owned()]));
    // Negated classes also exclude '/'.
    assert!(match_any("a.txt", &["[!b]*.txt".to_owned()]));
    assert!(!match_any("b.txt", &["[!b]*.txt".to_owned()]));
    assert!(!match_any("sub/a.txt", &["[!b]*.txt".to_owned()]));

    let matcher = Matcher::new(["**/private".to_owned()]);
    assert!(matcher.is_match("private"));
    assert!(matcher.is_match("nested/private"));
}

#[test]
fn exclude_directories_are_pruned_and_include_patterns_apply() {
    let fixture = Fixture::new("exclude");
    fixture.write("index.rst", "Index\n=====\n");
    fixture.write("keep.rst", "Keep\n====\n");
    fixture.write("secret/hidden.rst", "Hidden\n======\n");
    fixture.write("nested/also.rst", "Also\n====\n");

    let mut options = DiscoveryOptions::from_config(&common::default_config(), fixture.src());
    options.exclude_patterns = vec!["secret".to_owned()];
    options.include_patterns = vec!["**".to_owned()];
    let result = discover_sources(&options).expect("discover");
    assert!(!result.docs.contains_key("secret/hidden"), "excluded dir must be pruned");
    assert!(result.docs.contains_key("keep"));
    assert!(result.docs.contains_key("nested/also"));

    let mut only_root = options.clone();
    only_root.include_patterns = vec!["*.rst".to_owned()];
    let result = discover_sources(&only_root).expect("discover root only");
    assert!(result.docs.contains_key("index"));
    assert!(!result.docs.contains_key("nested/also"));
}

#[test]
fn path2doc_resolves_absolute_and_relative_paths() {
    let root = workspace().join("sphinx/tests/roots/test-basic");
    let options = DiscoveryOptions::from_config(&common::default_config(), &root);
    let result = discover_sources(&options).expect("discover");
    assert_eq!(path2doc(&result, &root.join("index.rst")), Some("index"));
    assert_eq!(path2doc(&result, Path::new("index.rst")), Some("index"));
    assert_eq!(path2doc(&result, Path::new("missing.rst")), None);
}

#[test]
fn environment_exposes_glob_spec_and_predicate() {
    use scribe_core::BuildEnvironment;

    let fixture = Fixture::new("glob-spec");
    let mut config = common::default_config();
    config
        .set_explicit(
            "exclude_patterns",
            scribe_config::ConfigValue::List(vec![scribe_config::ConfigValue::Str(
                "private.rst".to_owned(),
            )]),
        )
        .unwrap();
    config
        .set_explicit(
            "templates_path",
            scribe_config::ConfigValue::List(vec![scribe_config::ConfigValue::Str(
                "_templates".to_owned(),
            )]),
        )
        .unwrap();
    let mut environment = BuildEnvironment::with_config(fixture.src(), &config);
    let spec = environment.glob_to_spec();
    assert!(spec.exclude.contains(&"private.rst".to_owned()));
    assert!(spec.exclude.contains(&"_templates".to_owned()));
    assert!(spec.exclude.contains(&"**/_sources".to_owned()));
    assert!(environment.glob("private.rst"));
    assert!(environment.glob("_sources"));
    assert!(!environment.glob("index.rst"));

    environment.set_parser_version("v2");
    assert_eq!(environment.parser_version(), "v2");
}

#[test]
fn custom_source_suffixes_are_mapped_to_docnames() {
    let fixture = Fixture::new("suffix");
    fixture.write("index.md", "# Index\n");
    fixture.write("api.foo", "API\n===\n");
    let options = DiscoveryOptions {
        source_dir: fixture.src().to_path_buf(),
        source_suffix: BTreeMap::from([
            (".md".to_owned(), "myst".to_owned()),
            (".foo".to_owned(), "custom".to_owned()),
        ]),
        ..Default::default()
    };
    let result = discover_sources(&options).expect("discover custom suffixes");
    assert_eq!(result.docs["index"].parser, "myst");
    assert_eq!(result.docs["api"].parser, "custom");
}
