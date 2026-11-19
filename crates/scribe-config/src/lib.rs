//! Configuration engine and `conf.py` bridge for `scribe`.
//!
//! This crate owns the strongly-typed configuration model used by every later
//! phase: [`ConfigValue`] is the tagged value representation, [`ConfigRegistry`]
//! describes known keys with upstream defaults and types, [`Config`] is the
//! resolved configuration object, and [`Tags`] evaluates `only` expressions.
//!
//! [`load_config`] is the main entry point. It accepts a configuration
//! directory, optional `-D` overrides and an optional explicit config file,
//! and returns the resolved [`Config`] together with any non-fatal
//! [`ConfigWarning`]s. Both native `scribe.toml` files and Python `conf.py`
//! files are supported; the latter can be loaded by executing Python in a
//! subprocess bridge or by statically parsing literal assignments in Rust.

mod conf_py;
mod registry;
mod tags;
mod toml_loader;
mod value;

pub use conf_py::{
    default_python_interpreter, evaluate_conf_py, load_conf_values, parse_static_conf_py,
    BridgeOutcome, BridgeStrategy, PythonBridgeOptions,
};
pub use registry::{ConfigDefault, ConfigEntry, ConfigRegistry, ConfigType};
pub use tags::{Tags, TagsError};
pub use toml_loader::load_toml;
pub use value::{ConfigValue, ConfigValueKind};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors raised by the configuration engine.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Reading a configuration file failed.
    #[error("I/O error while loading configuration: {0}")]
    Io(#[from] std::io::Error),

    /// No configuration file could be discovered in the supplied directory.
    #[error("no conf.py or scribe.toml in configuration directory `{0}`")]
    NoConfigFile(PathBuf),

    /// An explicitly requested config file does not exist.
    #[error("configuration file not found: {0}")]
    ConfigFileNotFound(PathBuf),

    /// A config file had an invalid top-level structure.
    #[error("invalid configuration file {}: {message}", .path.display())]
    InvalidConfigFile {
        /// Path of the offending file.
        path: PathBuf,
        /// Explanation.
        message: String,
    },

    /// The config file extension is not supported.
    #[error("unsupported configuration file format: {0}")]
    UnsupportedFormat(PathBuf),

    /// TOML parsing failed.
    #[error("invalid TOML configuration: {0}")]
    Toml(#[from] toml::de::Error),

    /// JSON returned by the Python bridge could not be parsed.
    #[error("invalid JSON from configuration bridge: {0}")]
    BridgeJson(#[from] serde_json::Error),

    /// The Python bridge failed.
    #[error("configuration bridge failed: {message}{}", stderr.as_str())]
    Bridge {
        /// Human-readable reason.
        message: String,
        /// Captured standard error from the child process.
        stderr: String,
    },

    /// The Python bridge exceeded its timeout.
    #[error("configuration bridge timed out after {seconds} seconds")]
    BridgeTimeout {
        /// Timeout in seconds.
        seconds: u64,
    },

    /// An attempt was made to register a duplicate key.
    #[error("config value `{0}` is already registered")]
    DuplicateKey(String),

    /// An override referenced an unknown key.
    #[error("unknown config value in override: `{0}`")]
    UnknownOverride(String),

    /// A key name was not present in the registry.
    #[error("unknown config value: `{0}`")]
    UnknownKey(String),

    /// A whole-dictionary override was attempted.
    #[error("cannot override dictionary config setting `{0}`; use `{0}.key=value`")]
    DictOverride(String),

    /// An override could not be parsed as a number.
    #[error("invalid number `{value}` for config value `{name}`")]
    InvalidNumber {
        /// Configuration key.
        name: String,
        /// Raw override value.
        value: String,
    },

    /// A boolean override was neither `0` nor `1`.
    #[error("config value `{name}` must be `0` or `1`, got `{value}`")]
    InvalidBoolOverride {
        /// Configuration key.
        name: String,
        /// Raw override value.
        value: String,
    },

    /// The static `conf.py` parser could not read a construct.
    #[error("static conf.py parser: {0}")]
    StaticParse(String),
}

/// Categories of non-fatal configuration diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigWarningKind {
    /// A key in a config file or override is not known to the registry.
    UnknownKey,
    /// A value's type differed from the registered type.
    TypeMismatch,
    /// An override was ignored (for example, a whole-dictionary override).
    OverrideIgnored,
    /// No config file was found; defaults are used.
    NoConfigFile,
    /// Python was unavailable or the bridge failed; a fallback was used.
    PythonUnavailable,
    /// `conf.py` raised an exception during evaluation.
    PythonExecutionFailed,
    /// A namespace value could not be serialized or converted.
    UnserializableValue,
    /// The static parser skipped a non-literal statement.
    StaticParseIssue,
    /// `language = None` was rewritten to `en`, as upstream does.
    LanguageFallback,
}

impl fmt::Display for ConfigWarningKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::UnknownKey => "unknown-key",
            Self::TypeMismatch => "type-mismatch",
            Self::OverrideIgnored => "override-ignored",
            Self::NoConfigFile => "no-config-file",
            Self::PythonUnavailable => "python-unavailable",
            Self::PythonExecutionFailed => "python-execution-failed",
            Self::UnserializableValue => "unserializable-value",
            Self::StaticParseIssue => "static-parse-issue",
            Self::LanguageFallback => "language-fallback",
        };
        formatter.write_str(name)
    }
}

