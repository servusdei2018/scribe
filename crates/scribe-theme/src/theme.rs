//! Theme discovery, metadata parsing and inheritance resolution.
//!
//! The algorithm mirrors `sphinx/theming.py`:
//!
//! * a theme is a directory containing either `theme.toml` (preferred) or the
//!   legacy `theme.conf`;
//! * `inherit` chains are followed child-first, up to 10 ancestors;
//! * `[options]` are merged root-first so a child overrides its parent;
//! * `stylesheets`, `sidebars` and `pygments_style` are taken from the
//!   nearest ancestor that defines them (also root-first, later assignments
//!   win over earlier ones).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dashmap::DashMap;
use miette::NamedSource;
use once_cell::sync::Lazy;
use scribe_config::Config;
use walkdir::WalkDir;

use crate::bundled::{BUNDLED_THEME_NAMES, BUNDLED_THEME_ROOT};
use crate::context::RenderContext;
use crate::environment::TemplateEnvironment;
use crate::error::{ThemeError, ThemeResult};

const MAX_ANCESTORS: usize = 10;

/// Site-package directories of the active Python environment, discovered once
/// per process. `SCRIBE_THEME_SITE_PATHS` (colon-separated) overrides detection
/// so tests can point discovery at a temporary tree deterministically.
static SITE_PACKAGES_DIRS: Lazy<Vec<PathBuf>> = Lazy::new(detect_site_packages_dirs);

/// Theme directories discovered under site-packages, computed once per process.
/// `ThemeManager::discover_environment_themes` clones this instead of
/// re-walking the Python environment on every (incremental) build.
static ENVIRONMENT_THEMES: Lazy<Vec<(String, PathBuf)>> = Lazy::new(|| {
    let mut found = Vec::new();
    for dir in SITE_PACKAGES_DIRS.iter() {
        for entry in WalkDir::new(dir)
            .follow_links(false)
            .min_depth(1)
            .max_depth(3)
            .into_iter()
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                entry.file_type().is_dir()
                    && !name.starts_with('.')
                    && name != "__pycache__"
            })
            .flatten()
        {
            let path = entry.path();
            if path.join("theme.toml").is_file() || path.join("theme.conf").is_file() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    found.push((name.to_owned(), path.to_path_buf()));
                }
            }
        }
    }
    found.sort();
    found
});

/// Best-effort detection of the active Python environment's site-packages.
///
/// Never fails the build: an absent or misbehaving interpreter simply yields an
/// empty list. The order mirrors `scribe-config`'s bridge (explicit override,
/// workspace-local oracle venv, then `python3` on `PATH`).
fn detect_site_packages_dirs() -> Vec<PathBuf> {
    if let Ok(raw) = std::env::var("SCRIBE_THEME_SITE_PATHS") {
        let paths: Vec<PathBuf> = raw
            .split(':')
            .filter(|part| !part.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }

    let mut dirs = Vec::new();
    if let Some(interpreter) = python_interpreter() {
        // `sysconfig` reports the purelib/platlib directories of whichever
        // interpreter actually backs the conf.py bridge.
        if let Ok(output) = Command::new(&interpreter)
            .args([
                "-c",
                "import sysconfig; p = sysconfig.get_paths(); print(p.get('purelib', '')); print(p.get('platlib', ''))",
            ])
            .output()
        {
            if output.status.success() {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let path = PathBuf::from(line.trim());
                    if path.is_dir() && !dirs.contains(&path) {
                        dirs.push(path);
                    }
                }
            }
        }
    }

    // Virtualenv / conda fallbacks (useful when no interpreter is on PATH).
    for key in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Ok(base) = std::env::var(key) {
            if let Ok(libraries) = fs::read_dir(Path::new(&base).join("lib")) {
                for library in libraries.flatten() {
                    let site = library.path().join("site-packages");
                    if site.is_dir() && !dirs.contains(&site) {
                        dirs.push(site);
                    }
                }
            }
        }
    }
    dirs
}

