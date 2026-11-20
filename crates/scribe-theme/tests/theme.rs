//! Theme metadata parsing, inheritance and option tests.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use scribe_theme::{RenderContext, TemplateEnvironment, ThemeError, ThemeManager, theme_static_files};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempThemes {
    root: PathBuf,
}

impl TempThemes {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-theme-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp theme root");
        Self { root }
    }

    fn write(&self, name: &str, metadata: &str) {
        let dir = self.root.join(name);
        fs::create_dir_all(dir.join("static")).expect("theme dir");
        fs::write(dir.join("theme.toml"), metadata).expect("theme.toml");
    }

    fn write_conf(&self, name: &str, metadata: &str) {
        let dir = self.root.join(name);
        fs::create_dir_all(dir.join("static")).expect("theme dir");
        fs::write(dir.join("theme.conf"), metadata).expect("theme.conf");
    }

    fn manager(&self) -> ThemeManager {
        let mut manager = ThemeManager::new();
        manager.add_theme_path(&self.root);
        manager
    }
}

impl Drop for TempThemes {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn bundled_themes_are_discovered() {
    let manager = ThemeManager::new();
    let mut names: Vec<&str> = manager.theme_names().collect();
    names.sort_unstable();
    assert_eq!(names, vec!["alabaster", "basic", "classic", "nature"]);

