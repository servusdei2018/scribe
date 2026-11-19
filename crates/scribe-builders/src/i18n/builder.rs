//! The `gettext` message-catalog builder (Phase 26).
//!
//! Mirrors `sphinx/builders/gettext.py::MessageCatalogBuilder`: parse every
//! source with the normal read pipeline, extract translatable messages with
//! source locations, and emit one `.pot` per gettext domain. Existing `.po`
//! catalogs are updated in place with their translations preserved.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Local, Utc};
use scribe_ast::Document;
use scribe_config::Config;
use scribe_core::{clean_extracted_message, is_in_substitution_definition, extract_messages, BuildEnvironment};

use crate::build_common;
use crate::error::{OutputError, OutputResult};
use crate::html;

use super::catalog::{compact_setting, docname_to_domain, locale_dirs};
use super::po::{should_write, Message, PoCatalog, PotContext};

/// Result of a `gettext` build.
#[derive(Clone, Debug)]
pub struct GettextResult {
    /// Documents read during the build.
    pub built_docnames: Vec<String>,
    /// Catalog files written (absolute or relative to the output dir).
    pub files: Vec<PathBuf>,
    /// Non-fatal warnings gathered while preparing the build.
    pub warnings: Vec<String>,
}

impl GettextResult {
    /// Number of emitted catalog files.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// The gettext message-catalog builder.
#[derive(Debug, Default)]
pub struct GettextBuilder;

impl GettextBuilder {
    /// Runs a full `scribe build -b gettext` into `options.output_dir`.
    pub fn build(
        options: &html::BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> OutputResult<GettextResult> {
        let builder = "gettext";
        if !options.source_dir.is_dir() {
            return Err(OutputError::MissingSource(builder, options.source_dir.clone()));
        }
        fs::create_dir_all(&options.output_dir).map_err(|e| OutputError::io(builder, &options.output_dir, e))?;

        let prepared = build_common::prepare(options, config, env, builder)?;
        let compact = compact_setting(config);

        // Domain → catalog. Using a BTreeMap guarantees deterministic output.
        let mut catalogs: BTreeMap<String, PoCatalog> = BTreeMap::new();
        for docname in &prepared.docnames {
            let Some(document) = prepared.parsed.get(docname) else { continue };
            let domain = docname_to_domain(docname, &compact);
            let source_path = env.found_docs.get(docname).cloned();
            let catalog = catalogs.entry(domain).or_default();
            collect_document_messages(document, source_path.as_deref(), &options.output_dir, catalog);
        }
        collect_template_messages(config, &options.source_dir, &options.output_dir, &mut catalogs);

        let context = pot_context(config);
        let mut files = Vec::new();
        for (domain, catalog) in &catalogs {
            let content = catalog.to_pot(&context);
            let path = options.output_dir.join(format!("{domain}.pot"));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| OutputError::io(builder, parent, e))?;
            }
            if !path.is_file() || should_write(&fs::read_to_string(&path).unwrap_or_default(), &content) {
                fs::write(&path, &content).map_err(|e| OutputError::io(builder, &path, e))?;
            }
            files.push(path);

            // Update any existing `.po` catalog in place: translations are
            // preserved, new locations are refreshed. Upstream stops at the
            // `.pot`; this additive behavior keeps translator work across
            // incremental gettext builds (documented in ARCHITECTURE.md §32).
            for po_path in existing_po_paths(config, &options.source_dir, &options.output_dir, domain) {
                if !po_path.is_file() {
                    continue;
                }
                let old = fs::read_to_string(&po_path).map_err(|e| OutputError::io(builder, &po_path, e))?;
                let old_catalog = PoCatalog::parse(&old);
                let updated = catalog.merge_translations(&old_catalog).to_po();
                fs::write(&po_path, updated).map_err(|e| OutputError::io(builder, &po_path, e))?;
                files.push(po_path);
            }
        }

        Ok(GettextResult {
            built_docnames: prepared.docnames,
            files,
            warnings: prepared.warnings,
        })
    }
}

/// Collects messages from one parsed document into `catalog`.
fn collect_document_messages(
    document: &Document,
    source_path: Option<&Path>,
    output_dir: &Path,
    catalog: &mut PoCatalog,
) {
    for message in extract_messages(document) {
        if is_in_substitution_definition(document, message.node) {
            continue;
        }
        let source = source_path
            .map(|path| relative_path(path, output_dir))
            .filter(|rel| !rel.is_empty())
            .unwrap_or(message.source);
        catalog.add(Message {
            msgid: message.msgid,
            locations: vec![(source, message.line)],
            ..Message::default()
        });
    }
}