/// A non-fatal configuration diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Diagnostic category.
    pub kind: ConfigWarningKind,
    /// Human-readable message.
    pub message: String,
    /// File the diagnostic concerns, when known.
    pub path: Option<PathBuf>,
    /// One-based line number, when known.
    pub line: Option<usize>,
}

impl ConfigWarning {
    /// Create a warning without a source location.
    pub fn new(kind: ConfigWarningKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            line: None,
        }
    }

    /// Attach a path to this warning.
    pub fn with_path(mut self, path: &Path) -> Self {
        self.path = Some(path.to_path_buf());
        self
    }

    /// Attach a one-based line number to this warning.
    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, self.line) {
            (Some(path), Some(line)) => write!(
                formatter,
                "{}:{}: warning [{}]: {}",
                path.display(),
                line,
                self.kind,
                self.message
            ),
            (Some(path), None) => {
                write!(formatter, "{}: warning [{}]: {}", path.display(), self.kind, self.message)
            }
            _ => write!(formatter, "warning [{}]: {}", self.kind, self.message),
        }
    }
}

/// Pairs of config key and callable default resolver.
type ComputedDefaults = Vec<(String, fn(&Config) -> ConfigValue)>;

/// A fully resolved Sphinx-like configuration object.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    values: BTreeMap<String, ConfigValue>,
    registry: ConfigRegistry,
    warnings: Vec<ConfigWarning>,
    source: Option<PathBuf>,
    /// Keys explicitly supplied by a config file or an override. Computed
    /// defaults are never used for these keys.
    explicit: BTreeSet<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Build a configuration containing only upstream defaults.
    pub fn new() -> Self {
        Self::with_registry(ConfigRegistry::builtin())
    }

    /// Build a configuration using a custom registry.
    pub fn with_registry(registry: ConfigRegistry) -> Self {
        let mut config = Self {
            values: BTreeMap::new(),
            registry,
            warnings: Vec::new(),
            source: None,
            explicit: BTreeSet::new(),
        };
        config.initialize_defaults();
        config
    }

    fn initialize_defaults(&mut self) {
        let fixed: Vec<(String, ConfigValue)> = self
            .registry
            .iter()
            .filter_map(|entry| match &entry.default {
                ConfigDefault::Value(value) => Some((entry.name.to_owned(), value.clone())),
                ConfigDefault::Computed(_) => None,
            })
            .collect();
        for (name, value) in fixed {
            self.values.insert(name, value);
        }
        self.resolve_computed_defaults();
    }

    /// Recompute callable defaults after file values and overrides have been
    /// applied. Keys explicitly set by the user are left untouched, matching
    /// upstream's lazy `__getattr__` resolution.
    pub fn refresh_computed_defaults(&mut self) {
        self.resolve_computed_defaults();
    }

    fn resolve_computed_defaults(&mut self) {
        let computed: ComputedDefaults = self
            .registry
            .iter()
            .filter_map(|entry| match &entry.default {
                ConfigDefault::Computed(function) => Some((entry.name.to_owned(), *function)),
                ConfigDefault::Value(_) => None,
            })
            .collect();

        // Computed defaults depend on one another (`html_short_title` uses
        // `html_title`, for example); a few passes reach a fixed point.
        for _ in 0..4 {
            let snapshot = self.clone();
            let mut changed = false;
            for (name, function) in &computed {
                if self.explicit.contains(name) {
                    continue;
                }
                let value = function(&snapshot);
                if self.values.get(name) != Some(&value) {
                    self.values.insert(name.clone(), value);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The registry backing this configuration.
    pub fn registry(&self) -> &ConfigRegistry {
        &self.registry
    }

    /// The file this configuration was loaded from, if any.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// All collected warnings.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// Append a warning.
    pub fn add_warning(&mut self, warning: ConfigWarning) {
        self.warnings.push(warning);
    }

    /// Borrow a raw value by key.
    pub fn get(&self, name: &str) -> Option<&ConfigValue> {
        self.values.get(name)
    }

    /// Borrow a string value by key.
    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(ConfigValue::get_str)
    }

    /// Copy a boolean value by key.
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(ConfigValue::get_bool)
    }

    /// Copy an integer value by key.
    pub fn get_int(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(ConfigValue::get_int)
    }

    /// Copy a floating-point value by key.
    pub fn get_float(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(ConfigValue::get_float)
    }

    /// Borrow a list value by key.
    pub fn get_list(&self, name: &str) -> Option<&[ConfigValue]> {
        self.get(name).and_then(ConfigValue::get_list)
    }

    /// Borrow a dictionary value by key.
    pub fn get_dict(&self, name: &str) -> Option<&BTreeMap<String, ConfigValue>> {
        self.get(name).and_then(ConfigValue::get_dict)
    }

    /// Borrow a path value by key.
    pub fn get_path(&self, name: &str) -> Option<&Path> {
        self.get(name).and_then(ConfigValue::get_path)
    }

    /// Return a list-valued setting as a vector of strings.
    pub fn get_strings(&self, name: &str) -> Option<Vec<String>> {
        self.get(name).and_then(ConfigValue::as_string_list)
    }

    /// Effective master document name (`master_doc` / `root_doc`).
    pub fn master_doc(&self) -> &str {
        self.get_str("master_doc").unwrap_or("index")
    }

    /// Registered source suffixes, in sorted order.
    pub fn source_suffixes(&self) -> Vec<String> {
        self.get_dict("source_suffix")
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Set a known key, maintaining upstream's `master_doc`/`root_doc` and
    /// `copyright`/`project_copyright` aliases.
    pub fn set(&mut self, name: &str, value: ConfigValue) -> Result<(), ConfigError> {
        if !self.registry.contains(name) {
            return Err(ConfigError::UnknownKey(name.to_owned()));
        }
        self.set_raw(name, value);
        Ok(())
    }

    /// Set a key that came from a config file or override, recording it as
    /// explicit so computed defaults will not replace it.
    pub fn set_explicit(&mut self, name: &str, value: ConfigValue) -> Result<(), ConfigError> {
        self.set(name, value)?;
        self.explicit.insert(name.to_owned());
        Ok(())
    }

    fn set_raw(&mut self, name: &str, value: ConfigValue) {
        self.values.insert(name.to_owned(), value.clone());
        match name {
            "master_doc" => {
                self.values.insert("root_doc".to_owned(), value);
            }
            "root_doc" => {
                self.values.insert("master_doc".to_owned(), value);
            }
            "copyright" => {
                self.values.insert("project_copyright".to_owned(), value);
            }
            "project_copyright" => {
                self.values.insert("copyright".to_owned(), value);
            }
            _ => {}
        }
    }

    /// Apply a raw `-D name=value` override, coercing via the registry.
    pub fn apply_override(&mut self, name: &str, raw: &str) -> Result<(), ConfigError> {
        if let Some((base, subkey)) = name.split_once('.') {
            if !self.registry.contains(base) {
                return Err(ConfigError::UnknownOverride(name.to_owned()));
            }
            let mut mapping = self
                .values
                .get(base)
                .cloned()
                .unwrap_or_else(|| ConfigValue::Dict(BTreeMap::new()));
            let Some(entries) = mapping.get_dict_mut() else {
                return Err(ConfigError::DictOverride(base.to_owned()));
            };
            // Upstream's `-A` flags pass through `_parse_confoverrides`, which
            // coerces numeric values to `int` before storing them.
            let member = if base == "html_context" {
                raw.parse::<i64>().map(ConfigValue::Int).unwrap_or_else(|_| ConfigValue::Str(raw.to_owned()))
            } else {
                ConfigValue::Str(raw.to_owned())
            };
            entries.insert(subkey.to_owned(), member);
            self.set_raw(base, mapping);
            self.explicit.insert(base.to_owned());
            return Ok(());
        }

        let value = self.registry.coerce_override(name, raw)?;
        self.set_raw(name, value);
        self.explicit.insert(name.to_owned());
        Ok(())
    }

    /// Iterate over all resolved `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ConfigValue)> {
        self.values.iter().map(|(name, value)| (name.as_str(), value))
    }
}

// `get_dict_mut` lives on `ConfigValue` but is not part of the public accessor
// set advertised in the architecture doc; keep it local via this extension.
trait DictMut {
    fn get_dict_mut(&mut self) -> Option<&mut BTreeMap<String, ConfigValue>>;
}

impl DictMut for ConfigValue {
    fn get_dict_mut(&mut self) -> Option<&mut BTreeMap<String, ConfigValue>> {
        match self {
            ConfigValue::Dict(entries) => Some(entries),
            _ => None,
        }
    }
}

/// Parameters for [`load_config_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    /// Directory searched for `conf.py` or `scribe.toml`.
    pub conf_dir: PathBuf,
    /// Raw `-D` overrides, applied after file values.
    pub overrides: BTreeMap<String, String>,
    /// Explicit config file; overrides auto-discovery when present.
    pub config_file: Option<PathBuf>,
    /// Tags injected into `conf.py` as `sphinx.util.tags.Tags`.
    pub tags: Vec<String>,
    /// `conf.py` loading strategy.
    pub strategy: BridgeStrategy,
    /// Python bridge settings.
    pub python: PythonBridgeOptions,
}

