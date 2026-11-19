//! Registry of known configuration keys, their types and their defaults.
//!
//! The table below is transcribed from upstream `sphinx/config.py` (the core
//! values) and from the `setup()` functions of the standard builders and
//! extensions that register the remaining values. Callables used by upstream
//! as defaults are represented by [`ConfigDefault::Computed`] functions with
//! the same observable result.

use crate::{Config, ConfigError, ConfigValue};
use std::collections::BTreeMap;
use std::fmt;

/// The declared type of a configuration key.
///
/// Override coercion deliberately follows the *default value's* Python type
/// (as upstream `Config.convert_overrides` does); the declared type is kept for
/// documentation, validation and the `is_list`/`is_dict` helpers used by the
/// CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigType {
    /// A string (`str`).
    Str,
    /// A boolean (`bool`).
    Bool,
    /// A signed integer (`int`).
    Int,
    /// A floating-point number (`float`).
    Float,
    /// A list or tuple of strings.
    List,
    /// A mapping (`dict`).
    Dict,
    /// A filesystem path.
    Path,
    /// A fixed set of accepted string values (plus optional bool literals).
    Enum {
        /// String spellings of every accepted candidate, including `"True"`
        /// and `"False"` when booleans are accepted.
        candidates: &'static [&'static str],
    },
    /// Any value is accepted.
    Any,
}

impl ConfigType {
    /// Whether `0`/`1` should coerce to `False`/`True` for this type.
    pub fn accepts_bool_literals(&self) -> bool {
        match self {
            Self::Bool => true,
            Self::Enum { candidates } => {
                candidates.contains(&"False") || candidates.contains(&"True")
            }
            _ => false,
        }
    }

    /// Whether this type denotes a list.
    pub fn is_list(&self) -> bool {
        matches!(self, Self::List)
    }

    /// Whether this type denotes a mapping.
    pub fn is_dict(&self) -> bool {
        matches!(self, Self::Dict)
    }
}

impl fmt::Display for ConfigType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str => formatter.write_str("str"),
            Self::Bool => formatter.write_str("bool"),
            Self::Int => formatter.write_str("int"),
            Self::Float => formatter.write_str("float"),
            Self::List => formatter.write_str("list"),
            Self::Dict => formatter.write_str("dict"),
            Self::Path => formatter.write_str("path"),
            Self::Enum { candidates } => {
                write!(formatter, "enum({})", candidates.join("|"))
            }
            Self::Any => formatter.write_str("any"),
        }
    }
}

/// How a configuration key obtains its default value.
#[derive(Clone)]
pub enum ConfigDefault {
    /// A fixed default value.
    Value(ConfigValue),
    /// A default computed from other configuration values (upstream callable).
    Computed(fn(&Config) -> ConfigValue),
}

impl fmt::Debug for ConfigDefault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(value) => write!(formatter, "Value({value:?})"),
            Self::Computed(_) => formatter.write_str("Computed(<fn>)"),
        }
    }
}

impl PartialEq for ConfigDefault {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(left), Self::Value(right)) => left == right,
            (Self::Computed(left), Self::Computed(right)) => std::ptr::eq(left, right),
            _ => false,
        }
    }
}

impl Eq for ConfigDefault {}

/// Description of one known configuration key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Canonical name, e.g. `html_theme`.
    pub name: &'static str,
    /// Default value (fixed or computed).
    pub default: ConfigDefault,
    /// Declared value type.
    pub value_type: ConfigType,
    /// What upstream rebuilds when this value changes (`""`, `env`, `html`, ...).
    pub rebuild: &'static str,
    /// Short human-readable description.
    pub description: &'static str,
}

impl ConfigEntry {
    /// Resolve the effective default for this entry against `config`.
    pub fn default_value(&self, config: &Config) -> ConfigValue {
        match &self.default {
            ConfigDefault::Value(value) => value.clone(),
            ConfigDefault::Computed(function) => function(config),
        }
    }

    /// Whether this key holds a list-like value.
    pub fn is_list(&self) -> bool {
        self.value_type.is_list()
            || matches!(&self.default, ConfigDefault::Value(ConfigValue::List(_)))
    }

    /// Whether this key holds a mapping-like value.
    pub fn is_dict(&self) -> bool {
        self.value_type.is_dict()
            || matches!(&self.default, ConfigDefault::Value(ConfigValue::Dict(_)))
    }
}

/// Registry of all configuration keys known to this phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRegistry {
    entries: BTreeMap<String, ConfigEntry>,
}

