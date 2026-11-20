//! Integration tests against real upstream `conf.py` fixtures.
//!
//! The static parser is exercised on pure-data fixtures; the subprocess bridge
//! is exercised on fixtures that import modules or mutate `tags`. Together
//! these cover well over the required ten roots.

use scribe_config::{
    load_config_with, BridgeStrategy, Config, ConfigWarningKind, LoadRequest,
};
use std::path::PathBuf;

fn roots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sphinx/tests/roots")
}

fn load(root: &str, strategy: BridgeStrategy) -> Config {
    let request = LoadRequest {
        conf_dir: roots_dir().join(root),
        strategy,
        ..LoadRequest::default()
    };
    let (config, _warnings) =
        load_config_with(request).unwrap_or_else(|error| panic!("failed to load {root}: {error}"));
    config
}

#[test]
fn static_fallback_loads_fourteen_real_roots() {
    // (root, expected project override, expected html_theme override)
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        ("test-basic", None, Some("basic")),
        ("test-config", Some("Sphinx <Tests>"), None),
        ("test-copyright-multiline", None, Some("basic")),
        ("test-directive-only", Some("test-directive-only"), None),
        ("test-ext-todo", None, None),
        ("test-ext-autodoc", None, None),
        ("test-theming", None, Some("test-theme")),
        (
            "test-build-html-theme-having-multiple-stylesheets",
            None,
            Some("mytheme"),
        ),
        ("test-domain-py", None, None),
        ("test-directive-code", None, None),
        ("test-ext-intersphinx-role", None, None),
        (
            "test-ext-extlinks-hardcoded-urls-multiple-replacements",
            None,
            None,
        ),
        ("test-root", Some("Sphinx <Tests>"), None),
        ("test-smartquotes", None, None),
    ];

    for (root, project, theme) in cases {
        let config = load(root, BridgeStrategy::Static);
        assert_eq!(config.source().and_then(|path| path.to_str()), roots_dir().join(root).join("conf.py").to_str());
        if let Some(project) = project {
            assert_eq!(config.get_str("project"), Some(*project), "project in {root}");
        }
        if let Some(theme) = theme {
            assert_eq!(config.get_str("html_theme"), Some(*theme), "html_theme in {root}");
        }
    }
}

#[test]
fn representative_values_parse_correctly() {
    let config = load("test-config", BridgeStrategy::Static);
    assert_eq!(config.get_str("release"), Some("0.6alpha1"));
    assert_eq!(config.get_strings("templates_path"), Some(vec!["_templates".to_owned()]));

    let config = load("test-copyright-multiline", BridgeStrategy::Static);
    assert_eq!(
        config.get_strings("copyright"),
        Some(vec![
            "2006".to_owned(),
            "2006-2009, Alice".to_owned(),
            "2010-2013, Bob".to_owned(),
            "2014-2017, Charlie".to_owned(),
            "2018-2021, David".to_owned(),
            "2022-2025, Eve".to_owned(),
        ])
    );
    assert_eq!(config.get_str("project_copyright"), None);

    let config = load("test-ext-autodoc", BridgeStrategy::Static);
    assert_eq!(
        config.get_strings("extensions"),
        Some(vec!["sphinx.ext.autodoc".to_owned()])
    );
    assert_eq!(config.get_bool("nitpicky"), Some(true));
    assert!(config.get_dict("autodoc_type_aliases").is_some());

    let config = load("test-theming", BridgeStrategy::Static);
    assert_eq!(
        config.get_strings("html_theme_path"),
        Some(vec![".".to_owned(), "test_theme".to_owned()])
    );

    let config = load("test-ext-extlinks-hardcoded-urls-multiple-replacements", BridgeStrategy::Static);
    assert_eq!(
        config.get_strings("extensions"),
        Some(vec!["sphinx.ext.extlinks".to_owned()])
    );
    let extlinks = config.get_dict("extlinks").expect("extlinks dict");
    assert!(extlinks.contains_key("user"));
    assert!(extlinks.contains_key("repo"));
}

#[test]
fn python_bridge_executes_imports_and_tags() {
    let config = load("test-ext-autodoc", BridgeStrategy::Python);
    assert_eq!(
        config.get_strings("extensions"),
        Some(vec!["sphinx.ext.autodoc".to_owned()])
    );
    assert_eq!(config.get_bool("nitpicky"), Some(true));
    assert_eq!(
        config.get_strings("autodoc_mock_imports"),
        Some(vec!["dummy".to_owned()])
    );

    let config = load("test-root", BridgeStrategy::Python);
    assert_eq!(config.get_str("project"), Some("Sphinx <Tests>"));
    assert_eq!(config.get_str("version"), Some("0.6"));
    assert_eq!(config.get_str("release"), Some("0.6alpha1"));
    assert_eq!(config.source_suffixes(), vec![".foo".to_owned(), ".txt".to_owned()]);
    assert_eq!(config.get_bool("keep_warnings"), Some(true));
    assert_eq!(config.get_float("linkcheck_timeout"), Some(0.25));
}

#[test]
fn unknown_keys_become_warnings_not_errors() {
    // Inject a conf.py with a genuinely unknown key into a temp directory.
    let dir = std::env::temp_dir().join(format!("scribe-unknown-key-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let conf = dir.join("conf.py");
    std::fs::write(&conf, "project = 'X'\nscribe_definitely_unknown_key = 1\n").expect("write conf");
    let request = LoadRequest {
        conf_dir: dir.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let (config, warnings) = load_config_with(request).expect("load");
    assert!(warnings.iter().any(|warning| warning.kind == ConfigWarningKind::UnknownKey));
    assert_eq!(config.get_str("project"), Some("X"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_config_directory_is_non_fatal() {
    let request = LoadRequest {
        conf_dir: roots_dir().join("this-root-does-not-exist"),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let (config, warnings) = load_config_with(request).expect("load");
    assert!(warnings.iter().any(|warning| warning.kind == ConfigWarningKind::NoConfigFile));
    assert_eq!(config.get_str("project"), Some("Project name not set"));
}