    let basic = manager.get_theme("basic").expect("basic theme");
    assert_eq!(basic.stylesheets, vec!["basic.css"]);
    assert_eq!(
        basic.sidebars,
        vec![
            "localtoc.html",
            "relations.html",
            "sourcelink.html",
            "searchbox.html"
        ]
    );
    assert_eq!(basic.pygments_style.as_deref(), Some("none"));
    assert_eq!(basic.option("sidebarwidth"), Some("230"));
}

#[test]
fn classic_inherits_basic_metadata_and_options() {
    let theme = ThemeManager::new().get_theme("classic").expect("classic");
    assert_eq!(theme.stylesheets, vec!["classic.css"]);
    assert_eq!(
        theme.sidebars,
        vec![
            "localtoc.html",
            "relations.html",
            "sourcelink.html",
            "searchbox.html"
        ]
    );
    assert_eq!(theme.pygments_style.as_deref(), Some("sphinx"));
    // Parent option, not overridden by classic.
    assert_eq!(theme.option("nosidebar"), Some("false"));
    assert_eq!(theme.option("rightsidebar"), Some("false"));
}

#[test]
fn nature_inherits_basic_and_sets_pygments() {
    let theme = ThemeManager::new().get_theme("nature").expect("nature");
    assert_eq!(theme.stylesheets, vec!["nature.css"]);
    assert_eq!(theme.pygments_style.as_deref(), Some("tango"));
    assert_eq!(theme.option("sidebarwidth"), Some("230"));
}

#[test]
fn alabaster_legacy_theme_conf_is_parsed() {
    let theme = ThemeManager::new().get_theme("alabaster").expect("alabaster");
    assert_eq!(theme.stylesheets, vec!["basic.css", "alabaster.css"]);
    assert_eq!(
        theme.sidebars,
        vec![
            "about.html",
            "searchfield.html",
            "navigation.html",
            "relations.html",
            "donate.html"
        ]
    );
    assert_eq!(theme.pygments_style.as_deref(), Some("alabaster.support.Alabaster"));
    assert_eq!(theme.option("page_width"), Some("940px"));
    assert_eq!(theme.option("gray_1"), Some("#444"));
    assert_eq!(theme.option("sidebar_width"), Some("220px"));
}

#[test]
fn multi_level_inheritance_flattens_root_first() {
    let temp = TempThemes::new();
    temp.write(
        "grand",
        "[theme]\ninherit = \"none\"\nstylesheets = [\"grand.css\"]\nsidebars = [\"a.html\"]\n\n[options]\nshared = \"grand\"\nonly_grand = \"1\"\n",
    );
    temp.write(
        "parent",
        "[theme]\ninherit = \"grand\"\nstylesheets = [\"parent.css\"]\n\n[options]\nshared = \"parent\"\nonly_parent = \"1\"\n",
    );
    temp.write(
        "child",
        "[theme]\ninherit = \"parent\"\npygments_style = { default = \"friendly\", dark = \"monokai\" }\n\n[options]\nonly_child = \"true\"\n",
    );
    let theme = temp.manager().get_theme("child").expect("child theme");
    assert_eq!(theme.stylesheets, vec!["parent.css"]);
    assert_eq!(theme.sidebars, vec!["a.html"]);
    assert_eq!(theme.pygments_style.as_deref(), Some("friendly"));
    assert_eq!(theme.pygments_dark_style.as_deref(), Some("monokai"));
    assert_eq!(theme.option("shared"), Some("parent"));
    assert_eq!(theme.option("only_grand"), Some("1"));
    assert_eq!(theme.option("only_parent"), Some("1"));
    assert_eq!(theme.option("only_child"), Some("true"));
    assert_eq!(theme.template_dirs.len(), 3 + 1); // three dirs + one shared parent
    assert_eq!(theme.static_dirs.len(), 3);
}

#[test]
fn option_overrides_are_applied() {
    let temp = TempThemes::new();
    temp.write("solo", "[theme]\ninherit = \"none\"\n\n[options]\nsize = \"10\"\ncolor = \"red\"\n");
    let mut manager = temp.manager();
    manager.set_option("size", "99").set_option("color", "blue");
    let theme = manager.get_theme("solo").expect("solo");
    assert_eq!(theme.option("size"), Some("99"));
    assert_eq!(theme.option("color"), Some("blue"));
    assert!(!theme.option_bool("size")); // upstream `tobool` only accepts true/1/yes/on
    manager.set_option("flag", "YES");
    assert!(manager.get_theme("solo").expect("solo").option_bool("flag"));
}

#[test]
fn cycles_and_missing_parents_are_typed_errors() {
    let temp = TempThemes::new();
    temp.write("a", "[theme]\ninherit = \"b\"\n");
    temp.write("b", "[theme]\ninherit = \"a\"\n");
    temp.write("lonely", "[theme]\ninherit = \"ghost\"\n");

    let manager = temp.manager();
    let cycle = manager.get_theme("a").expect_err("cycle must fail");
    assert_eq!(cycle.code(), "scribe::theme::inheritance_cycle");
    assert!(cycle.to_string().contains("a -> b -> a"), "{cycle}");

    let missing = manager.get_theme("lonely").expect_err("missing parent must fail");
    assert_eq!(missing.code(), "scribe::theme::missing_parent");
    assert!(missing.to_string().contains("ghost"), "{missing}");

    let unknown = manager.get_theme("nope").expect_err("unknown must fail");
    assert_eq!(unknown.code(), "scribe::theme::missing_parent");
}

#[test]
fn malformed_metadata_never_panics() {
    let temp = TempThemes::new();
    temp.write("no_theme_table", "[options]\na = \"b\"\n");
    temp.write("no_inherit", "[theme]\nstylesheets = []\n");
    temp.write("bad_pygments", "[theme]\ninherit = \"none\"\npygments_style = \"sphinx\"\n");
    temp.write("bad_toml", "[theme\ninherit = \"none\"\n");
    temp.write("bad_type", "[theme]\ninherit = \"none\"\nstylesheets = \"x.css\"\n");
    temp.write_conf("bad_conf", "[options]\na = b\n");

    let manager = temp.manager();
    for name in [
        "no_theme_table",
        "no_inherit",
        "bad_pygments",
        "bad_toml",
        "bad_type",
        "bad_conf",
    ] {
        let error = manager.get_theme(name).expect_err("malformed theme must fail");
        assert!(error.to_string().contains(name) || error.code().starts_with("scribe::theme::"));
    }
}

#[test]
fn static_files_are_deterministic() {
    let manager = ThemeManager::new();
    let basic = manager.get_theme("basic").expect("basic");
    let files = theme_static_files(&basic);
    assert!(!files.is_empty());
    assert!(files.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(files.iter().any(|path| path.ends_with("basic.css.jinja")));

    let alabaster = manager.get_theme("alabaster").expect("alabaster");
    let files = theme_static_files(&alabaster);
    assert!(files.iter().any(|path| path.ends_with("alabaster.css_t")));
    assert!(files.iter().any(|path| path.ends_with("custom.css")));
}

#[test]
fn unknown_theme_error_lists_known_themes() {
    let error = ThemeManager::new().get_theme("missing").expect_err("unknown");
    assert!(error.to_string().contains("alabaster"));
    assert!(matches!(error, ThemeError::MissingParent { .. }));
}

#[test]
fn local_path_theme_inherits_bundled_basic() {
    // A project-local theme that inherits from a bundled theme must resolve
    // templates from both directories, exactly like sphinx/doc's `sphinx13`.
    let temp = TempThemes::new();
    temp.write(
        "projecttheme",
        "[theme]\ninherit = \"basic\"\nstylesheets = [\"project.css\"]\nsidebars = []\n\n[options]\naccent = \"blue\"\n",
    );
    let theme = temp.manager().get_theme("projecttheme").expect("project theme");
    assert_eq!(theme.name, "projecttheme");
    assert_eq!(theme.stylesheets, vec!["project.css"]);
    assert_eq!(theme.option("accent"), Some("blue"));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("projecttheme")));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("basic")));
    assert!(theme.static_dirs.iter().any(|p| p.ends_with("projecttheme/static")));
}

