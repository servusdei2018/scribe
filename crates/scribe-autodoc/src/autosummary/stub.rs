//! Stub-page generation for `.. autosummary::` (Phase 22).
//!
//! Upstream generates stubs in a pre-read pass (`generate_autosummary_docs`),
//! scanning source files for `.. autosummary::` blocks with a `:toctree:`
//! option, importing every listed object and rendering a template. Scribe uses
//! the Phase 21 static model instead of importing Python, but follows the same
//! scan → resolve → render → write → recurse pipeline. Stub paths use Sphinx's
//! `posixpath.normpath(dirname(prefix/name))` rule and `autosummary_filename_map`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use minijinja::value::Value;
use scribe_config::Config;

use crate::discovery::{discover_package, search_roots, resolve_module};

use super::render::StubRenderer;
use super::table::{filename_map, is_exception, parse_names, resolve_permissive};

/// Result of one stub-generation pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StubReport {
    /// Paths of stubs written or updated (relative to the source dir when
    /// possible, otherwise absolute).
    pub generated: Vec<PathBuf>,
    /// Non-fatal diagnostics, in deterministic order.
    pub warnings: Vec<String>,
}

/// One autosummary directive found in a source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FoundDirective {
    toctree: Option<String>,
    template: Option<String>,
    recursive: bool,
    names: Vec<String>,
    /// `.. currentmodule::` seen before the directive (used to qualify bare
    /// submodule names in generated stub files, like upstream's ref_context).
    current_module: Option<String>,
}

/// Generates all autosummary stubs for a build.
///
/// `docnames` selects source files when non-empty; when empty every `.rst`
/// file under `source_dir` is scanned. Generated files are rescanned so that
/// `:recursive:` module trees produce stubs for their submodules, mirroring
/// upstream's recursive call in `generate_autosummary_docs`.
#[must_use]
pub fn generate_stubs(config: &Config, source_dir: &Path, docnames: &[String]) -> StubReport {
    let mut report = StubReport::default();
    if !source_dir.is_dir() {
        return report;
    }
    let roots = search_roots(Some(source_dir), Some(config));
    let renderer = StubRenderer::new(template_dirs(config, source_dir));
    let names = filename_map(Some(config));
    let encoding = config.get_str("source_encoding").unwrap_or("utf-8-sig").to_ascii_lowercase();
    let respect_all = !config.get_bool("autosummary_ignore_module_all").unwrap_or(false);
    let imported = config.get_bool("autosummary_imported_members").unwrap_or(false);

    let mut sources: Vec<PathBuf> = if docnames.is_empty() {
        walk_rst(source_dir)
    } else {
        docnames.iter().map(|docname| source_dir.join(format!("{docname}.rst"))).collect()
    };
    sources.retain(|path| path.is_file());
    sources.sort();

    let mut seen: BTreeSet<PathBuf> = sources.iter().cloned().collect();
    let mut depth = 0usize;
    loop {
        let mut new_files = Vec::new();
        for source in &sources {
            let content = match fs::read_to_string(source) {
                Ok(content) => content,
                Err(error) => {
                    report.warnings.push(format!(
                        "autosummary: cannot read {}: {error}",
                        source.display()
                    ));
                    continue;
                }
            };
            for directive in find_directives(&content) {
                let Some(prefix) = directive.toctree.as_deref() else { continue };
                let mut visited = BTreeSet::new();
                for name in &directive.names {
                    let bare = name.trim_start_matches('~').to_owned();
                    // Upstream qualifies every bare name with the enclosing
                    // `currentmodule`/`automodule` before importing it.
                    let lookup = match &directive.current_module {
                        Some(module) if !bare.starts_with(&format!("{module}.")) => {
                            format!("{module}.{bare}")
                        }
                        _ => bare.clone(),
                    };
                    let resolved = resolve_permissive(&lookup, &roots);
                    let Some((module, object_path)) = resolved else {
                        report.warnings.push(format!("[autosummary] failed to import {name}"));
                        continue;
                    };
                    let real_name = if object_path.is_empty() {
                        module.name.clone()
                    } else {
                        bare.clone()
                    };
                    let stub_name = names.get(&real_name).cloned().unwrap_or_else(|| real_name.clone());
                    let dirname = source.parent().unwrap_or(source_dir);
                    let stub_dir = join_docname(dirname, prefix);
                    let stub_path = stub_dir.join(format!("{stub_name}.rst"));
                    if !visited.insert(stub_path.clone()) {
                        continue;
                    }
                    let vars = render_context(
                        &module,
                        &object_path,
                        &real_name,
                        directive.recursive,
                        respect_all,
                        imported,
                        &roots,
                    );
                    let objtype = vars.get("objtype").and_then(Value::as_str).unwrap_or("base");
                    let rendered = match renderer.render(directive.template.as_deref(), objtype, &vars) {
                        Ok(rendered) => rendered,
                        Err(error) => {
                            report.warnings.push(format!("autosummary: {error}"));
                            continue;
                        }
                    };
                    if let Err(error) = write_stub(&stub_path, &rendered, &encoding) {
                        report.warnings.push(format!(
                            "autosummary: cannot write {}: {error}",
                            stub_path.display()
                        ));
                        continue;
                    }
                    report.generated.push(stub_path.clone());
                    if directive.recursive && seen.insert(stub_path.clone()) {
                        new_files.push(stub_path);
                    }
                }
            }
        }
        if new_files.is_empty() || depth >= 8 {
            break;
        }
        sources = new_files;
        sources.sort();
        depth += 1;
    }
    report.generated.sort();
    report.generated.dedup();
    report
}

