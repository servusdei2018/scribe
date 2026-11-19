//! Catalog discovery, `.po` → `.mo` compilation and loading (Phase 26).
//!
//! Upstream reference: `sphinx/util/i18n.py` (`CatalogInfo`,
//! `CatalogRepository`, `docname_to_domain`) and
//! `sphinx/builders/gettext.py::setup` (`gettext_auto_build`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use scribe_config::Config;
use scribe_core::TranslationCatalog;

use super::mo::{read_mo, write_mo, MoError};
use super::po::PoCatalog;

/// Converts a docname to a gettext domain, mirroring
/// `sphinx.util.i18n.docname_to_domain`.
///
/// * a string `gettext_compact` is used verbatim;
/// * `true` (default) keeps the first path component;
/// * `false` uses the whole docname.
pub fn docname_to_domain(docname: &str, compact: &str) -> String {
    if compact.starts_with(':') {
        return compact.trim_start_matches(':').to_owned();
    }
    match compact {
        "true" | "1" => docname.split('/').next().unwrap_or(docname).to_owned(),
        "false" | "0" => docname.to_owned(),
        "" => docname.to_owned(),
        other => other.to_owned(),
    }
}

/// Resolves `gettext_compact` from config into the string form consumed by
/// [`docname_to_domain`].
pub fn compact_setting(config: &Config) -> String {
    if let Some(value) = config.get_bool("gettext_compact") {
        return if value { "true" } else { "false" }.to_owned();
    }
    config
        .get_str("gettext_compact")
        .filter(|value| !value.is_empty())
        .unwrap_or("true")
        .to_owned()
}

/// Paths of a domain's `.po` and `.mo` files for one locale directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPaths {
    /// Directory containing `LC_MESSAGES`.
    pub locale_messages: PathBuf,
    /// Source `.po` file.
    pub po: PathBuf,
    /// Compiled `.mo` file.
    pub mo: PathBuf,
}

/// Enumerates existing locale directories in config order.
pub fn locale_dirs(config: &Config) -> Vec<String> {
    let dirs = config.get_strings("locale_dirs").unwrap_or_default();
    if dirs.is_empty() {
        vec!["locales".to_owned()]
    } else {
        dirs
    }
}

/// Effective language code (empty means "no translation").
pub fn language(config: &Config) -> String {
    config.get_str("language").unwrap_or("en").to_owned()
}

/// Computes catalog paths for every configured locale directory.
pub fn catalog_paths(config: &Config, source_dir: &Path, domain: &str) -> Vec<CatalogPaths> {
    let language = language(config);
    if language.is_empty() {
        return Vec::new();
    }
    locale_dirs(config)
        .into_iter()
        .map(|locale_dir| {
            let locale_messages = source_dir.join(locale_dir).join(&language).join("LC_MESSAGES");
            CatalogPaths {
                po: locale_messages.join(format!("{domain}.po")),
                mo: locale_messages.join(format!("{domain}.mo")),
                locale_messages,
            }
        })
        .collect()
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok().and_then(|meta| meta.modified().ok())
}

/// Mirrors `CatalogInfo.is_outdated`.
pub fn is_outdated(paths: &CatalogPaths) -> bool {
    match (modified(&paths.mo), modified(&paths.po)) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(mo), Some(po)) => mo < po,
    }
}

/// Compiles a `.po` file into a `.mo` file in place when needed.
///
/// Returns `Ok(true)` when a file was written. Malformed PO files yield a
/// structured error and leave the old `.mo` untouched (matching upstream's
/// warning-and-continue behavior at the call site).
pub fn compile_catalog(paths: &CatalogPaths, allow_fuzzy: bool) -> Result<bool, MoError> {
    if !paths.po.is_file() || !is_outdated(paths) {
        return Ok(false);
    }
    let text = fs::read_to_string(&paths.po).map_err(MoError::Io)?;
    let catalog = PoCatalog::parse(&text);
    let entries = catalog.mo_entries(allow_fuzzy);
    let bytes = write_mo(&entries)?;
    if let Some(parent) = paths.mo.parent() {
        fs::create_dir_all(parent).map_err(MoError::Io)?;
    }
    fs::write(&paths.mo, bytes).map_err(MoError::Io)?;
    Ok(true)
}