/// Locate a usable Python interpreter for site-packages discovery.
fn python_interpreter() -> Option<PathBuf> {
    if let Ok(candidate) = std::env::var("SCRIBE_PYTHON") {
        if !candidate.trim().is_empty() {
            return Some(PathBuf::from(candidate));
        }
    }
    if let Ok(current) = std::env::current_dir() {
        let mut directory = Some(current.as_path());
        while let Some(base) = directory {
            let candidate = base.join(".oracle-venv/bin/python");
            if candidate.is_file() {
                return Some(candidate);
            }
            directory = base.parent();
        }
    }
    let fallback = PathBuf::from("python3");
    Command::new(&fallback)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|_| fallback)
}

/// A fully resolved theme ready for template lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Theme name as selected by `html_theme`.
    pub name: String,
    /// Stylesheet filenames, in cascade order.
    pub stylesheets: Vec<String>,
    /// Sidebar template names.
    pub sidebars: Vec<String>,
    /// Merged theme options (defaults + `html_theme_options`).
    pub options: BTreeMap<String, String>,
    /// Template search directories, most specific first.
    pub template_dirs: Vec<PathBuf>,
    /// Number of leading [`Self::template_dirs`] entries that come from the
    /// user's `templates_path` configuration. Sphinx's Jinja loader honours a
    /// leading `!` on a template name by skipping these directories, so
    /// `{% extends "!layout.html" %}` resolves to the theme's own template.
    pub user_template_dirs: usize,
    /// Static asset directories, most specific first.
    pub static_dirs: Vec<PathBuf>,
    /// Pygments style hint from the theme metadata.
    pub pygments_style: Option<String>,
    /// Dark-mode Pygments style hint (TOML themes only).
    pub pygments_dark_style: Option<String>,
}

impl Theme {
    /// Return a merged option value.
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    /// Return a merged option converted to a boolean using Sphinx's `tobool`.
    #[must_use]
    pub fn option_bool(&self, name: &str) -> bool {
        self.option(name).map(to_bool).unwrap_or(false)
    }

    /// Directories that contain static files for this theme.
    #[must_use]
    pub fn static_files(&self) -> Vec<PathBuf> {
        theme_static_files(self)
    }
}

/// Parse `val.lower() in {'true','1','yes','on'}` from Sphinx's `_tobool`.
#[must_use]
pub fn to_bool(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// One parsed configuration file in an inheritance chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ThemeConfig {
    inherit: Option<String>,
    stylesheets: Option<Vec<String>>,
    sidebars: Option<Vec<String>>,
    pygments_style: Option<String>,
    pygments_dark_style: Option<String>,
    options: BTreeMap<String, String>,
}

impl ThemeConfig {
    /// Merge `self` (an ancestor) into an accumulator, overwriting defined values.
    fn merge_into(&self, theme: &mut Theme) {
        if let Some(stylesheets) = &self.stylesheets {
            theme.stylesheets = stylesheets.clone();
        }
        if let Some(sidebars) = &self.sidebars {
            theme.sidebars = sidebars.clone();
        }
        if let Some(style) = &self.pygments_style {
            theme.pygments_style = Some(style.clone());
        }
        if let Some(style) = &self.pygments_dark_style {
            theme.pygments_dark_style = Some(style.clone());
        }
        for (key, value) in &self.options {
            theme.options.insert(key.clone(), value.clone());
        }
    }
}

/// Discover, parse and instantiate themes.
#[derive(Debug)]
pub struct ThemeManager {
    /// name → theme directory (bundled + user paths).
    themes: BTreeMap<String, PathBuf>,
    /// Directories searched for user templates before theme templates.
    template_dirs: Vec<PathBuf>,
    /// `html_theme_options` overrides, applied to every created theme.
    overrides: BTreeMap<String, String>,
    /// Compiled template environments, keyed by theme + search path.
    environments: DashMap<String, Arc<TemplateEnvironment>>,
}

impl Default for ThemeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeManager {
    /// Build a manager containing the four bundled themes.
    #[must_use]
    pub fn new() -> Self {
        let mut manager = Self {
            themes: BTreeMap::new(),
            template_dirs: Vec::new(),
            overrides: BTreeMap::new(),
            environments: DashMap::new(),
        };
        for name in BUNDLED_THEME_NAMES {
            let dir = BUNDLED_THEME_ROOT.join(name);
            if dir.is_dir() {
                manager.themes.insert(name.to_owned(), dir);
            }
        }
        manager
    }