fn walk_rst(source_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(source_dir).sort_by_file_name().into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else { continue };
        if extension != "rst" {
            continue;
        }
        let relative = path.strip_prefix(source_dir).unwrap_or(path);
        let excluded = relative.components().any(|part| {
            matches!(part.as_os_str().to_str(), Some("_build" | ".doctrees" | "_templates" | ".scribe-cache"))
        });
        if excluded {
            continue;
        }
        out.push(path.to_path_buf());
    }
    out
}

fn template_dirs(config: &Config, source_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![source_dir.to_path_buf()];
    if let Some(paths) = config.get_strings("templates_path") {
        for path in paths {
            let candidate = PathBuf::from(path);
            let resolved = if candidate.is_absolute() { candidate } else { source_dir.join(candidate) };
            if !dirs.contains(&resolved) {
                dirs.push(resolved);
            }
        }
    }
    dirs
}

/// Lightweight scan of `.. autosummary::` blocks, transcribed from upstream's
/// `find_autosummary_in_lines` (option lines, then indented content names).
fn find_directives(content: &str) -> Vec<FoundDirective> {
    let mut out = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut current_module: Option<String> = None;
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if let Some(module) = trimmed
            .strip_prefix(".. currentmodule::")
            .or_else(|| trimmed.strip_prefix(".. automodule::"))
        {
            current_module = Some(module.trim().to_owned());
            index += 1;
            continue;
        }
        if !trimmed.starts_with(".. autosummary::") {
            index += 1;
            continue;
        }
        let base_indent = lines[index].len() - lines[index].trim_start().len();
        let mut directive = FoundDirective::default();
        index += 1;
        while index < lines.len() {
            let line = lines[index];
            let trimmed = line.trim();
            if trimmed.is_empty() {
                index += 1;
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            if indent <= base_indent {
                break;
            }
            if let Some(option) = trimmed.strip_prefix(':') {
                if let Some((key, value)) = option.split_once(':') {
                    let value = value.trim();
                    match key.trim() {
                        "toctree" => directive.toctree = Some(value.to_owned()),
                        "template" => directive.template = Some(value.to_owned()),
                        "recursive" => directive.recursive = true,
                        _ => {}
                    }
                    index += 1;
                    continue;
                }
            }
            break;
        }
        let mut body = Vec::new();
        while index < lines.len() {
            let line = lines[index];
            if line.trim().is_empty() {
                index += 1;
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            if indent <= base_indent {
                break;
            }
            body.push(line.trim().to_owned());
            index += 1;
        }
        directive.names = parse_names(&body.join("\n"));
        directive.current_module = current_module.clone();
        if directive.toctree.is_some() {
            out.push(directive);
        }
    }
    out
}

fn join_docname(dirname: &Path, prefix: &str) -> PathBuf {
    let mut out = dirname.to_path_buf();
    for part in prefix.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => { out.pop(); }
            other => out.push(other),
        }
    }
    out
}

fn write_stub(path: &Path, content: &str, encoding: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = Vec::with_capacity(content.len() + 3);
    if encoding == "utf-8-sig" {
        bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
    }
    bytes.extend_from_slice(content.as_bytes());
    fs::write(path, bytes)
}