impl Default for LoadRequest {
    fn default() -> Self {
        Self {
            conf_dir: PathBuf::new(),
            overrides: BTreeMap::new(),
            config_file: None,
            tags: Vec::new(),
            strategy: BridgeStrategy::Auto,
            python: PythonBridgeOptions::default(),
        }
    }
}

/// Load configuration using the default options.
///
/// `overrides` uses the same `name=value` key/value split produced by the
/// CLI's `-D` flag (the key must not contain `=`). An explicit `config_file`
/// may be either a `conf.py` or a `scribe.toml`.
pub fn load_config(
    conf_dir: impl AsRef<Path>,
    overrides: Option<&BTreeMap<String, String>>,
    config_file: Option<&Path>,
) -> Result<(Config, Vec<ConfigWarning>), ConfigError> {
    load_config_with(LoadRequest {
        conf_dir: conf_dir.as_ref().to_path_buf(),
        overrides: overrides.cloned().unwrap_or_default(),
        config_file: config_file.map(Path::to_path_buf),
        ..LoadRequest::default()
    })
}

/// Load configuration with full control over discovery, overrides, tags and
/// the `conf.py` bridge strategy.
pub fn load_config_with(request: LoadRequest) -> Result<(Config, Vec<ConfigWarning>), ConfigError> {
    let mut config = Config::new();

    let file = resolve_config_file(&request)?;
    match file {
        Some(path) => {
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let (values, warnings) = match extension.as_str() {
                "toml" => load_toml(&path, config.registry())?,
                "py" => {
                    let outcome = load_conf_values(&path, &request.tags, &request.python, request.strategy)?;
                    (outcome.values, outcome.warnings)
                }
                _ => return Err(ConfigError::UnsupportedFormat(path.clone())),
            };

            config.source = Some(path.clone());
            for warning in warnings {
                config.add_warning(warning);
            }
            for (name, value) in values {
                match config.set_explicit(&name, value.clone()) {
                    Ok(()) => {}
                    Err(ConfigError::UnknownKey(key)) => {
                        // Upstream's config namespace retains arbitrary
                        // `conf.py` assignments; `sphinx.ext.ifconfig`
                        // evaluates expressions against that full namespace.
                        // Keep the value accessible while still warning that
                        // it is not a registered Sphinx setting.
                        config.set_raw(&name, value);
                        config.add_warning(
                            ConfigWarning::new(
                                ConfigWarningKind::UnknownKey,
                                format!("unknown config value `{key}` in {}, ignoring", path.display()),
                            )
                            .with_path(&path),
                        );
                    }
                    Err(error) => config.add_warning(
                        ConfigWarning::new(
                            ConfigWarningKind::TypeMismatch,
                            format!("could not apply `{name}` from {}: {error}", path.display()),
                        )
                        .with_path(&path),
                    ),
                }
            }

            normalize_source_suffix(&mut config);
            if config.get("language").is_some_and(ConfigValue::is_none) {
                config.add_warning(
                    ConfigWarning::new(
                        ConfigWarningKind::LanguageFallback,
                        "`language = None` is deprecated; falling back to `en`",
                    )
                    .with_path(&path),
                );
                config.set_raw("language", ConfigValue::from("en"));
            }
        }
        None => config.add_warning(ConfigWarning::new(
            ConfigWarningKind::NoConfigFile,
            format!(
                "no conf.py or scribe.toml found in `{}`; using defaults",
                request.conf_dir.display()
            ),
        )),
    }

    for (name, raw) in &request.overrides {
        if let Err(error) = config.apply_override(name, raw) {
            config.add_warning(ConfigWarning::new(
                ConfigWarningKind::OverrideIgnored,
                format!("override `{name}={raw}` ignored: {error}"),
            ));
        }
    }

    config.refresh_computed_defaults();

    let warnings = config.warnings().to_vec();
    Ok((config, warnings))
}