impl Default for ConfigRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl ConfigRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Look up an entry by name.
    pub fn get(&self, name: &str) -> Option<&ConfigEntry> {
        self.entries.get(name)
    }

    /// Whether `name` is a known configuration key.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Number of registered keys.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all registered entries (ordered by name).
    pub fn iter(&self) -> impl Iterator<Item = &ConfigEntry> {
        self.entries.values()
    }

    /// Register an additional key, failing if it already exists.
    pub fn register(&mut self, entry: ConfigEntry) -> Result<(), ConfigError> {
        if self.entries.contains_key(entry.name) {
            return Err(ConfigError::DuplicateKey(entry.name.to_owned()));
        }
        self.entries.insert(entry.name.to_owned(), entry);
        Ok(())
    }

    /// Coerce a raw `-D name=value` string into the registered value type.
    ///
    /// Mirrors upstream `Config.convert_overrides`:
    /// * booleans accept only `0`/`1`;
    /// * lists are comma-split;
    /// * integers are parsed;
    /// * dictionaries reject whole-value overrides (use `name.key=value`);
    /// * enum values are returned verbatim, except `0`/`1` when `False`/`True`
    ///   are declared candidates;
    /// * strings and callables are returned verbatim.
    pub fn coerce_override(&self, name: &str, raw: &str) -> Result<ConfigValue, ConfigError> {
        let Some(entry) = self.get(name) else {
            return Err(ConfigError::UnknownOverride(name.to_owned()));
        };

        if entry.value_type == ConfigType::Any {
            return Ok(ConfigValue::Str(raw.to_owned()));
        }

        if let ConfigType::Enum { candidates } = &entry.value_type {
            if raw == "0" && candidates.contains(&"False") {
                return Ok(ConfigValue::Bool(false));
            }
            if raw == "1" && candidates.contains(&"True") {
                return Ok(ConfigValue::Bool(true));
            }
            return Ok(ConfigValue::Str(raw.to_owned()));
        }

        // Upstream branches on the *type of the default value*, not on the
        // declared valid types. Callable defaults are treated as strings.
        let default_bool = matches!(&entry.default, ConfigDefault::Value(ConfigValue::Bool(_)));
        let default_dict = matches!(&entry.default, ConfigDefault::Value(ConfigValue::Dict(_)));
        let default_list = matches!(&entry.default, ConfigDefault::Value(ConfigValue::List(_)));
        let default_int = matches!(&entry.default, ConfigDefault::Value(ConfigValue::Int(_)));
        let default_float = matches!(&entry.default, ConfigDefault::Value(ConfigValue::Float(_)));
        let default_callable = matches!(&entry.default, ConfigDefault::Computed(_));

        if entry.value_type == ConfigType::Bool || default_bool {
            return match raw {
                "0" => Ok(ConfigValue::Bool(false)),
                "1" => Ok(ConfigValue::Bool(true)),
                _ => Err(ConfigError::InvalidBoolOverride {
                    name: name.to_owned(),
                    value: raw.to_owned(),
                }),
            };
        }

        if default_dict {
            return Err(ConfigError::DictOverride(name.to_owned()));
        }

        if default_list {
            return Ok(ConfigValue::List(
                raw.split(',').map(|part| ConfigValue::Str(part.to_owned())).collect(),
            ));
        }

        if default_int || entry.value_type == ConfigType::Int {
            return raw.parse::<i64>().map(ConfigValue::Int).map_err(|_| {
                ConfigError::InvalidNumber {
                    name: name.to_owned(),
                    value: raw.to_owned(),
                }
            });
        }

        if default_float || entry.value_type == ConfigType::Float {
            return raw.parse::<f64>().map(ConfigValue::Float).map_err(|_| {
                ConfigError::InvalidNumber {
                    name: name.to_owned(),
                    value: raw.to_owned(),
                }
            });
        }

        // Strings, None, paths and callable defaults all pass through verbatim.
        let _ = default_callable;
        Ok(ConfigValue::Str(raw.to_owned()))
    }
}

const fn entry(
    name: &'static str,
    default: ConfigValue,
    value_type: ConfigType,
    rebuild: &'static str,
    description: &'static str,
) -> ConfigEntry {
    ConfigEntry {
        name,
        default: ConfigDefault::Value(default),
        value_type,
        rebuild,
        description,
    }
}

const fn computed(
    name: &'static str,
    default: fn(&Config) -> ConfigValue,
    value_type: ConfigType,
    rebuild: &'static str,
    description: &'static str,
) -> ConfigEntry {
    ConfigEntry {
        name,
        default: ConfigDefault::Computed(default),
        value_type,
        rebuild,
        description,
    }
}

fn string_list(items: &[&str]) -> ConfigValue {
    ConfigValue::List(items.iter().map(|item| ConfigValue::Str((*item).to_owned())).collect())
}