    /// Build a manager configured from `scribe-config`.
    ///
    /// Resolution order (first match wins): the four bundled themes, themes
    /// from project-local `html_theme_path` directories (resolved against
    /// `conf_dir`), then themes discovered in the active Python environment's
    /// `site-packages`. User templates from `templates_path` are searched
    /// before theme templates, and `html_theme_options` becomes the default
    /// override map. Missing directories are ignored, matching upstream's
    /// tolerant discovery.
    #[must_use]
    pub fn with_config(config: &Config, conf_dir: Option<&Path>) -> Self {
        Self::with_config_inner(config, conf_dir, true)
    }

    /// Like [`Self::with_config`], but deliberately omits the user
    /// `templates_path` directories. The HTML builder uses this when rendering
    /// generated index pages so a user template that minijinja cannot compile
    /// (e.g. Sphinx's `{% extends "!layout.html" %}`) cannot break those pages,
    /// while still resolving the project's selected theme correctly.
    #[must_use]
    pub fn with_config_no_user_templates(config: &Config, conf_dir: Option<&Path>) -> Self {
        Self::with_config_inner(config, conf_dir, false)
    }

    fn with_config_inner(config: &Config, conf_dir: Option<&Path>, user_templates: bool) -> Self {
        let mut manager = Self::new();

        if let Some(paths) = config.get_strings("html_theme_path") {
            for raw in paths {
                let path = resolve_against(raw, conf_dir);
                manager.add_theme_path(&path);
            }
        }
        // Site-package themes have the lowest precedence: `add_named_theme`
        // keeps bundled and project-local themes in front.
        // Phase 67: a bundled theme never inherits from a site-packages theme,
        // so walking the Python environment is pure overhead in that common
        // case (it dominated warm rebuild latency). Third-party and project
        // themes still get full discovery.
        let theme_is_bundled = config
            .get_str("html_theme")
            .is_some_and(|name| BUNDLED_THEME_NAMES.contains(&name));
        if !theme_is_bundled {
            manager.discover_environment_themes();
        }

        if user_templates {
            if let Some(paths) = config.get_strings("templates_path") {
                for raw in paths {
                    manager.add_template_dir(resolve_against(raw, conf_dir));
                }
            }
        }
        if let Some(dict) = config.get_dict("html_theme_options") {
            for (key, value) in dict {
                manager.set_option(key, config_value_to_string(value));
            }
        }
        manager
    }