/// A small, deterministic `os.path.relpath` equivalent for location comments.
fn relative_path(target: &Path, start: &Path) -> String {
    let target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let target_parts: Vec<_> = target.components().collect();
    let start_parts: Vec<_> = start.components().collect();
    let common = target_parts
        .iter()
        .zip(start_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<String> = start_parts[common..]
        .iter()
        .map(|_| "..".to_owned())
        .collect();
    out.extend(target_parts[common..].iter().map(|part| part.as_os_str().to_string_lossy().into_owned()));
    out.join("/")
}

/// Finds existing `.po` files to update for `domain`.
fn existing_po_paths(
    config: &Config,
    source_dir: &Path,
    output_dir: &Path,
    domain: &str,
) -> Vec<PathBuf> {
    let mut paths = vec![output_dir.join(format!("{domain}.po"))];
    for locale_dir in locale_dirs(config) {
        let language = config.get_str("language").unwrap_or("en");
        if language.is_empty() {
            continue;
        }
        paths.push(
            source_dir
                .join(locale_dir)
                .join(language)
                .join("LC_MESSAGES")
                .join(format!("{domain}.po")),
        );
    }
    paths
}

/// Extracts `_('...')` and `{% trans %}...{% endtrans %}` messages from HTML
/// templates, mirroring `MessageCatalogBuilder._extract_from_template`.
fn collect_template_messages(
    config: &Config,
    source_dir: &Path,
    output_dir: &Path,
    catalogs: &mut BTreeMap<String, PoCatalog>,
) {
    let template_dirs = config.get_strings("templates_path").unwrap_or_default();
    for template_dir in template_dirs {
        let root = source_dir.join(&template_dir);
        let Ok(entries) = fs::read_dir(&root) else { continue };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("html"))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let source = relative_path(&path, output_dir);
            for (offset, message) in extract_template_messages(&text) {
                let line = i64::try_from(text[..offset].bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(-1);
                catalogs.entry("sphinx".to_owned()).or_default().add(Message {
                    msgid: clean_extracted_message(&message),
                    locations: vec![(source.clone(), line)],
                    ..Message::default()
                });
            }
        }
    }
}

/// Returns `(byte_offset, message)` for every template translatable string.
///
/// The scanner only inspects UTF-8 character boundaries so a multibyte
/// template string can never trigger a slicing panic.
fn extract_template_messages(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut skip_until = 0usize;
    for (index, _) in text.char_indices() {
        if index < skip_until {
            continue;
        }
        // `{{ _('message') }}`
        if text[index..].starts_with("_(") || text[index..].starts_with("_ (") {
            let after_paren = text[index..].find('(').map(|pos| index + pos + 1).unwrap_or(index);
            if let Some((quote_pos, quote)) = find_quote(&text[after_paren..]) {
                let body_start = after_paren + quote_pos + 1;
                if let Some(end) = find_closing_quote(text, body_start, quote) {
                    out.push((index, text[body_start..end].to_owned()));
                    skip_until = end + 1;
                    continue;
                }
            }
        }
        // `{% trans %}message{% endtrans %}`
        if text[index..].starts_with("{%") {
            if let Some(close) = text[index..].find("%}") {
                let tag_inner = text[index + 2..index + close].trim();
                if tag_inner == "trans" || tag_inner == "translate" {
                    let body_start = index + close + 2;
                    if let Some(end_rel) = text[body_start..].find("{%") {
                        let end_tag = &text[body_start + end_rel..];
                        if end_tag.contains("endtrans") {
                            out.push((index, text[body_start..body_start + end_rel].trim().to_owned()));
                            skip_until = body_start + end_rel + 2;
                            continue;
                        }
                    }
                }
            }
        }
    }
    out
}

fn find_quote(text: &str) -> Option<(usize, char)> {
    let start = text.find(['\'', '"'])?;
    Some((start, text[start..].chars().next()?))
}

fn find_closing_quote(text: &str, start: usize, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some(start + offset);
        }
    }
    None
}

/// Builds the POT header context from configuration.
fn pot_context(config: &Config) -> PotContext {
    let project = config.get_str("project").filter(|s| !s.is_empty()).unwrap_or("Project name not set").to_owned();
    let version = config.get_str("version").unwrap_or("").to_owned();
    let copyright = config
        .get_str("copyright")
        .or_else(|| config.get_str("project_copyright"))
        .unwrap_or("")
        .to_owned();
    PotContext {
        project,
        version,
        copyright,
        ctime: creation_date(),
        last_translator: config
            .get_str("gettext_last_translator")
            .unwrap_or("FULL NAME <EMAIL@ADDRESS>")
            .to_owned(),
        language_team: config
            .get_str("gettext_language_team")
            .unwrap_or("LANGUAGE <LL@li.org>")
            .to_owned(),
        display_location: config.get_bool("gettext_location").unwrap_or(true),
        display_uuid: config.get_bool("gettext_uuid").unwrap_or(false),
    }
}

/// POT-Creation-Date. Honors `SOURCE_DATE_EPOCH` for reproducible builds and
/// otherwise uses local time, matching upstream's module-level `ctime`.
fn creation_date() -> String {
    if let Ok(epoch) = std::env::var("SOURCE_DATE_EPOCH") {
        if let Ok(seconds) = epoch.trim().parse::<i64>() {
            if let Some(date) = chrono::DateTime::from_timestamp(seconds, 0) {
                return date.with_timezone(&Utc).format("%Y-%m-%d %H:%M%z").to_string();
            }
        }
    }
    Local::now().format("%Y-%m-%d %H:%M%z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_extraction_finds_both_forms() {
        let text = "<h1>{{ _('Title') }}</h1>\n<p>{%trans%}Body text{%endtrans%}</p>\n";
        let messages = extract_template_messages(text);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].1, "Title");
        assert_eq!(messages[1].1, "Body text");
        assert!(messages[0].0 < messages[1].0);
        assert_eq!(&text[messages[0].0..messages[0].0 + 2], "_(");
    }

    #[test]
    fn template_extraction_ignores_unterminated_strings() {
        let text = "{{ _('unterminated) }}\n";
        assert!(extract_template_messages(text).is_empty());
    }
}