fn resolve_config_file(request: &LoadRequest) -> Result<Option<PathBuf>, ConfigError> {
    if let Some(path) = &request.config_file {
        if !path.is_file() {
            return Err(ConfigError::ConfigFileNotFound(path.clone()));
        }
        return Ok(Some(path.clone()));
    }

    let conf_py = request.conf_dir.join("conf.py");
    if conf_py.is_file() {
        return Ok(Some(conf_py));
    }
    let toml = request.conf_dir.join("scribe.toml");
    if toml.is_file() {
        return Ok(Some(toml));
    }
    Ok(None)
}

/// Mirror of upstream `convert_source_suffix`: strings and lists become a
/// mapping to `restructuredtext`.
fn normalize_source_suffix(config: &mut Config) {
    let Some(value) = config.get("source_suffix").cloned() else {
        return;
    };
    let normalized = match value {
        ConfigValue::Str(suffix) => ConfigValue::Dict(BTreeMap::from([(
            suffix,
            ConfigValue::from("restructuredtext"),
        )])),
        ConfigValue::List(items) => {
            let mut entries = BTreeMap::new();
            for item in items {
                let suffix = match item {
                    ConfigValue::Str(text) => Some(text),
                    ConfigValue::Path(path) => Some(path.to_string_lossy().into_owned()),
                    _ => None,
                };
                if let Some(suffix) = suffix {
                    entries.insert(suffix, ConfigValue::from("restructuredtext"));
                }
            }
            ConfigValue::Dict(entries)
        }
        _ => return,
    };
    config.set_raw("source_suffix", normalized);
}