    /// Register every theme found directly under `path`.
    ///
    /// A directory is a theme when it contains `theme.toml` or `theme.conf`.
    /// Names already registered (notably the four bundled themes) win, so a
    /// project directory cannot shadow a built-in theme.
    pub fn add_theme_path(&mut self, path: &Path) -> &mut Self {
        if let Ok(entries) = fs::read_dir(path) {
            let mut found: Vec<(String, PathBuf)> = entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let dir = entry.path();
                    dir.is_dir()
                        && (dir.join("theme.toml").is_file() || dir.join("theme.conf").is_file())
                })
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .map(|name| (name.to_owned(), entry.path()))
                })
                .collect();
            found.sort();
            for (name, dir) in found {
                self.add_named_theme(name, dir);
            }
        }
        self
    }

    /// Register a theme directory under `name` without displacing a theme
    /// that was registered earlier (enforcing the built-in → local →
    /// site-packages precedence).
    fn add_named_theme(&mut self, name: String, dir: PathBuf) -> bool {
        if self.themes.contains_key(&name) {
            return false;
        }
        self.themes.insert(name, dir);
        true
    }

    /// Discover third-party themes installed in the active Python
    /// environment's `site-packages`, including nested layouts used by
    /// `furo`, `pydata_sphinx_theme` and `sphinx_rtd_theme`.
    ///
    /// The filesystem walk is performed once per process and shared by every
    /// `ThemeManager` (Phase 67). Incremental rebuilds therefore do not pay the
    /// site-packages scan again; they clone the cached name→directory list.
    pub fn discover_environment_themes(&mut self) -> &mut Self {
        for (name, path) in ENVIRONMENT_THEMES.iter() {
            self.add_named_theme(name.clone(), path.clone());
        }
        self
    }

    /// Add a directory to the front of the template search chain.
    pub fn add_template_dir(&mut self, dir: PathBuf) -> &mut Self {
        if !self.template_dirs.contains(&dir) {
            self.template_dirs.push(dir);
        }
        self
    }

    /// Set an `html_theme_options` override.
    pub fn set_option(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.overrides.insert(name.into(), value.into());
        self
    }

    /// Clear all option overrides.
    pub fn clear_options(&mut self) -> &mut Self {
        self.overrides.clear();
        self
    }

    /// Sorted names of all discovered themes.
    pub fn theme_names(&self) -> impl Iterator<Item = &str> {
        self.themes.keys().map(String::as_str)
    }

    /// Resolve a theme by name, applying the configured option overrides.
    pub fn get_theme(&self, name: &str) -> ThemeResult<Theme> {
        let mut chain: Vec<(String, PathBuf, ThemeConfig)> = Vec::new();
        let mut visited: Vec<String> = Vec::new();
        let mut visited_set: HashSet<String> = HashSet::new();
        let mut current = name.to_owned();

        for _ in 0..MAX_ANCESTORS {
            if !visited_set.insert(current.clone()) {
                let start = visited.iter().position(|n| n == &current).unwrap_or(0);
                let mut cycle: Vec<String> = visited[start..].to_vec();
                cycle.push(current.clone());
                return Err(ThemeError::InheritanceCycle {
                    name: current,
                    cycle: cycle.join(" -> "),
                });
            }
            visited.push(current.clone());

            let dir = self.themes.get(&current).cloned().ok_or_else(|| {
                ThemeError::MissingParent {
                    name: if visited.len() > 1 {
                        visited[visited.len() - 2].clone()
                    } else {
                        current.clone()
                    },
                    parent: current.clone(),
                    known: self.known_themes(),
                }
            })?;

            let config = load_theme_config(&dir)?;
            let parent = config.inherit.clone();
            chain.push((current.clone(), dir, config));

            match parent {
                None => {
                    return Err(ThemeError::InvalidMetadata {
                        path: self.themes.get(&current).cloned().unwrap_or_default(),
                        detail: "the `theme.inherit` setting is missing".to_owned(),
                    });
                }
                Some(parent) if parent == "none" => break,
                Some(parent) => current = parent,
            }
        }

        // The loop above consumed MAX_ANCESTORS without reaching `none`.
        // Upstream reports the *last* name reached as having too many ancestors.
        if visited.len() >= MAX_ANCESTORS
            && chain
                .last()
                .and_then(|(_, _, config)| config.inherit.as_deref())
                .is_some_and(|parent| parent != "none")
        {
            return Err(ThemeError::TooManyAncestors {
                name: current,
                limit: MAX_ANCESTORS,
            });
        }

        // Upstream theme_dirs are child-first; template lookup prepends the
        // parent directories so `extends "basic/layout.html"` resolves.
        let theme_dirs: Vec<PathBuf> = chain.iter().map(|(_, dir, _)| dir.clone()).collect();
        let mut template_dirs = self.template_dirs.clone();
        template_dirs.extend(theme_dirs.iter().cloned());
        for dir in &theme_dirs {
            if let Some(parent) = dir.parent() {
                let parent = parent.to_path_buf();
                if !template_dirs.contains(&parent) {
                    template_dirs.push(parent);
                }
            }
        }

        let mut theme = Theme {
            name: name.to_owned(),
            stylesheets: Vec::new(),
            sidebars: Vec::new(),
            options: BTreeMap::new(),
            template_dirs,
            user_template_dirs: self.template_dirs.len(),
            static_dirs: theme_dirs
                .iter()
                .map(|dir| dir.join("static"))
                .filter(|dir| dir.is_dir())
                .collect(),
            pygments_style: None,
            pygments_dark_style: None,
        };

        // Root first, so the nearest ancestor wins.
        for (_, _, config) in chain.iter().rev() {
            config.merge_into(&mut theme);
        }
        for (key, value) in &self.overrides {
            theme.options.insert(key.clone(), value.clone());
        }
        Ok(theme)
    }

    /// Resolve the theme selected by `html_theme` in `config`.
    pub fn theme_from_config(&self, config: &Config) -> ThemeResult<Theme> {
        self.get_theme(config.get_str("html_theme").unwrap_or("alabaster"))
    }

    /// Return a cached (or newly compiled) environment for `theme`.
    pub fn environment(&self, theme: &Theme) -> ThemeResult<Arc<TemplateEnvironment>> {
        let key = format!(
            "{}|{}",
            theme.name,
            theme
                .template_dirs
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("|")
        );
        if let Some(cached) = self.environments.get(&key) {
            return Ok(cached.clone());
        }
        let environment = Arc::new(TemplateEnvironment::new(theme)?);
        self.environments.insert(key, environment.clone());
        Ok(environment)
    }

    /// Render a template with the theme named in `ctx.theme_name`.
    pub fn render_template(&self, name: &str, ctx: &RenderContext) -> ThemeResult<String> {
        let theme = self.get_theme(&ctx.theme_name)?;
        self.environment(&theme)?.render(name, ctx)
    }

    fn known_themes(&self) -> String {
        self.themes.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
    }
}

