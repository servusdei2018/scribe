//! `sphinx.ext.viewcode`: collect Python objects, inject `[source]` links and
//! describe the `_modules/` source pages emitted by the HTML builder.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::Config;

/// One module whose source is available for a viewcode page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewcodeModule {
    /// Dotted module name (`spam.mod1`).
    pub name: String,
    /// Absolute path of the Python source file.
    pub source_path: PathBuf,
    /// Raw source text.
    pub code: String,
    /// `fullname -> (start_line, end_line)` zero-based, inclusive.
    pub tags: BTreeMap<String, (usize, usize)>,
}

impl ViewcodeModule {
    /// Output page path relative to the build directory (`_modules/spam/mod1.html`).
    #[must_use]
    pub fn page_path(&self) -> String {
        module_page_path(&self.name)
    }

    /// Anchor id used for a definition on the generated page.
    #[must_use]
    pub fn anchor(&self, fullname: &str) -> String {
        fullname.to_owned()
    }
}

/// Output page path for a dotted module name.
#[must_use]
pub fn module_page_path(name: &str) -> String {
    format!("_modules/{}.html", name.replace('.', "/"))
}

/// Collect the modules referenced by Python object descriptions.
#[derive(Clone, Debug, Default)]
pub struct ViewcodeCollector {
    modules: BTreeMap<String, ViewcodeModule>,
    /// Non-fatal diagnostics (unreadable source files).
    pub warnings: Vec<String>,
}

impl ViewcodeCollector {
    /// Scan a parsed document, inject `[source]` links into matching signatures
    /// and remember every referenced module's source.
    pub fn collect(
        &mut self,
        docname: &str,
        doc: &mut Document,
        config: &Config,
        source_dir: &Path,
    ) {
        // Pure-Rust builds always use local source files; `viewcode_import`
        // is accepted by the config registry but needs no runtime action here.
        let _ = config;
        let ids: Vec<NodeId> = doc.preorder_ids();
        let mut linked: BTreeSet<String> = BTreeSet::new();
        let mut current_module = String::new();
        for node_id in ids {
            let Ok(node) = doc.get(node_id) else { continue };
            if node.kind == NodeKind::Target
                && node.attrs.get_bool("ismod").ok().flatten().unwrap_or(false)
            {
                current_module = node
                    .attrs
                    .get_str("py:signature")
                    .ok()
                    .flatten()
                    .map(str::to_owned)
                    .or_else(|| {
                        node.ids()
                            .ok()
                            .and_then(|ids| ids.first().cloned())
                            .and_then(|id| id.strip_prefix("module-").map(str::to_owned))
                    })
                    .unwrap_or_default();
                continue;
            }
            if node.kind != NodeKind::Desc
                || node.attrs.get_str("domain").ok().flatten() != Some("py")
            {
                continue;
            }
            let signatures: Vec<NodeId> = node.children.clone();
            for signature_id in signatures {
                if !doc.get(signature_id).is_ok_and(|node| node.kind == NodeKind::DescSignature) {
                    continue;
                }
                let (module, fullname) = {
                    let Ok(signature) = doc.get(signature_id) else { continue };
                    (
                        signature.attrs.get_str("module").ok().flatten().unwrap_or("").to_owned(),
                        signature.attrs.get_str("fullname").ok().flatten().unwrap_or("").to_owned(),
                    )
                };
                let module = if module.is_empty() { current_module.clone() } else { module };
                if fullname.is_empty() {
                    continue;
                }
                // Scribe's Python-domain transform can leave `module` empty
                // for `.. py:module::` context; fall back to locating the
                // definition by scanning local Python sources.
                let module = resolve_module(source_dir, &module)
                    .or_else(|| infer_module_for_name(source_dir, &fullname));
                let module = match module {
                    Some(name) => name,
                    None => continue,
                };
                if !self.modules.contains_key(&module) {
                    if let Some(source) = source_for_module(source_dir, &module) {
                        if let Ok(code) = fs::read_to_string(&source) {
                            let tags = parse_tags(&code);
                            self.modules.insert(
                                module.clone(),
                                ViewcodeModule {
                                    name: module.clone(),
                                    source_path: source,
                                    code,
                                    tags,
                                },
                            );
                        } else {
                            self.warnings.push(format!("{docname}: viewcode cannot read source for {module}"));
                        }
                    }
                }
                let has_tag = self
                    .modules
                    .get(&module)
                    .is_some_and(|entry| entry.tags.contains_key(&fullname));
                if !has_tag || !linked.insert(fullname.clone()) {
                    continue;
                }
                inject_source_link(doc, signature_id, &module, &fullname);
            }
        }
    }

    /// All collected modules, in deterministic (dotted-name) order.
    #[must_use]
    pub fn modules(&self) -> &BTreeMap<String, ViewcodeModule> {
        &self.modules
    }