fn dict(items: &[(&str, ConfigValue)]) -> ConfigValue {
    ConfigValue::Dict(
        items
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

impl ConfigRegistry {
    /// Build the built-in registry with all core, builder and extension keys
    /// required by Phase 02.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        let entries = [
            // ---- General (sphinx/config.py) ----
            entry("project", ConfigValue::from("Project name not set"), ConfigType::Str, "env", "Project name."),
            entry("author", ConfigValue::from("Author name not set"), ConfigType::Str, "env", "Project author."),
            entry("extensions", string_list(&[]), ConfigType::List, "env", "Extension module names."),
            entry("project_copyright", ConfigValue::from(""), ConfigType::Str, "html", "Copyright string (canonical)."),
            computed("copyright", default_copyright, ConfigType::Str, "html", "Copyright string (alias of project_copyright)."),
            entry("version", ConfigValue::from(""), ConfigType::Str, "env", "Short version."),
            entry("release", ConfigValue::from(""), ConfigType::Str, "env", "Full version."),
            entry("today", ConfigValue::from(""), ConfigType::Str, "env", "Today's date."),
            entry("today_fmt", ConfigValue::None, ConfigType::Str, "env", "Format for today."),
            entry("language", ConfigValue::from("en"), ConfigType::Str, "env", "Document language."),
            entry("locale_dirs", string_list(&["locales"]), ConfigType::List, "env", "Locale directories."),
            entry("figure_language_filename", ConfigValue::from("{root}.{language}{ext}"), ConfigType::Str, "env", "Figure filename template."),
            entry("master_doc", ConfigValue::from("index"), ConfigType::Str, "env", "Master document name."),
            computed("root_doc", default_root_doc, ConfigType::Str, "env", "Root document (alias of master_doc)."),
            entry("source_suffix", dict(&[(".rst", ConfigValue::from("restructuredtext"))]), ConfigType::Dict, "env", "Mapping from extension to parser."),
            entry("source_encoding", ConfigValue::from("utf-8-sig"), ConfigType::Str, "env", "Source encoding."),
            entry("exclude_patterns", string_list(&[]), ConfigType::List, "env", "Globs excluded from discovery."),
            entry("include_patterns", string_list(&["**"]), ConfigType::List, "env", "Globs included in discovery."),
            entry("default_role", ConfigValue::None, ConfigType::Str, "env", "Default interpreted-text role."),
            entry("add_function_parentheses", ConfigValue::Bool(true), ConfigType::Bool, "env", "Append () to function names."),
            entry("add_module_names", ConfigValue::Bool(true), ConfigType::Bool, "env", "Prefix object names with module."),
            // Phase 12: Python-domain configuration values (upstream
            // `sphinx.domains.python.setup`).
            entry("python_use_unqualified_type_names", ConfigValue::Bool(false), ConfigType::Bool, "env", "Display only the last component of type names."),
            entry("python_display_short_literal_types", ConfigValue::Bool(false), ConfigType::Bool, "env", "Render Literal[...] annotations compactly."),
            entry("python_trailing_comma_in_multi_line_signatures", ConfigValue::Bool(true), ConfigType::Bool, "env", "Trailing comma in multi-line signatures."),
            entry("python_maximum_signature_line_length", ConfigValue::None, ConfigType::Int, "env", "Wrap signatures longer than this many characters."),
            entry("trim_footnote_reference_space", ConfigValue::Bool(false), ConfigType::Bool, "env", "Trim footnote spaces."),
            entry("show_authors", ConfigValue::Bool(false), ConfigType::Bool, "env", "Show authors."),
            entry("pygments_style", ConfigValue::None, ConfigType::Str, "html", "Pygments style."),
            entry("highlight_language", ConfigValue::from("default"), ConfigType::Str, "env", "Default highlighting language."),
            entry("highlight_options", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Per-language highlighting options."),
            entry("templates_path", string_list(&[]), ConfigType::List, "html", "Template directories."),
            entry("template_bridge", ConfigValue::None, ConfigType::Str, "html", "Template bridge class."),
            entry("keep_warnings", ConfigValue::Bool(false), ConfigType::Bool, "env", "Keep warnings in output."),
            entry("suppress_warnings", string_list(&[]), ConfigType::List, "env", "Warning subtypes to suppress."),
            entry("show_warning_types", ConfigValue::Bool(true), ConfigType::Bool, "env", "Show warning types."),
            entry("modindex_common_prefix", string_list(&[]), ConfigType::List, "html", "Common module-index prefix."),
            entry("rst_epilog", ConfigValue::None, ConfigType::Str, "env", "reST appended to every source."),
            entry("rst_prolog", ConfigValue::None, ConfigType::Str, "env", "reST prepended to every source."),
            entry("trim_doctest_flags", ConfigValue::Bool(true), ConfigType::Bool, "env", "Trim doctest flags."),
            entry("primary_domain", ConfigValue::from("py"), ConfigType::Str, "env", "Primary domain."),
            entry("needs_sphinx", ConfigValue::None, ConfigType::Str, "", "Required Sphinx version."),
            entry("needs_extensions", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "", "Required extension versions."),
            entry("manpages_url", ConfigValue::None, ConfigType::Str, "env", "Manpage URL template."),
            entry("nitpicky", ConfigValue::Bool(false), ConfigType::Bool, "", "Warn about all missing references."),
            entry("nitpick_ignore", string_list(&[]), ConfigType::List, "", "References to ignore."),
            entry("nitpick_ignore_regex", string_list(&[]), ConfigType::List, "", "Regex references to ignore."),
            entry("numfig", ConfigValue::Bool(false), ConfigType::Bool, "env", "Number figures and tables."),
            entry("numfig_secnum_depth", ConfigValue::Int(1), ConfigType::Int, "env", "Section-number depth."),
            entry("numfig_format", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Numbering format strings."),
            entry("math_number_all", ConfigValue::Bool(false), ConfigType::Bool, "env", "Number all math."),
            entry("math_numfig", ConfigValue::Bool(true), ConfigType::Bool, "env", "Number math per section."),
            entry("math_numsep", ConfigValue::from("."), ConfigType::Str, "env", "Math number separator."),
            entry("tls_verify", ConfigValue::Bool(true), ConfigType::Bool, "env", "Verify TLS certificates."),
            entry("tls_cacerts", ConfigValue::None, ConfigType::Str, "env", "CA certificate bundle."),
            entry("user_agent", ConfigValue::None, ConfigType::Str, "env", "HTTP user agent."),
            entry("smartquotes", ConfigValue::Bool(true), ConfigType::Bool, "env", "Enable smart quotes."),
            entry("smartquotes_action", ConfigValue::from("qDe"), ConfigType::Str, "env", "Smart quote actions."),
            entry("smartquotes_excludes", dict(&[
                ("languages", string_list(&["ja", "zh_CN", "zh_TW"])),
                ("builders", string_list(&["man", "text"])),
            ]), ConfigType::Dict, "env", "Smart quote exclusions."),
            entry("option_emphasise_placeholders", ConfigValue::Bool(false), ConfigType::Bool, "env", "Emphasise option placeholders."),
            entry("translation_progress_classes", ConfigValue::Bool(false),
                ConfigType::Enum { candidates: &["True", "False", "translated", "untranslated"] },
                "env", "Translation progress classes."),

            // ---- gettext / i18n (Phase 26) ----
            // `gettext_compact` is bool|str upstream; the Bool default makes
            // `-D gettext_compact=0` coerce correctly while a string loaded
            // from conf.py is consumed verbatim by the builder helpers.
            entry("gettext_compact", ConfigValue::Bool(true), ConfigType::Bool,
                "gettext", "Compact gettext domains to the first path segment."),
            entry("gettext_location", ConfigValue::Bool(true), ConfigType::Bool,
                "gettext", "Emit `#: file:line` location comments."),
            entry("gettext_uuid", ConfigValue::Bool(false), ConfigType::Bool,
                "gettext", "Emit UUID location comments instead of files."),
            entry("gettext_auto_build", ConfigValue::Bool(true), ConfigType::Bool,
                "env", "Compile updated .po catalogs into .mo automatically."),
            entry("gettext_additional_targets", string_list(&[]), ConfigType::List,
                "env", "Additional nodes to extract (e.g. `index`, `literal-block`)."),
            entry("gettext_last_translator", ConfigValue::from("FULL NAME <EMAIL@ADDRESS>"),
                ConfigType::Str, "gettext", "POT last-translator header."),
            entry("gettext_language_team", ConfigValue::from("LANGUAGE <LL@li.org>"),
                ConfigType::Str, "gettext", "POT language-team header."),
            entry("gettext_allow_fuzzy_translations", ConfigValue::Bool(false), ConfigType::Bool,
                "gettext", "Include fuzzy entries when compiling .mo catalogs."),

            // ---- HTML builder ----
            computed("htmlhelp_basename", default_htmlhelp_basename, ConfigType::Str, "html", "HTML Help basename (legacy compatibility key)."),
            entry("html_theme", ConfigValue::from("alabaster"), ConfigType::Str, "html", "HTML theme name."),
            entry("html_theme_path", string_list(&[]), ConfigType::List, "html", "Theme search paths."),
            entry("html_theme_options", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "html", "Theme options."),
            computed("html_title", default_html_title, ConfigType::Str, "html", "HTML title."),
            computed("html_short_title", default_html_short_title, ConfigType::Str, "html", "Short HTML title."),
            entry("html_style", ConfigValue::None, ConfigType::Str, "html", "Custom stylesheet."),
            entry("html_logo", ConfigValue::None, ConfigType::Str, "html", "Logo image."),
            entry("html_favicon", ConfigValue::None, ConfigType::Str, "html", "Favicon image."),
            entry("html_css_files", string_list(&[]), ConfigType::List, "html", "Extra CSS files."),
            entry("html_js_files", string_list(&[]), ConfigType::List, "html", "Extra JS files."),
            entry("html_static_path", string_list(&[]), ConfigType::List, "html", "Static file directories."),
            entry("html_extra_path", string_list(&[]), ConfigType::List, "html", "Extra file directories."),
            entry("html_context", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "html", "Template context."),
            entry("html_sidebars", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "html", "Sidebar templates."),
            entry("html_last_updated_fmt", ConfigValue::None, ConfigType::Str, "html", "Last-updated date format."),
            entry("html_baseurl", ConfigValue::from(""), ConfigType::Str, "html", "Canonical base URL."),
            entry("html_copy_source", ConfigValue::Bool(true), ConfigType::Bool, "html", "Copy source files."),
            entry("html_show_sourcelink", ConfigValue::Bool(true), ConfigType::Bool, "html", "Show source links."),
            entry("html_use_index", ConfigValue::Bool(true), ConfigType::Bool, "html", "Add an index page."),
            entry("html_permalinks", ConfigValue::Bool(true), ConfigType::Bool, "html", "Add permalinks."),
            entry("html_show_copyright", ConfigValue::Bool(true), ConfigType::Bool, "html", "Show copyright."),
            entry("html_show_sphinx", ConfigValue::Bool(true), ConfigType::Bool, "html", "Show Sphinx credit."),
            entry("html_search_language", ConfigValue::None, ConfigType::Str, "html", "Search language."),

            // ---- intersphinx ----
            entry("intersphinx_mapping", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Intersphinx targets."),
            entry("intersphinx_cache_limit", ConfigValue::Int(5), ConfigType::Int, "", "Inventory cache lifetime."),
            entry("intersphinx_timeout", ConfigValue::None, ConfigType::Float, "", "Intersphinx fetch timeout in seconds."),
            entry("intersphinx_resolve_self", ConfigValue::from(""), ConfigType::Str, "env", "Name of the current project for self-referential intersphinx targets."),
            entry("intersphinx_disabled_reftypes", string_list(&[]), ConfigType::List, "env", "Intersphinx reference types to disable."),
            entry("extlinks", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "External link templates."),
            entry("linkcheck_timeout", ConfigValue::Float(30.0), ConfigType::Float, "", "Linkcheck timeout."),
            entry("linkcheck_ignore", string_list(&[]), ConfigType::List, "", "Linkcheck ignore regexes."),
            entry("linkcheck_exclude_documents", string_list(&[]), ConfigType::List, "", "Documents excluded from link checking."),
            entry("linkcheck_allowed_redirects", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "", "Allowed linkcheck redirect patterns."),
            entry("linkcheck_auth", string_list(&[]), ConfigType::List, "", "Linkcheck authentication patterns."),
            entry("linkcheck_request_headers", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "", "Per-URL linkcheck request headers."),
            entry("linkcheck_retries", ConfigValue::Int(1), ConfigType::Int, "", "Linkcheck retry count."),
            entry("linkcheck_workers", ConfigValue::Int(5), ConfigType::Int, "", "Linkcheck worker count."),
            entry("linkcheck_anchors", ConfigValue::Bool(true), ConfigType::Bool, "", "Check URL anchors."),
            entry("linkcheck_anchor_re", string_list(&[]), ConfigType::List, "", "Additional anchor character classes."),
            entry("linkcheck_anchors_ignore", string_list(&["^!"]), ConfigType::List, "", "Ignored anchor patterns."),
            entry("linkcheck_anchors_ignore_for_url", string_list(&[]), ConfigType::List, "", "URLs whose anchors are ignored."),
            entry("linkcheck_rate_limit_timeout", ConfigValue::Float(300.0), ConfigType::Float, "", "Maximum linkcheck rate-limit delay."),
            entry("linkcheck_allow_unauthorized", ConfigValue::Bool(false), ConfigType::Bool, "", "Treat 401 as working."),
            entry("linkcheck_report_timeouts_as_broken", ConfigValue::Bool(false), ConfigType::Bool, "", "Report timeouts as broken links."),
            entry("linkcheck_case_insensitive_urls", string_list(&[]), ConfigType::List, "", "Case-insensitive URL patterns."),
            entry("xml_pretty", ConfigValue::Bool(true), ConfigType::Bool, "", "Pretty-print Docutils XML output."),
            entry("coverage_c_path", string_list(&[]), ConfigType::List, "", "C coverage header paths."),
            entry("coverage_c_regexes", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "", "C coverage regexes."),

            // ---- LaTeX / man / texinfo ----
            computed("latex_documents", default_latex_documents, ConfigType::List, "", "LaTeX document definitions."),
            entry("latex_toplevel_sectioning", ConfigValue::from("section"),
                ConfigType::Enum { candidates: &["part", "chapter", "section", "subsection", "subsubsection"] },
                "", "LaTeX top-level sectioning command."),
            entry("latex_engine", ConfigValue::from("pdflatex"),
                ConfigType::Enum { candidates: &["pdflatex", "xelatex", "lualatex", "platex", "uplatex"] },
                "", "LaTeX engine."),
            entry("latex_theme", ConfigValue::from("manual"), ConfigType::Str, "", "LaTeX theme."),
            entry("latex_logo", ConfigValue::None, ConfigType::Str, "", "LaTeX logo."),
            entry("latex_elements", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "", "LaTeX elements."),
            entry("latex_show_urls", ConfigValue::from("no"), ConfigType::Str, "", "Show URLs in LaTeX."),
            entry("latex_additional_files", string_list(&[]), ConfigType::List, "", "Extra LaTeX files."),
            computed("man_pages", default_man_pages, ConfigType::List, "", "Manpage definitions."),
            entry("man_show_urls", ConfigValue::Bool(false), ConfigType::Bool, "", "Show URLs in manpages."),
            entry("man_make_section_directory", ConfigValue::Bool(false), ConfigType::Bool, "", "Emit man pages into manN/ directories."),
            computed("texinfo_documents", default_texinfo_documents, ConfigType::List, "", "Texinfo document definitions."),
            entry("texinfo_show_urls", ConfigValue::from("footnote"), ConfigType::Str, "", "Show URLs in Texinfo."),
            entry("texinfo_appendices", string_list(&[]), ConfigType::List, "", "Texinfo appendix documents."),
            entry("texinfo_no_detailmenu", ConfigValue::Bool(false), ConfigType::Bool, "", "Suppress the Texinfo detail menu."),

            // ---- Plain text ----
            entry("text_width", ConfigValue::Int(80), ConfigType::Int, "text", "Plain-text wrap width (0 = terminal width, 80 fallback)."),
            entry("text_newlines", ConfigValue::from("unix"), ConfigType::Str, "text", "Plain-text newline style (unix/windows/native)."),
            entry("text_sectionchars", ConfigValue::from("*=-~\"+`"), ConfigType::Str, "text", "Heading underline characters by level."),
            entry("text_add_secnumbers", ConfigValue::Bool(true), ConfigType::Bool, "text", "Prefix section titles with toctree numbers."),
            entry("text_secnumber_suffix", ConfigValue::from(". "), ConfigType::Str, "text", "Separator after a section number."),
            entry("text_unicode_tables", ConfigValue::Bool(false), ConfigType::Bool, "text", "Draw plain-text tables with Unicode box characters."),

            // ---- EPUB ----
            computed("epub_title", default_epub_title, ConfigType::Str, "epub", "EPUB title."),
            computed("epub_author", default_epub_author, ConfigType::Str, "epub", "EPUB author."),
            computed("epub_copyright", default_copyright, ConfigType::Str, "epub", "EPUB copyright."),
            entry("epub_uid", ConfigValue::from("unknown"), ConfigType::Str, "epub", "EPUB unique identifier."),
            entry("epub_publisher", ConfigValue::None, ConfigType::Str, "epub", "EPUB publisher."),
            entry("epub_language", ConfigValue::from("en"), ConfigType::Str, "epub", "EPUB language."),
            entry("epub_basename", ConfigValue::None, ConfigType::Str, "epub", "EPUB output basename."),
            entry("epub_description", ConfigValue::from("unknown"), ConfigType::Str, "epub", "EPUB description."),
            entry("epub_contributor", ConfigValue::from("unknown"), ConfigType::Str, "epub", "EPUB contributor."),
            entry("epub_identifier", ConfigValue::from("unknown"), ConfigType::Str, "epub", "EPUB identifier value."),
            entry("epub_scheme", ConfigValue::from("unknown"), ConfigType::Str, "epub", "EPUB identifier scheme."),
            entry("epub_version", ConfigValue::from("3.0"), ConfigType::Str, "epub", "EPUB package version."),
            entry("epub_tocdepth", ConfigValue::Int(6), ConfigType::Int, "epub", "Maximum EPUB navigation depth."),
            entry("epub_writing_mode", ConfigValue::from("horizontal"), ConfigType::Str, "epub", "EPUB page progression direction."),
            entry("epub_exclude_files", string_list(&[]), ConfigType::List, "epub", "Files to exclude from the EPUB archive."),

            // ---- autodoc / todo ----
            entry("autoclass_content", ConfigValue::from("class"),
                ConfigType::Enum { candidates: &["both", "class", "init"] }, "env", "Autoclass content."),
            entry("autodoc_member_order", ConfigValue::from("alphabetical"),
                ConfigType::Enum { candidates: &["alphabetical", "bysource", "groupwise"] },
                "env", "Autodoc member order."),
            entry("autodoc_typehints", ConfigValue::from("signature"),
                ConfigType::Enum { candidates: &["signature", "description", "none", "both"] },
                "env", "Autodoc typehint rendering."),
            entry("autodoc_mock_imports", string_list(&[]), ConfigType::List, "env", "Mocked imports."),
            entry("autodoc_type_aliases", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Type alias mappings."),
            entry("autodoc_default_options", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Default autodoc options."),
            // ---- Phase 27 third-party Python/Wasm interop bridge ----
            entry("scribe_python_extensions", string_list(&[]), ConfigType::List, "env", "Python extension worker scripts to load via the JSON-RPC shim."),
            entry("scribe_python", ConfigValue::from("python3"), ConfigType::Str, "", "Python interpreter used by the extension shim."),
            entry("scribe_wasm_plugins", string_list(&[]), ConfigType::List, "env", "Wasm plugin manifests/modules to load via the plugin host."),
            entry("todo_include_todos", ConfigValue::Bool(false), ConfigType::Bool, "html", "Include todo nodes."),
            entry("todo_link_only", ConfigValue::Bool(false), ConfigType::Bool, "html", "Link-only todo nodes."),
            entry("todo_emit_warnings", ConfigValue::Bool(false), ConfigType::Bool, "env", "Emit a warning for every unresolved todo entry."),

            // ---- Phase 20 built-in extensions ----
            entry("extlinks_detect_hardcoded_links", ConfigValue::Bool(false), ConfigType::Bool, "env", "Warn about hardcoded links that an extlink could replace."),
            entry("viewcode_import", ConfigValue::None, ConfigType::Any, "", "Import hook hook for viewcode (native no-op)."),
            entry("viewcode_enable_epub", ConfigValue::Bool(false), ConfigType::Bool, "", "Enable viewcode for EPUB builders."),
            entry("viewcode_follow_imported_members", ConfigValue::Bool(true), ConfigType::Bool, "", "Follow imported members to their defining module."),
            entry("viewcode_line_numbers", ConfigValue::Bool(false), ConfigType::Bool, "env", "Show line numbers in viewcode module pages."),
            entry("graphviz_dot", ConfigValue::from("dot"), ConfigType::Str, "html", "Graphviz `dot` executable."),
            entry("graphviz_dot_args", string_list(&[]), ConfigType::List, "html", "Extra arguments passed to `dot`."),
            entry("graphviz_output_format", ConfigValue::from("png"), ConfigType::Str, "html", "Graphviz output format (`png` or `svg`)."),
            entry("duration_print_total", ConfigValue::Bool(true), ConfigType::Bool, "", "Print the total reading duration report."),
            entry("duration_print_slowest", ConfigValue::Bool(true), ConfigType::Bool, "", "Print the slowest reading durations report."),
            entry("duration_n_slowest", ConfigValue::Int(5), ConfigType::Int, "", "Number of slowest documents to print."),
            entry("duration_write_json", ConfigValue::None, ConfigType::Str, "", "Write reading durations to this JSON file."),
            entry("duration_limit", ConfigValue::None, ConfigType::Float, "", "Warn when a document's reading duration exceeds this value."),

            // ---- Phase 22 autosummary / doctest ----
            entry("autosummary_generate", ConfigValue::Bool(true), ConfigType::Bool, "env", "Generate autosummary stub pages before reading."),
            entry("autosummary_imported_members", ConfigValue::Bool(false), ConfigType::Bool, "env", "Include imported members in generated stubs."),
            entry("autosummary_filename_map", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Rename generated autosummary stub files."),
            entry("autosummary_ignore_module_all", ConfigValue::Bool(false), ConfigType::Bool, "env", "Ignore __all__ when generating stubs."),
            entry("autosummary_context", ConfigValue::Dict(BTreeMap::new()), ConfigType::Dict, "env", "Extra template context for autosummary stubs."),
            entry("doctest_test_doctest_blocks", ConfigValue::from("default"), ConfigType::Str, "env", "Group name for free-standing doctest blocks (empty disables them)."),
            entry("doctest_global_setup", ConfigValue::from(""), ConfigType::Str, "", "Python code run before every doctest group."),
            entry("doctest_global_cleanup", ConfigValue::from(""), ConfigType::Str, "", "Python code run after every doctest group."),
            entry("doctest_default_flags", ConfigValue::Int(41), ConfigType::Int, "", "Default doctest comparison option bitmask."),
            entry("doctest_fail_fast", ConfigValue::Bool(false), ConfigType::Bool, "", "Stop at the first failing doctest."),
            entry("doctest_show_successes", ConfigValue::Bool(true), ConfigType::Bool, "", "List successful groups in output.txt."),
            entry("doctest_path", string_list(&[]), ConfigType::List, "", "Extra sys.path entries for doctest execution."),
        ];

        for candidate in entries {
            let existing = registry.entries.insert(candidate.name.to_owned(), candidate);
            debug_assert!(existing.is_none(), "duplicate registry entry");
        }
        registry
    }
}

// ---- Computed defaults ---------------------------------------------------

fn default_copyright(config: &Config) -> ConfigValue {
    config.get("project_copyright").cloned().unwrap_or_else(|| ConfigValue::from(""))
}

fn default_root_doc(config: &Config) -> ConfigValue {
    config.get("master_doc").cloned().unwrap_or_else(|| ConfigValue::from("index"))
}

fn default_html_title(config: &Config) -> ConfigValue {
    let project = config.get_str("project").unwrap_or("Project name not set");
    let release = config.get_str("release").unwrap_or("");
    ConfigValue::Str(format!("{project} {release} documentation"))
}

fn default_html_short_title(config: &Config) -> ConfigValue {
    match config.get("html_title") {
        Some(value) => value.clone(),
        None => default_html_title(config),
    }
}

fn default_htmlhelp_basename(config: &Config) -> ConfigValue {
    let project = config.get_str("project").unwrap_or("Project name not set");
    ConfigValue::Str(format!("{}doc", make_filename_from_project(project)))
}

fn default_epub_title(config: &Config) -> ConfigValue {
    config
        .get("project")
        .cloned()
        .unwrap_or_else(|| ConfigValue::from("Project name not set"))
}

fn default_epub_author(config: &Config) -> ConfigValue {
    config
        .get("author")
        .cloned()
        .unwrap_or_else(|| ConfigValue::from("Author name not set"))
}

/// Mirror of `sphinx.util.osutil.make_filename_from_project`.
fn make_filename_from_project(project: &str) -> String {
    let stripped = project.strip_suffix(" Documentation").unwrap_or(project);
    let filename: String = stripped
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect();
    if filename.is_empty() {
        "sphinx".to_owned()
    } else {
        filename.to_lowercase()
    }
}

/// Subset of `sphinx.util.texescape.escape` covering the ASCII specials that
/// occur in project/author names. Unicode-specific replacements are deferred
/// to the LaTeX writer phase.
fn tex_escape(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '$' => result.push_str(r"\$"),
            '%' => result.push_str(r"\%"),
            '&' => result.push_str(r"\&"),
            '#' => result.push_str(r"\#"),
            '_' => result.push_str(r"\_"),
            '{' => result.push_str(r"\{"),
            '}' => result.push_str(r"\}"),
            '\\' => result.push_str(r"\textbackslash{}"),
            '~' => result.push_str(r"\textasciitilde{}"),
            '^' => result.push_str(r"\textasciicircum{}"),
            '[' => result.push_str("{[}"),
            ']' => result.push_str("{]}"),
            _ => result.push(ch),
        }
    }
    result
}

/// Mirror of `sphinx.util.texescape.escape_abbr`.
fn escape_abbr(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    for (index, ch) in characters.iter().enumerate() {
        if *ch == '.' && (index + 1 == characters.len() || characters[index + 1].is_whitespace()) {
            result.push_str(r".\@{}");
        } else {
            result.push(*ch);
        }
    }
    result
}

fn scalar(value: &str) -> ConfigValue {
    ConfigValue::from(value)
}

fn default_man_pages(config: &Config) -> ConfigValue {
    let project = config.get_str("project").unwrap_or("Project name not set");
    let release = config.get_str("release").unwrap_or("");
    let author = config.get_str("author").unwrap_or("Author name not set");
    let root = config.get_str("master_doc").unwrap_or("index");
    let filename = make_filename_from_project(project);
    ConfigValue::List(vec![ConfigValue::List(vec![
        scalar(root),
        scalar(&filename),
        scalar(&format!("{project} {release}")),
        ConfigValue::List(vec![scalar(author)]),
        ConfigValue::Int(1),
    ])])
}

fn default_latex_documents(config: &Config) -> ConfigValue {
    let project = config.get_str("project").unwrap_or("Project name not set");
    let author = config.get_str("author").unwrap_or("Author name not set");
    let root = config.get_str("master_doc").unwrap_or("index");
    let theme = config.get_str("latex_theme").unwrap_or("manual");
    let filename = make_filename_from_project(project);
    let title = escape_abbr(&tex_escape(project));
    let author = escape_abbr(&tex_escape(author));
    ConfigValue::List(vec![ConfigValue::List(vec![
        scalar(root),
        scalar(&format!("{filename}.tex")),
        scalar(&title),
        scalar(&author),
        scalar(theme),
    ])])
}

fn default_texinfo_documents(config: &Config) -> ConfigValue {
    let project = config.get_str("project").unwrap_or("Project name not set");
    let author = config.get_str("author").unwrap_or("Author name not set");
    let root = config.get_str("master_doc").unwrap_or("index");
    let filename = make_filename_from_project(project);
    ConfigValue::List(vec![ConfigValue::List(vec![
        scalar(root),
        scalar(&filename),
        scalar(project),
        scalar(author),
        scalar(&filename),
        scalar("One line description of project"),
        scalar("Miscellaneous"),
    ])])
}

#[cfg(test)]
mod tests {
    use super::ConfigRegistry;

    #[test]
    fn builtin_registry_has_all_required_keys() {
        let registry = ConfigRegistry::builtin();
        for key in [
            "project", "author", "copyright", "release", "version", "extensions",
            "templates_path", "exclude_patterns", "html_theme", "html_theme_options",
            "html_static_path", "html_title", "html_logo", "html_favicon",
            "intersphinx_mapping", "nitpicky", "suppress_warnings", "master_doc",
            "root_doc", "source_suffix", "language", "locale_dirs", "htmlhelp_basename",
            "latex_documents", "man_pages", "texinfo_documents", "epub_title",
            "todo_include_todos", "autodoc_member_order", "autodoc_typehints", "smartquotes",
        ] {
            assert!(registry.contains(key), "missing registry key: {key}");
        }
    }

    #[test]
    fn known_defaults_match_upstream() {
        let registry = ConfigRegistry::builtin();
        let config = crate::Config::new();
        assert_eq!(config.get_str("project"), Some("Project name not set"));
        assert_eq!(config.get_str("html_theme"), Some("alabaster"));
        assert_eq!(config.get_str("master_doc"), Some("index"));
        assert_eq!(config.get_str("root_doc"), Some("index"));
        assert_eq!(config.get_str("language"), Some("en"));
        assert_eq!(config.get_bool("smartquotes"), Some(true));
        assert_eq!(config.get_bool("nitpicky"), Some(false));
        assert_eq!(config.get_str("autodoc_member_order"), Some("alphabetical"));
        assert_eq!(config.get_str("autodoc_typehints"), Some("signature"));
        assert_eq!(
            registry.get("extensions").map(|entry| entry.is_list()),
            Some(true)
        );
    }
}