/// Builds the Jinja context for one object. Mirrors upstream
/// `generate_autosummary_content`: `members`, `functions`, `classes`,
/// `exceptions`, `attributes`, `methods` plus the six scalar names.
fn render_context(
    module: &crate::model::ModuleInfo,
    object_path: &str,
    real_name: &str,
    recursive: bool,
    respect_all: bool,
    imported: bool,
    roots: &[PathBuf],
) -> BTreeMap<String, Value> {
    use crate::model::Member;

    let mut vars = BTreeMap::new();
    let module_name = module.name.clone();
    let (objtype, qualname) = if object_path.is_empty() {
        ("module", real_name.to_owned())
    } else {
        (object_kind(module, object_path), object_path.to_owned())
    };
    let all = if respect_all { module.all.as_deref() } else { None };
    let allowed = |name: &str| all.is_none_or(|list| list.iter().any(|candidate| candidate == name));
    // Upstream flips `imported_members` on when the module defines `__all__`
    // (and `autosummary_ignore_module_all` is off), so its non-`__all__` public
    // attributes still appear in the generated module stub.
    let imported = imported || (respect_all && module.all.is_some());

    let mut functions = Vec::new();
    let mut classes = Vec::new();
    let mut exceptions = Vec::new();
    let mut attributes = Vec::new();
    let mut members = Vec::new();
    let mut methods = Vec::new();
    let mut class_attributes = Vec::new();
    let mut submodules_in_imports = Vec::new();

    if objtype == "module" {
        // Imported names listed in `__all__` are visible through `dir(obj)`
        // upstream and therefore appear in the stub even though the static
        // parser keeps them in `module.imports` rather than `module.members`.
        let mut imported_names: Vec<(String, Option<crate::model::Member>)> = Vec::new();
        for import in &module.imports {
            let imported_module = if import.level > 0 {
                let mut parts: Vec<&str> = module_name.split('.').collect();
                for _ in 1..import.level {
                    parts.pop();
                }
                let base = parts.join(".");
                let stripped = import.module.trim_start_matches('.');
                if stripped.is_empty() {
                    base
                } else if base.is_empty() {
                    stripped.to_owned()
                } else {
                    format!("{base}.{stripped}")
                }
            } else {
                import.module.clone()
            };
            let loaded = resolve_module(&imported_module, roots)
                .and_then(|path| crate::directives::load_cached(&imported_module, &path));
            for (name, alias) in &import.names {
                let local = alias.as_deref().unwrap_or(name).to_owned();
                let member = loaded.as_ref().and_then(|module| module.find(name).cloned());
                imported_names.push((local, member));
            }
        }
        // Preserve upstream ordering: `__all__` order first (public names
        // only), then documented non-`__all__` attributes in source order.
        let mut ordered: Vec<(String, Option<Member>)> = Vec::new();
        if let Some(all) = all {
            for name in all {
                if name.starts_with('_') {
                    continue;
                }
                let member = module
                    .members
                    .iter()
                    .find(|member| member.name() == name)
                    .cloned()
                    .or_else(|| {
                        imported_names
                            .iter()
                            .find(|(candidate, _)| candidate == name)
                            .and_then(|(_, member)| member.clone())
                    });
                if member.is_some() {
                    ordered.push((name.clone(), member));
                } else if resolve_module(&format!("{module_name}.{name}"), roots).is_some()
                    && !submodules_in_imports.contains(name)
                {
                    submodules_in_imports.push(name.clone());
                }
            }
            for member in &module.members {
                if let Member::Attribute(info) = member {
                    let name = member.name();
                    if !all.iter().any(|candidate| candidate == name)
                        && (documented_attribute(info.docstring.as_deref())
                            || has_attribute_comment(&module.path, info.span))
                    {
                        ordered.push((name.to_owned(), Some(member.clone())));
                    }
                }
            }
        } else {
            for member in &module.members {
                if member.is_public() {
                    ordered.push((member.name().to_owned(), Some(member.clone())));
                }
            }
            for (name, member) in imported_names {
                if member.is_some() && !ordered.iter().any(|(existing, _)| existing == &name) {
                    ordered.push((name, member));
                }
            }
        }
        let _ = imported;
        for (name, member) in ordered {
            members.push(name.clone());
            match member {
                Some(Member::Function(_)) => functions.push(name),
                Some(Member::Class(class)) if is_exception(&class) => exceptions.push(name),
                Some(Member::Class(_)) => classes.push(name),
                Some(Member::Attribute(_)) => attributes.push(name),
                None => {}
            }
        }
    } else if objtype == "class" {
        let mut seen_classes = BTreeSet::new();
        collect_class_members(
            module,
            object_path,
            roots,
            &mut methods,
            &mut class_attributes,
            &mut seen_classes,
        );
        // `dir()` always exposes `__init__`, and instance attributes assigned
        // as `self.name = ...` are visible to the dynamic documenter though
        // the static model cannot put them in `class.members`.
        if !methods.iter().any(|name| name == "__init__") {
            methods.push("__init__".to_owned());
        }
        for name in instance_attributes(module, object_path) {
            if !class_attributes.contains(&name) {
                class_attributes.push(name);
            }
        }
        methods.sort();
        class_attributes.sort();
    }

    // Recursive packages: list public submodules that are not shadowed by a
    // non-module member, as upstream's `_get_modules` does. Explicitly
    // imported modules (e.g. via `__all__`) are always listed.
    let mut submodules = submodules_in_imports;
    if objtype == "module" && recursive {
        if let Some(path) = resolve_module(&module_name, roots) {
            if path.file_name().is_some_and(|name| name == "__init__.py" || name == "__init__.pyi") {
                if let Some(dir) = path.parent() {
                    for (name, _) in discover_package(&module_name, dir) {
                        if name == module_name {
                            continue;
                        }
                        let short = name.strip_prefix(&format!("{module_name}.")).unwrap_or(&name);
                        if !short.contains('.') && allowed(short) && !submodules.iter().any(|name| name == short) {
                            submodules.push(short.to_owned());
                        }
                    }
                }
            }
        }
    }

    let shortname = if objtype == "class" {
        qualname.clone()
    } else {
        qualname.rsplit('.').next().unwrap_or(&qualname).to_owned()
    };
    vars.insert("fullname".to_owned(), Value::from(real_name));
    vars.insert("module".to_owned(), Value::from(module_name.as_str()));
    vars.insert("objname".to_owned(), Value::from(qualname.as_str()));
    vars.insert("name".to_owned(), Value::from(shortname.as_str()));
    vars.insert("objtype".to_owned(), Value::from(objtype));
    vars.insert("underline".to_owned(), Value::from("=".repeat(real_name.chars().count())));
    vars.insert("members".to_owned(), Value::from(members));
    vars.insert("functions".to_owned(), Value::from(functions.clone()));
    vars.insert("all_functions".to_owned(), Value::from(functions));
    vars.insert("classes".to_owned(), Value::from(classes.clone()));
    vars.insert("all_classes".to_owned(), Value::from(classes));
    vars.insert("exceptions".to_owned(), Value::from(exceptions.clone()));
    vars.insert("all_exceptions".to_owned(), Value::from(exceptions));
    let effective_attributes = if objtype == "class" { class_attributes } else { attributes };
    vars.insert("attributes".to_owned(), Value::from(effective_attributes.clone()));
    vars.insert("all_attributes".to_owned(), Value::from(effective_attributes));
    vars.insert("methods".to_owned(), Value::from(methods.clone()));
    vars.insert("all_methods".to_owned(), Value::from(methods));
    vars.insert("modules".to_owned(), Value::from(submodules));
    vars
}