/// Parse the metadata file in a theme directory.
fn load_theme_config(dir: &Path) -> ThemeResult<ThemeConfig> {
    let toml_path = dir.join("theme.toml");
    if toml_path.is_file() {
        return parse_theme_toml(&toml_path);
    }
    let conf_path = dir.join("theme.conf");
    if conf_path.is_file() {
        return parse_theme_conf(&conf_path);
    }
    Err(ThemeError::MissingConfig {
        path: dir.to_path_buf(),
    })
}

/// Parse a `theme.toml` file using the same validation as upstream.
fn parse_theme_toml(path: &Path) -> ThemeResult<ThemeConfig> {
    let raw = fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        detail: source.to_string(),
    })?;
    let document: toml::Value = toml::from_str(&raw).map_err(|error| {
        let span = error.span().map(|range| {
            miette::SourceSpan::new(range.start.into(), range.end.saturating_sub(range.start))
        });
        ThemeError::InvalidToml {
            path: path.to_path_buf(),
            detail: error.to_string(),
            text: std::sync::Arc::new(NamedSource::new(path.to_string_lossy(), raw.clone())),
            span,
        }
    })?;

    let table = match document.as_table() {
        Some(table) => table,
        None => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: "top-level TOML value is not a table".to_owned(),
            })
        }
    };
    let theme_table = match table.get("theme").and_then(toml::Value::as_table) {
        Some(table) => table,
        None => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: "theme doesn't have the \"theme\" table".to_owned(),
            })
        }
    };
    let inherit = match theme_table.get("inherit").and_then(toml::Value::as_str) {
        Some(value) if !value.is_empty() => Some(value.to_owned()),
        _ => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: "theme must define the \"theme.inherit\" setting".to_owned(),
            })
        }
    };

    let stylesheets = match theme_table.get("stylesheets") {
        Some(value) => Some(string_array(value, "stylesheets", path)?),
        None => None,
    };
    let sidebars = match theme_table.get("sidebars") {
        Some(value) => Some(string_array(value, "sidebars", path)?),
        None => None,
    };
    let (pygments_style, pygments_dark_style) = match theme_table.get("pygments_style") {
        Some(toml::Value::String(style)) => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: format!(
                    "the \"theme.pygments_style\" setting must be a table; hint: pygments_style = {{ default = \"{style}\" }}"
                ),
            })
        }
        Some(toml::Value::Table(table)) => (
            table.get("default").map(toml_value_to_string),
            table.get("dark").map(toml_value_to_string),
        ),
        Some(_) => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: "the \"theme.pygments_style\" setting must be a table".to_owned(),
            })
        }
        None => (None, None),
    };

    let options = match table.get("options") {
        Some(toml::Value::Table(table)) => table
            .iter()
            .map(|(key, value)| (key.clone(), toml_value_to_string(value)))
            .collect(),
        Some(_) => {
            return Err(ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: "the \"options\" table is not a table".to_owned(),
            })
        }
        None => BTreeMap::new(),
    };

    Ok(ThemeConfig {
        inherit,
        stylesheets,
        sidebars,
        pygments_style,
        pygments_dark_style,
        options,
    })
}