#[cfg(test)]
mod tests {
    use super::{load_config, Config, ConfigWarningKind};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../sphinx/tests/roots")
            .join(name)
    }

    #[test]
    fn defaults_and_aliases_are_consistent() {
        let config = Config::new();
        assert_eq!(config.master_doc(), "index");
        assert_eq!(config.get_str("root_doc"), Some("index"));
        assert_eq!(config.get_str("copyright"), Some(""));
        assert_eq!(config.get_str("html_title"), Some("Project name not set  documentation"));
        assert_eq!(config.source_suffixes(), vec![".rst".to_owned()]);

        let mut config = config;
        config.set("master_doc", "contents".into()).expect("known key");
        assert_eq!(config.get_str("root_doc"), Some("contents"));
        config.set("root_doc", "index".into()).expect("known key");
        assert_eq!(config.get_str("master_doc"), Some("index"));
    }

    #[test]
    fn loading_a_real_root_applies_values() {
        let (config, warnings) = load_config(fixture("test-basic"), None, None).expect("load");
        assert_eq!(config.get_str("html_theme"), Some("basic"));
        assert!(config.get_list("latex_documents").is_some());
        assert!(warnings.iter().all(|warning| warning.kind != ConfigWarningKind::NoConfigFile));
    }

    #[test]
    fn overrides_are_coerced_by_registry_type() {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("nitpicky".to_owned(), "1".to_owned());
        overrides.insert("extensions".to_owned(), "sphinx.ext.autodoc,sphinx.ext.todo".to_owned());
        overrides.insert("project".to_owned(), "Override".to_owned());
        let (config, _) = load_config(fixture("test-basic"), Some(&overrides), None).expect("load");
        assert_eq!(config.get_bool("nitpicky"), Some(true));
        assert_eq!(
            config.get_strings("extensions"),
            Some(vec!["sphinx.ext.autodoc".to_owned(), "sphinx.ext.todo".to_owned()])
        );
        assert_eq!(config.get_str("project"), Some("Override"));
    }
}