/// Compiles every discoverable `.po` catalog for the configured locales.
/// Called by non-gettext builders when `gettext_auto_build` is enabled.
pub fn compile_all(config: &Config, source_dir: &Path) -> Vec<String> {
    let mut messages = Vec::new();
    if !config.get_bool("gettext_auto_build").unwrap_or(true) {
        return messages;
    }
    let allow_fuzzy = config.get_bool("gettext_allow_fuzzy_translations").unwrap_or(false);
    for locale_dir in locale_dirs(config) {
        let messages_dir = source_dir.join(locale_dir).join(language(config)).join("LC_MESSAGES");
        let Ok(entries) = fs::read_dir(&messages_dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("po") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else { continue };
            let paths = CatalogPaths {
                locale_messages: messages_dir.clone(),
                po: path.clone(),
                mo: messages_dir.join(format!("{stem}.mo")),
            };
            match compile_catalog(&paths, allow_fuzzy) {
                Ok(true) => messages.push(format!("compiled {}", paths.po.display())),
                Ok(false) => {}
                Err(error) => messages.push(format!("cannot compile {}: {error}", paths.po.display())),
            }
        }
    }
    messages
}

/// Loads the translation catalog for `domain`, compiling `.po` files on demand
/// when `gettext_auto_build` is enabled.
///
/// Returns `None` when no catalog exists, the language is unset, or the binary
/// catalog could not be decoded. The caller treats that as "no translations".
pub fn load_catalog(config: &Config, source_dir: &Path, domain: &str) -> Option<TranslationCatalog> {
    let paths = catalog_paths(config, source_dir, domain);
    if paths.is_empty() {
        return None;
    }
    let allow_fuzzy = config.get_bool("gettext_allow_fuzzy_translations").unwrap_or(false);
    for path in paths {
        if !path.mo.is_file() && config.get_bool("gettext_auto_build").unwrap_or(true) {
            let _ = compile_catalog(&path, allow_fuzzy);
        }
        let Ok(bytes) = fs::read(&path.mo) else { continue };
        if let Ok(entries) = read_mo(&bytes) {
            return Some(TranslationCatalog::from_entries(
                domain.to_owned(),
                entries.into_iter().filter(|(key, _)| !key.is_empty()),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::{Config, ConfigRegistry};

    fn config_with(values: &[(&str, &str)]) -> Config {
        let registry = ConfigRegistry::builtin();
        let mut config = Config::new();
        for (key, value) in values {
            let coerced = registry.coerce_override(key, value).expect("known key");
            config.set(key, coerced).expect("set");
        }
        config
    }

    #[test]
    fn domain_compaction_matches_upstream() {
        assert_eq!(docname_to_domain("index", "true"), "index");
        assert_eq!(docname_to_domain("guide/install", "true"), "guide");
        assert_eq!(docname_to_domain("guide/install", "false"), "guide/install");
        assert_eq!(docname_to_domain("guide/install", "custom"), "custom");
    }

    #[test]
    fn paths_follow_language_and_locale_dirs() {
        let config = config_with(&[("language", "xx"), ("locale_dirs", "translations")]);
        let paths = catalog_paths(&config, Path::new("/src"), "index");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].po.ends_with("translations/xx/LC_MESSAGES/index.po"));
        assert!(paths[0].mo.ends_with("translations/xx/LC_MESSAGES/index.mo"));
    }

    #[test]
    fn compiles_po_and_loads_translation() {
        let temp = std::env::temp_dir().join(format!("scribe-i18n-catalog-{}", std::process::id()));
        let messages = temp.join("locales/xx/LC_MESSAGES");
        fs::create_dir_all(&messages).expect("create dirs");
        fs::write(
            messages.join("index.po"),
            "msgid \"\"\nmsgstr \"\"\n\"Language: xx\\n\"\n\nmsgid \"Hello\"\nmsgstr \"Hallo\"\n",
        )
        .expect("write po");
        let config = config_with(&[("language", "xx")]);
        let catalog = load_catalog(&config, &temp, "index").expect("catalog");
        assert_eq!(catalog.get("Hello"), Some("Hallo"));
        assert!(messages.join("index.mo").is_file());
        fs::remove_dir_all(&temp).ok();
    }
}