/// Collects methods/attributes from a class and its statically resolvable base
/// classes, in the same alphabetical shape upstream gets from `dir(obj)`.
fn collect_class_members(
    module: &crate::model::ModuleInfo,
    object_path: &str,
    roots: &[PathBuf],
    methods: &mut Vec<String>,
    attributes: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) {
    use crate::model::Member;
    let Some(Member::Class(class)) = module.find(object_path) else { return };
    let identity = format!("{}::{}", module.name, class.qualname);
    if !seen.insert(identity) {
        return;
    }
    for base in &class.bases {
        let short = base.rsplit('.').next().unwrap_or(base).trim();
        let owner = module
            .imports
            .iter()
            .find_map(|import| {
                import.names.iter().find_map(|(name, alias)| {
                    if alias.as_deref().unwrap_or(name) == short {
                        if import.level == 0 && !import.module.is_empty() {
                            Some(import.module.clone())
                        } else {
                            let parts: Vec<&str> = module.name.split('.').collect();
                            let mut parent = parts[..parts.len().saturating_sub(1).max(1)].to_vec();
                            for _ in 1..import.level {
                                parent.pop();
                            }
                            parent.extend(import.module.split('.'));
                            Some(parent.join("."))
                        }
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| module.name.clone());
        let Some(path) = crate::discovery::resolve_module(&owner, roots) else { continue };
        let Some(base_module) = crate::directives::load_cached(&owner, &path) else { continue };
        collect_class_members(&base_module, short, roots, methods, attributes, seen);
        for name in instance_attributes(&base_module, short) {
            if !attributes.contains(&name) {
                attributes.push(name);
            }
        }
    }
    for member in &class.members {
        let name = member.name();
        if !member.is_public() && name != "__init__" {
            continue;
        }
        match member {
            Member::Function(info) if info.kind == crate::model::FunctionKind::Property => {
                attributes.push(name.to_owned());
            }
            Member::Function(_) => methods.push(name.to_owned()),
            Member::Attribute(_) => attributes.push(name.to_owned()),
            Member::Class(_) => {}
        }
    }
    methods.sort();
    methods.dedup();
    attributes.sort();
    attributes.dedup();
}

/// Phase 21 synthesizes `alias of X` docstrings for bare assignments; those
/// are not real documentation and upstream omits them from module attribute
/// summaries.
fn documented_attribute(doc: Option<&str>) -> bool {
    doc.is_some_and(|doc| !doc.trim_start().starts_with("alias of "))
}

/// Whether an assignment is preceded by a Sphinx `#:` attribute comment.
fn has_attribute_comment(path: &std::path::Path, span: crate::model::SourceSpan) -> bool {
    if !span.is_valid() {
        return false;
    }
    let Ok(source) = fs::read_to_string(path) else { return false };
    let before = &source[..(span.start as usize).min(source.len())];
    before
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with("#:"))
}

/// Names assigned as `self.<name> = ...` inside a class body. Upstream sees
/// these through `dir()`; the static parser deliberately does not.
fn instance_attributes(module: &crate::model::ModuleInfo, object_path: &str) -> Vec<String> {
    use crate::model::Member;
    let Some(Member::Class(class)) = module.find(object_path) else { return Vec::new() };
    if !class.span.is_valid() {
        return Vec::new();
    }
    let Ok(source) = fs::read_to_string(&module.path) else { return Vec::new() };
    let mut offset = 0usize;
    let mut out = Vec::new();
    for line in source.lines() {
        let line_end = offset + line.len();
        if offset >= class.span.start as usize && line_end <= class.span.end as usize {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("self.") {
                let name: String = rest
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                    .collect();
                if !name.is_empty()
                    && rest[name.len()..].trim_start().starts_with('=')
                    && !out.contains(&name)
                {
                    out.push(name);
                }
            }
        }
        offset = line_end + 1;
    }
    out
}

fn object_kind(module: &crate::model::ModuleInfo, object_path: &str) -> &'static str {
    use crate::model::Member;
    match module.find(object_path) {
        Some(Member::Class(class)) if is_exception(class) => "exception",
        Some(Member::Class(_)) => "class",
        Some(Member::Function(info)) => info.kind.objtype(),
        Some(Member::Attribute(_)) if object_path.contains('.') => "attribute",
        Some(Member::Attribute(_)) => "data",
        None if object_path.contains('.') => "attribute",
        None => "function",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_scanning_extracts_options_and_names() {
        let content = ".. autosummary::\n   :toctree: generated/\n   :template: custom.rst\n   :recursive:\n\n   pkg.mod\n   ~pkg.Foo\n";
        let found = find_directives(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].toctree.as_deref(), Some("generated/"));
        assert_eq!(found[0].template.as_deref(), Some("custom.rst"));
        assert!(found[0].recursive);
        assert_eq!(found[0].names, vec!["pkg.mod", "~pkg.Foo"]);
    }

    #[test]
    fn join_docname_normalizes_prefix() {
        let dir = Path::new("/src/api");
        assert_eq!(join_docname(dir, "generated"), Path::new("/src/api/generated"));
        assert_eq!(join_docname(dir, "../gen"), Path::new("/src/gen"));
    }

    #[test]
    fn write_stub_honors_source_encoding() {
        let dir = std::env::temp_dir().join(format!("scribe-stub-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.rst");
        write_stub(&path, "hello", "utf-8-sig").unwrap();
        assert_eq!(fs::read(&path).unwrap()[..3], [0xef, 0xbb, 0xbf]);
        write_stub(&path, "hello", "utf-8").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        let _ = fs::remove_dir_all(dir);
    }
}
