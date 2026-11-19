//! `sphinx.ext.extlinks` configuration parsing and hardcoded-link diagnostics.

use std::collections::BTreeMap;

use scribe_ast::{Document, Node, NodeId, NodeKind};
use scribe_config::{Config, ConfigValue};
use scribe_rst::ExtLinkSpec;

/// Parse the `extlinks` config mapping into role specifications.
///
/// Upstream expects `{'name': (base_url, caption)}`. Entries with an
/// unexpected shape are skipped and reported; they never abort the build.
#[must_use]
pub fn parse_extlinks(config: &Config) -> (BTreeMap<String, ExtLinkSpec>, Vec<String>) {
    let mut specs = BTreeMap::new();
    let mut warnings = Vec::new();
    let Some(entries) = config.get_dict("extlinks") else {
        return (specs, warnings);
    };
    for (name, value) in entries {
        let pair = match value {
            ConfigValue::List(items) if items.len() == 2 => items,
            ConfigValue::Str(base) => {
                // Tolerate the legacy shorthand `{'name': 'https://x/%s'}`.
                let spec = ExtLinkSpec::new(name.clone(), base.clone(), None);
                warnings.extend(spec.validate());
                specs.insert(name.clone(), spec);
                continue;
            }
            _ => {
                warnings.push(format!(
                    "invalid extlinks entry '{name}': expected a (base_url, caption) pair"
                ));
                continue;
            }
        };
        let ConfigValue::Str(base_url) = &pair[0] else {
            warnings.push(format!("invalid extlinks entry '{name}': base URL must be a string"));
            continue;
        };
        let caption = match &pair[1] {
            ConfigValue::None => None,
            ConfigValue::Str(text) => Some(text.clone()),
            other => {
                warnings.push(format!(
                    "invalid extlinks entry '{name}': caption must be a string or None, got {other:?}"
                ));
                continue;
            }
        };
        let spec = ExtLinkSpec::new(name.clone(), base_url.clone(), caption);
        warnings.extend(spec.validate());
        specs.insert(name.clone(), spec);
    }
    (specs, warnings)
}

/// Emit upstream's `hardcoded link ... could be replaced by an extlink`
/// warnings for external `Reference` nodes matching an extlink base URL.
///
/// This mirrors `ExternalLinksChecker` (priority 500), gated by
/// `extlinks_detect_hardcoded_links`. The returned strings are already
/// formatted as Sphinx warning messages.
#[must_use]
pub fn hardcoded_link_warnings(
    config: &Config,
    docs: &BTreeMap<String, Document>,
) -> Vec<String> {
    if !config.get_bool("extlinks_detect_hardcoded_links").unwrap_or(false) {
        return Vec::new();
    }
    let (specs, _) = parse_extlinks(config);
    if specs.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    for (docname, doc) in docs {
        for id in doc.preorder_ids() {
            let Ok(node) = doc.get(id) else { continue };
            // Upstream checks `'internal' in refnode` — *presence*, not truth.
            // Roles such as extlinks set `internal=False` explicitly, which
            // must also be excluded from hardcoded-link warnings.
            if node.kind != NodeKind::Reference || node.attrs.contains_key("internal") {
                continue;
            }
            let Some(uri) = node.attrs.get_str("refuri").ok().flatten() else { continue };
            let title = node_text(doc, id);
            for (alias, spec) in &specs {
                if let Some(value) = match_base(&spec.base_url, uri) {
                    if !value.is_empty() && !value.contains('/') {
                        let replacement = if uri != title {
                            format!(":{alias}:`{title} <{value}>`")
                        } else {
                            format!(":{alias}:`{value}`")
                        };
                        warnings.push(format!(
                            "{docname}: hardcoded link '{uri}' could be replaced by an extlink (try using '{replacement}' instead)"
                        ));
                    }
                }
            }
        }
    }
    warnings
}

fn node_text(doc: &Document, id: NodeId) -> String {
    let Ok(node) = doc.get(id) else { return String::new() };
    let mut out = String::new();
    collect_text(doc, node, &mut out);
    out
}

fn collect_text(doc: &Document, node: &Node, out: &mut String) {
    if node.kind == NodeKind::Text {
        if let Ok(Some(text)) = node.attrs.get_str("text") {
            out.push_str(text);
        }
    }
    for &child in &node.children {
        if let Ok(child_node) = doc.get(child) {
            collect_text(doc, child_node, out);
        }
    }
}

/// Match `uri` against a base template whose only placeholder is `%s`.
fn match_base(base: &str, uri: &str) -> Option<String> {
    let parts: Vec<&str> = base.split("%s").collect();
    if parts.len() == 1 {
        return (base == uri).then(String::new);
    }
    let mut rest = uri;
    let mut captured = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            rest = rest.strip_prefix(part)?;
        } else if index + 1 == parts.len() {
            captured = rest.strip_suffix(part)?.to_owned();
            rest = "";
        } else {
            captured = rest.strip_prefix(part)?.to_owned();
            // Multiple placeholders: upstream's `(?P<value>.+)` only
            // captures the first, so we stop after a successful first match.
            return Some(captured);
        }
    }
    Some(captured)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extlinks: &[(&str, &str, Option<&str>)], detect: bool) -> Config {
        let mut config = Config::new();
        let mut entries = BTreeMap::new();
        for (name, base, caption) in extlinks {
            entries.insert(
                (*name).to_owned(),
                ConfigValue::List(vec![
                    ConfigValue::from(*base),
                    caption.map_or(ConfigValue::None, ConfigValue::from),
                ]),
            );
        }
        config.set("extlinks", ConfigValue::Dict(entries)).unwrap();
        config.set("extlinks_detect_hardcoded_links", ConfigValue::Bool(detect)).unwrap();
        config
    }

    #[test]
    fn parses_valid_and_invalid_entries() {
        let config = config(&[("issue", "https://x/%s", Some("issue %s")), ("bad", "https://x/%d", None)], false);
        let (specs, warnings) = parse_extlinks(&config);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs["issue"].resolve("7").0, "https://x/7");
        assert!(warnings.iter().any(|warning| warning.contains("invalid")));
    }

    #[test]
    fn match_base_handles_single_and_repeated_placeholders() {
        assert_eq!(match_base("https://x/%s.html", "https://x/1.html").as_deref(), Some("1"));
        assert_eq!(match_base("https://github.com/%s", "https://github.com/octocat").as_deref(), Some("octocat"));
        assert_eq!(match_base("https://x/%s", "https://y/1"), None);
    }
}