    /// Whether any module page is needed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}


/// Infer the defining module by scanning `.py` files below `source_dir` for a
/// definition named `fullname`. Only the first deterministic match is used.
fn infer_module_for_name(source_dir: &Path, fullname: &str) -> Option<String> {
    let mut files = Vec::new();
    collect_python_files(source_dir, &mut files);
    for path in files {
        let Ok(code) = fs::read_to_string(&path) else { continue };
        if !parse_tags(&code).contains_key(fullname) {
            continue;
        }
        let relative = path.strip_prefix(source_dir).ok()?;
        let mut parts: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        let last = parts.last()?.clone();
        if last == "__init__.py" {
            parts.pop();
        } else if last.ends_with(".py") {
            let stem = last.trim_end_matches(".py").to_owned();
            *parts.last_mut()? = stem;
        } else {
            continue;
        }
        if parts.is_empty() {
            continue;
        }
        return Some(parts.join("."));
    }
    None
}

fn collect_python_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut children: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    children.sort();
    for path in children {
        if path.is_dir() {
            collect_python_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("py") {
            out.push(path);
        }
    }
}

fn inject_source_link(doc: &mut Document, signature: NodeId, module: &str, fullname: &str) {
    let page = module_page_path(module);
    let Ok(link) = doc.add(NodeKind::Reference, Some(signature)) else { return };
    if let Ok(node) = doc.get_mut(link) {
        node.attrs.set_bool("internal", true);
        node.attrs.set_str("refuri", format!("{page}#{fullname}"));
    }
    let Ok(inner) = doc.add(NodeKind::Inline, Some(link)) else { return };
    if let Ok(node) = doc.get_mut(inner) {
        node.attrs.set_list("classes", vec!["viewcode-link".to_owned()]);
    }
    let _ = doc.new_text("[source]", Some(inner));
}

/// Find the importable module name for a dotted name by probing the source tree.
fn resolve_module(source_dir: &Path, module: &str) -> Option<String> {
    if source_for_module(source_dir, module).is_some() {
        return Some(module.to_owned());
    }
    // Upstream's `viewcode_follow_imported_members` resolves the *defining*
    // module. Without a Python interpreter, fall back to progressively
    // stripping the rightmost component (e.g. `spam.mod1.Class1` → `spam.mod1`).
    let mut candidate = module.to_owned();
    while let Some((parent, _)) = candidate.rsplit_once('.') {
        candidate = parent.to_owned();
        if source_for_module(source_dir, &candidate).is_some() {
            return Some(candidate);
        }
    }
    None
}

fn source_for_module(source_dir: &Path, module: &str) -> Option<PathBuf> {
    let relative = module.replace('.', "/");
    let as_file = source_dir.join(format!("{relative}.py"));
    if as_file.is_file() {
        return Some(as_file);
    }
    let as_package = source_dir.join(&relative).join("__init__.py");
    if as_package.is_file() {
        return Some(as_package);
    }
    None
}

/// Extract top-level/nested `def`, `async def` and `class` tags from Python source.
#[must_use]
pub fn parse_tags(code: &str) -> BTreeMap<String, (usize, usize)> {
    let lines: Vec<&str> = code.lines().collect();
    let mut starts: Vec<(usize, usize, String)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let after_keyword = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "))
            .or_else(|| trimmed.strip_prefix("class "));
        let Some(rest) = after_keyword else { continue };
        let name: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        starts.push((indent, index, name));
    }
    let mut tags = BTreeMap::new();
    for position in 0..starts.len() {
        let (indent, start, name) = &starts[position];
        let end = starts
            .iter()
            .skip(position + 1)
            .find(|(next_indent, _, _)| next_indent <= indent)
            .map_or(lines.len().saturating_sub(1), |(_, next_start, _)| next_start.saturating_sub(1))
            .max(*start);
        // Upstream's `ModuleAnalyzer` records the fully qualified tag name for
        // nested definitions; the dotted prefix from the signature supplies
        // that context, so plain names are sufficient here.
        tags.insert(name.clone(), (*start, end));
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tags_finds_functions_and_classes() {
        let code = "def one():\n    pass\n\nclass Two:\n    def method(self):\n        pass\n\ndef three():\n    pass\n";
        let tags = parse_tags(code);
        assert_eq!(tags["one"].0, 0);
        assert_eq!(tags["one"].1, 2);
        assert_eq!(tags["Two"].1, 6);
        assert_eq!(tags["method"].0, 4);
        assert_eq!(tags["three"].0, 7);
    }

    #[test]
    fn module_page_paths_are_deterministic() {
        assert_eq!(module_page_path("spam.mod1"), "_modules/spam/mod1.html");
    }
}