#[test]
fn multi_level_external_chain_merges_stylesheets_and_css_variables() {
    // A three-level chain rooted in a bundled theme: CSS variables/options
    // are merged root-first (nearest ancestor wins for defined keys).
    let temp = TempThemes::new();
    temp.write(
        "brand",
        "[theme]\ninherit = \"basic\"\nstylesheets = [\"basic.css\", \"brand.css\"]\n\n[options]\n--brand-color = \"#0A507A\"\n--layout = \"root\"\n",
    );
    temp.write(
        "child",
        "[theme]\ninherit = \"brand\"\n\n[options]\n--layout = \"child\"\n",
    );
    temp.write(
        "grandchild",
        "[theme]\ninherit = \"child\"\nstylesheets = [\"basic.css\", \"brand.css\", \"grandchild.css\"]\n",
    );
    let theme = temp.manager().get_theme("grandchild").expect("grandchild");
    assert_eq!(
        theme.stylesheets,
        vec!["basic.css", "brand.css", "grandchild.css"]
    );
    assert_eq!(theme.option("--brand-color"), Some("#0A507A"));
    assert_eq!(theme.option("--layout"), Some("child"));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("grandchild")));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("child")));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("brand")));
    assert!(theme.template_dirs.iter().any(|p| p.ends_with("basic")));
}

#[test]
fn builtin_themes_win_over_local_path_collisions() {
    // Resolution order: bundled themes are registered first and cannot be
    // shadowed by a same-named directory in html_theme_path.
    let temp = TempThemes::new();
    temp.write("basic", "[theme]\ninherit = \"none\"\nstylesheets = [\"hijacked.css\"]\n");
    let theme = temp.manager().get_theme("basic").expect("bundled basic");
    assert_eq!(theme.stylesheets, vec!["basic.css"]);
}

#[test]
fn cycle_detection_works_across_external_themes() {
    let temp = TempThemes::new();
    temp.write("x", "[theme]\ninherit = \"y\"\n");
    temp.write("y", "[theme]\ninherit = \"z\"\n");
    temp.write("z", "[theme]\ninherit = \"x\"\n");
    let error = temp.manager().get_theme("x").expect_err("cycle");
    assert_eq!(error.code(), "scribe::theme::inheritance_cycle");
    assert!(error.to_string().contains("x -> y -> z -> x"), "{error}");
}

#[test]
fn site_packages_discovery_finds_nested_theme_layouts() {
    // Simulate a `site-packages` tree with a nested third-party theme layout
    // (`furo/theme/furo/theme.conf`) via the explicit discovery path hook.
    let root = TempThemes::new();
    let site = root.root.join("site-packages");
    let nested = site.join("furo/theme/furo");
    fs::create_dir_all(nested.join("static")).expect("nested theme dir");
    fs::write(nested.join("theme.conf"), "[theme]\ninherit = basic\nstylesheet = furo.css\n")
        .expect("theme.conf");

    // SAFETY: edition 2021; this test binary is the only one that mutates the
    // discovery hook, and `SITE_PACKAGES_DIRS` is lazily initialised on first
    // use inside this test.
    std::env::set_var("SCRIBE_THEME_SITE_PATHS", &site);
    let mut manager = ThemeManager::new();
    manager.discover_environment_themes();
    std::env::remove_var("SCRIBE_THEME_SITE_PATHS");

    assert!(manager.theme_names().any(|name| name == "furo"));
    let theme = manager.get_theme("furo").expect("furo from site-packages");
    assert_eq!(theme.stylesheets, vec!["furo.css"]);
    assert!(theme.static_dirs.iter().any(|p| p.ends_with("furo/static")));
}

#[test]
fn bang_prefix_bypasses_user_template_dirs() {
    // Sphinx's `SphinxFileSystemLoader` strips a leading `!` and starts the
    // search after the user's `templates_path` directories, so a user template
    // can extend the theme's own `layout.html` instead of recursing into itself.
    let temp = TempThemes::new();
    let user = temp.root.join("user_templates");
    let theme_dir = temp.root.join("theme_templates");
    fs::create_dir_all(&user).expect("user template dir");
    fs::create_dir_all(&theme_dir).expect("theme template dir");
    fs::write(user.join("layout.html"), "USER|{% block body %}{% endblock %}")
        .expect("user layout");
    fs::write(theme_dir.join("layout.html"), "THEME|{% block body %}{% endblock %}")
        .expect("theme layout");

    let mut theme = ThemeManager::new().get_theme("basic").expect("basic");
    theme.template_dirs = vec![user.clone(), theme_dir.clone()];
    theme.user_template_dirs = 1;
    let env = TemplateEnvironment::with_search_paths(&theme, vec![user, theme_dir]).expect("env");
    let ctx = RenderContext::new("index").with_theme("basic");

    let plain = env
        .render_string(
            "x.html",
            "{% extends 'layout.html' %}{% block body %}B{% endblock %}",
            &ctx,
        )
        .expect("plain extends");
    assert_eq!(plain, "USER|B");

    let bypass = env
        .render_string(
            "x.html",
            "{% extends '!layout.html' %}{% block body %}B{% endblock %}",
            &ctx,
        )
        .expect("bang extends");
    assert_eq!(bypass, "THEME|B");
}