fn string_array(value: &toml::Value, key: &str, path: &Path) -> ThemeResult<Vec<String>> {
    let array = value.as_array().ok_or_else(|| ThemeError::InvalidMetadata {
        path: path.to_path_buf(),
        detail: format!("\"theme.{key}\" must be an array of strings"),
    })?;
    array
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                ThemeError::InvalidMetadata {
                    path: path.to_path_buf(),
                    detail: format!("\"theme.{key}\" must be an array of strings"),
                }
            })
        })
        .collect()
}

fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(value) => value.clone(),
        toml::Value::Boolean(value) => value.to_string(),
        toml::Value::Integer(value) => value.to_string(),
        toml::Value::Float(value) => value.to_string(),
        toml::Value::Datetime(value) => value.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => value.to_string(),
    }
}

/// Parse the legacy INI-style `theme.conf` file.
fn parse_theme_conf(path: &Path) -> ThemeResult<ThemeConfig> {
    let raw = fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        detail: source.to_string(),
    })?;

    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = Some(section.trim().to_owned());
            sections.entry(current.clone().unwrap_or_default()).or_default();
            continue;
        }
        let separator = trimmed
            .find('=')
            .or_else(|| trimmed.find(':'))
            .ok_or_else(|| ThemeError::InvalidMetadata {
                path: path.to_path_buf(),
                detail: format!("line {} is not a `key = value` pair", index + 1),
            })?;
        let key = trimmed[..separator].trim().to_owned();
        let value = trimmed[separator + 1..].trim().to_owned();
        let section = current.clone().ok_or_else(|| ThemeError::InvalidMetadata {
            path: path.to_path_buf(),
            detail: format!("line {} appears before any [section]", index + 1),
        })?;
        sections.entry(section).or_default().insert(key, value);
    }

    let theme = sections.get("theme").ok_or_else(|| ThemeError::InvalidMetadata {
        path: path.to_path_buf(),
        detail: "theme doesn't have the \"theme\" table".to_owned(),
    })?;
    let inherit = theme
        .get("inherit")
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| ThemeError::InvalidMetadata {
            path: path.to_path_buf(),
            detail: "theme must define the \"theme.inherit\" setting".to_owned(),
        })?;
    let comma_list = |key: &str| -> Option<Vec<String>> {
        theme
            .get(key)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.split(',').map(str::trim).map(str::to_owned).collect())
    };

    Ok(ThemeConfig {
        inherit: Some(inherit),
        stylesheets: comma_list("stylesheet"),
        sidebars: comma_list("sidebars"),
        pygments_style: theme.get("pygments_style").cloned(),
        pygments_dark_style: theme.get("pygments_dark_style").cloned(),
        options: sections.get("options").cloned().unwrap_or_default(),
    })
}

fn resolve_against(raw: impl AsRef<str>, base: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(raw.as_ref());
    match base {
        Some(base) if path.is_relative() => base.join(path),
        _ => path,
    }
}

fn config_value_to_string(value: &scribe_config::ConfigValue) -> String {
    match value {
        scribe_config::ConfigValue::Str(value) => value.clone(),
        scribe_config::ConfigValue::Bool(value) => value.to_string(),
        scribe_config::ConfigValue::Int(value) => value.to_string(),
        scribe_config::ConfigValue::Float(value) => value.to_string(),
        scribe_config::ConfigValue::Path(value) => value.to_string_lossy().into_owned(),
        scribe_config::ConfigValue::None => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Collect every static file of a theme, in deterministic order.
///
/// The returned paths are real filesystem paths; Phase 17 copies them into
/// `_static/` after rendering `.jinja` / `_t` templates.
#[must_use]
pub fn theme_static_files(theme: &Theme) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in &theme.static_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(dir).follow_links(false).into_iter().flatten() {
            if entry.file_type().is_file() {
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files.dedup();
    files
}
