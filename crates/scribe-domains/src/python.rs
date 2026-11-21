//! The Python domain (`py`) — faithful port of `sphinx/domains/python`.
//!
//! This module owns three responsibilities, matching upstream's split between
//! `sphinx.domains.python`, `sphinx.domains.python._object` and
//! `sphinx.domains.python._annotations`:
//!
//! 1. the mutable inventory ([`PythonDomainData`]: objects + modules) and the
//!    [`Domain`] implementation (xref resolution, module index, persistence);
//! 2. signature/annotation parsing and `desc_*` node construction
//!    ([`signature`], in the `python/` submodule directory);
//! 3. module/class context tracking and `py:module` / `py:class` annotation on
//!    pending cross-references ([`prepare_document`]).
//!
//! The RST layer emits *skeleton* `desc` nodes (`crates/scribe-rst`'s
//! `py:*` directives). They carry the raw signature and the directive options
//! in `py:*` attributes; [`prepare_document`] expands them into the frozen
//! `desc_*`/`desc_sig_*` subtree and rewrites typed field bodies. Keeping the
//! skeleton seam in `scribe-rst` preserves the acyclic crate graph documented
//! in `ARCHITECTURE.md` §15.1: `scribe-rst` never depends on `scribe-domains`,
//! and the domain mutates the arena through [`Document`] only.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use scribe_ast::{Document, Node, NodeId, NodeKind, Span};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::traits::{docname_join, Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

pub mod signature;

pub use signature::{
    parse_annotation, parse_reftarget, parse_sig_re, prepare_document, PyOptions,
    SignatureInfo, SigContext,
};

// ----- static metadata -------------------------------------------------------

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();
static DIRECTIVES: OnceLock<Vec<&'static str>> = OnceLock::new();
static ROLES: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Upstream `PythonDomain.object_types`, in declaration order.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("function", "function", &["func", "obj"]),
            ObjectType::new("data", "data", &["data", "obj"]),
            ObjectType::new("class", "class", &["class", "exc", "obj"]),
            ObjectType::new("exception", "exception", &["exc", "class", "obj"]),
            ObjectType::new("method", "method", &["meth", "obj"]),
            ObjectType::new("classmethod", "class method", &["meth", "obj"]),
            ObjectType::new("staticmethod", "static method", &["meth", "obj"]),
            ObjectType::new("attribute", "attribute", &["attr", "obj"]),
            ObjectType::new("property", "property", &["attr", "_prop", "obj"]),
            ObjectType::new("type", "type alias", &["type", "class", "obj"]),
            ObjectType::new("module", "module", &["mod", "obj"]),
        ]
    })
}

/// Upstream `PythonDomain.directives`.
pub fn directive_names() -> &'static [&'static str] {
    DIRECTIVES.get_or_init(|| {
        vec![
            "function", "data", "class", "exception", "method", "classmethod", "staticmethod",
            "attribute", "property", "type", "module", "currentmodule", "decorator",
            "decoratormethod",
        ]
    })
}

/// Upstream `PythonDomain.roles`.
pub fn role_names() -> &'static [&'static str] {
    ROLES.get_or_init(|| {
        vec![
            "data", "exc", "func", "deco", "class", "const", "attr", "type", "meth", "mod", "obj",
        ]
    })
}

/// Roles whose upstream `XRefRole` sets `fix_parens=True`.
pub fn fix_parens_roles() -> &'static [&'static str] {
    &["func", "meth"]
}

/// `True` when `role` is owned by this domain.
pub fn is_py_role(role: &str) -> bool {
    role_names().contains(&role)
}

/// Object types addressable by a role (upstream `objtypes_for_role`).
pub fn objtypes_for_role(role: &str) -> Option<Vec<&'static str>> {
    if role == "obj" {
        return Some(object_types().iter().map(|object| object.name.as_str()).collect());
    }
    let types: Vec<&'static str> = object_types()
        .iter()
        .filter(|object| object.roles.iter().any(|candidate| candidate == role))
        .map(|object| object.name.as_str())
        .collect();
    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

/// First public role for an object type (upstream `role_for_objtype`).
pub fn role_for_objtype(objtype: &str) -> &'static str {
    object_types()
        .iter()
        .find(|object| object.name == objtype)
        .and_then(|object| object.roles.iter().find(|role| role.as_str() != "_prop"))
        .map_or("obj", String::as_str)
}

// ----- inventory -------------------------------------------------------------

/// Upstream `ObjectEntry` named tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonObjectEntry {
    pub docname: String,
    pub node_id: String,
    pub objtype: String,
    pub aliased: bool,
}

/// Upstream `ModuleEntry` named tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonModuleEntry {
    pub docname: String,
    pub node_id: String,
    pub synopsis: String,
    pub platform: String,
    pub deprecated: bool,
}

/// Serializable Python-domain inventory (`initial_data = {'objects': {}, 'modules': {}}`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonDomainData {
    /// Full qualified name → object entry.
    pub objects: BTreeMap<String, PythonObjectEntry>,
    /// Module name → module entry.
    pub modules: BTreeMap<String, PythonModuleEntry>,
}

/// The `py` domain implementation.
pub struct PythonDomain {
    data: RwLock<PythonDomainData>,
}

impl Default for PythonDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PythonDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let data = self.data.read();
        f.debug_struct("PythonDomain")
            .field("objects", &data.objects.len())
            .field("modules", &data.modules.len())
            .finish_non_exhaustive()
    }
}

impl PythonDomain {
    /// Creates an empty domain (upstream `initial_data`).
    pub fn new() -> Self {
        Self { data: RwLock::new(PythonDomainData::default()) }
    }

    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: PythonDomainData) -> Self {
        Self { data: RwLock::new(data) }
    }

    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> PythonDomainData {
        self.data.read().clone()
    }

    /// Upstream `note_object` (including the aliasing rules and exact warning).
    pub fn note_object(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        objtype: &str,
        node_id: &str,
        aliased: bool,
        span: Option<Span>,
    ) {
        self.note_object_ex(ctx, name, objtype, node_id, aliased, span, false);
    }

    /// `note_object` with an autodoc provenance flag. Autodoc duplicates use a
    /// distinct diagnostic code so builders can emit only those (Sphinx logs
    /// them with an `<unknown>:1` location, unlike ordinary domain duplicates).
    #[allow(clippy::too_many_arguments)]
    pub fn note_object_ex(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        objtype: &str,
        node_id: &str,
        aliased: bool,
        span: Option<Span>,
        autodoc: bool,
    ) {
        let mut data = self.data.write();
        if let Some(other) = data.objects.get(name) {
            if other.aliased && !aliased {
                // The original definition found; override the alias.
            } else if !other.aliased && aliased {
                // The original is already registered; ignore the alias.
                return;
            } else {
                let message = format!(
                    "duplicate object description of {name}, other instance in {}, use :no-index: for one of them",
                    other.docname
                );
                let mut diagnostic = DomainDiagnostic::duplicate_object(
                    message, objtype, name, Some(ctx.docname.to_owned()), span,
                );
                // Sphinx warns about autodoc duplicates only when both
                // descriptions live in the same document (e.g. `automodule`
                // plus `autoclass` in one page). Cross-document re-exports are
                // silently deduplicated.
                if autodoc && other.docname == ctx.docname {
                    diagnostic.code = crate::diagnostics::codes::AUTODOC_DUPLICATE.to_owned();
                }
                ctx.push_diagnostic(diagnostic);
            }
        }
        data.objects.insert(
            name.to_owned(),
            PythonObjectEntry {
                docname: ctx.docname.to_owned(),
                node_id: node_id.to_owned(),
                objtype: objtype.to_owned(),
                aliased,
            },
        );
    }

    /// Upstream `note_module`.
    pub fn note_module(
        &self,
        docname: &str,
        name: &str,
        node_id: &str,
        synopsis: &str,
        platform: &str,
        deprecated: bool,
    ) {
        self.data.write().modules.insert(
            name.to_owned(),
            PythonModuleEntry {
                docname: docname.to_owned(),
                node_id: node_id.to_owned(),
                synopsis: synopsis.to_owned(),
                platform: platform.to_owned(),
                deprecated,
            },
        );
    }

    /// Upstream `PythonDomain.find_obj`, faithfully ported.
    ///
    /// Returns `(matched fullname, entry)` pairs. `searchmode == 1` is the
    /// "specific" mode used by the `.` modifier and by `:any:`.
    pub fn find_obj(
        &self,
        module: Option<&str>,
        classname: Option<&str>,
        name: &str,
        typ: Option<&str>,
        searchmode: u8,
    ) -> Vec<(String, PythonObjectEntry)> {
        let name = name.strip_suffix("()").unwrap_or(name);
        if name.is_empty() {
            return Vec::new();
        }
        let data = self.data.read();
        let mut matches = Vec::new();
        let mut newname: Option<String> = None;

        if searchmode == 1 {
            let objtypes: Option<Vec<String>> = if let Some(typ) = typ {
                objtypes_for_role(typ).map(|types| types.into_iter().map(str::to_owned).collect())
            } else {
                Some(object_types().iter().map(|object| object.name.clone()).collect())
            };
            if let Some(objtypes) = objtypes {
                let allowed = |entry: &PythonObjectEntry| objtypes.iter().any(|kind| kind == &entry.objtype);
                if let (Some(module), Some(class)) = (module, classname) {
                    let full = format!("{module}.{class}.{name}");
                    if data.objects.get(&full).is_some_and(allowed) {
                        newname = Some(full);
                    }
                }
                if newname.is_none() {
                    let scoped = module.map(|module| format!("{module}.{name}"));
                    if scoped.as_deref().is_some_and(|key| data.objects.get(key).is_some_and(allowed)) {
                        newname = scoped;
                    } else if data.objects.get(name).is_some_and(allowed) {
                        newname = Some(name.to_owned());
                    } else {
                        let suffix = format!(".{name}");
                        for (oname, entry) in data.objects.iter() {
                            if oname.ends_with(&suffix) && allowed(entry) {
                                matches.push((oname.clone(), entry.clone()));
                            }
                        }
                    }
                }
            }
        } else {
            if data.objects.contains_key(name) {
                newname = Some(name.to_owned());
            } else if typ == Some("mod") {
                return Vec::new();
            } else if let Some(class) = classname {
                let key = format!("{class}.{name}");
                if data.objects.contains_key(&key) {
                    newname = Some(key);
                }
            }
            if newname.is_none() {
                if let Some(module) = module {
                    let key = format!("{module}.{name}");
                    if data.objects.contains_key(&key) {
                        newname = Some(key);
                    } else if let Some(class) = classname {
                        let key = format!("{module}.{class}.{name}");
                        if data.objects.contains_key(&key) {
                            newname = Some(key);
                        }
                    }
                }
            }
            if newname.is_none() {
                // Upstream's Python domain also accepts a partially-qualified
                // name: `:meth:`Sphinx.add_js_file`` resolves against
                // `sphinx.application.Sphinx.add_js_file` when the reference
                // carries no module context.  Try a unique suffix match over
                // objects addressable by this role; multiple hits are reported
                // as ambiguous by `resolve_xref`.
                let allowed: Option<Vec<&str>> = typ.and_then(objtypes_for_role);
                let suffix = format!(".{name}");
                for (oname, entry) in &data.objects {
                    if oname == name || !oname.ends_with(&suffix) {
                        continue;
                    }
                    if allowed.as_ref().is_none_or(|types| types.iter().any(|kind| *kind == entry.objtype)) {
                        matches.push((oname.clone(), entry.clone()));
                    }
                }
            }
        }
        if let Some(newname) = newname {
            if let Some(entry) = data.objects.get(&newname) {
                matches.push((newname, entry.clone()));
            }
        }
        matches
    }

    /// Builds the `py-modindex` (upstream `PythonModuleIndex.generate`).
    ///
    /// The returned groups are keyed by the lowercased first letter and sorted
    /// by module name; `collapse` is the upstream heuristic that decides
    /// whether the index starts collapsed in the HTML writer.
    pub fn py_modindex(
        &self,
        docnames: Option<&BTreeSet<String>>,
        common_prefixes: &[String],
    ) -> PythonModuleIndex {
        let data = self.data.read();
        let mut ignores: Vec<&String> = common_prefixes.iter().collect();
        ignores.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
        let mut modules: Vec<(&String, &PythonModuleEntry)> = data.modules.iter().collect();
        modules.sort_by_key(|entry| entry.0.to_lowercase());

        let mut groups: BTreeMap<String, Vec<ModuleIndexEntry>> = BTreeMap::new();
        let mut prev_modname = String::new();
        let mut num_top_levels = 0usize;
        let mut total = 0usize;

        for (modname_raw, module) in modules {
            total += 1;
            if docnames.is_some_and(|docs| !docs.contains(&module.docname)) {
                continue;
            }
            let mut modname = modname_raw.clone();
            let mut stripped = String::new();
            for ignore in &ignores {
                if modname.starts_with(ignore.as_str()) {
                    stripped = (*ignore).clone();
                    modname = modname[ignore.len()..].to_owned();
                    break;
                }
            }
            if modname.is_empty() {
                modname = std::mem::take(&mut stripped);
            }
            let Some(first) = modname.chars().next() else {
                continue;
            };
            let entries = groups.entry(first.to_lowercase().to_string()).or_default();
            let package = modname.split('.').next().unwrap_or(&modname).to_owned();
            let subtype = if package != modname {
                if prev_modname == package {
                    if let Some(last) = entries.last_mut() {
                        last.subtype = 1;
                    }
                } else if !prev_modname.starts_with(&package) {
                    entries.push(ModuleIndexEntry {
                        name: format!("{stripped}{package}"),
                        subtype: 1,
                        docname: String::new(),
                        anchor: String::new(),
                        extra: String::new(),
                        qualifier: String::new(),
                        descr: String::new(),
                    });
                }
                2
            } else {
                num_top_levels += 1;
                0
            };
            entries.push(ModuleIndexEntry {
                name: format!("{stripped}{modname}"),
                subtype,
                docname: module.docname.clone(),
                anchor: module.node_id.clone(),
                extra: module.platform.clone(),
                qualifier: if module.deprecated { "Deprecated".to_owned() } else { String::new() },
                descr: module.synopsis.clone(),
            });
            prev_modname = modname;
        }

        PythonModuleIndex {
            groups: groups.into_iter().collect(),
            collapse: total.saturating_sub(num_top_levels) < num_top_levels,
        }
    }

    // ----- resolution helpers -------------------------------------------------

    fn make_refnode(
        &self,
        ctx: &DomainContext<'_>,
        docname: &str,
        anchor: &str,
        reftitle: Option<String>,
        classes: &[&str],
    ) -> ResolvedReference {
        let mut reference = if ctx.docname == docname && !anchor.is_empty() {
            ResolvedReference::local(anchor, None)
        } else {
            let mut uri = ctx.relative_uri(ctx.docname, docname);
            if !anchor.is_empty() {
                uri.push('#');
                uri.push_str(anchor);
            }
            ResolvedReference::uri(uri, None)
        };
        reference.docname = Some(docname.to_owned());
        if let Some(title) = reftitle {
            reference.reftitle = Some(title);
        }
        reference.classes.extend(classes.iter().map(|class| (*class).to_owned()));
        reference
    }

    fn module_refnode(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        module: &PythonModuleEntry,
    ) -> ResolvedReference {
        let mut parts = vec![name.to_owned()];
        if !module.synopsis.is_empty() {
            parts.push(format!(": {}", module.synopsis));
        }
        if module.deprecated {
            parts.push(" (deprecated)".to_owned());
        }
        if !module.platform.is_empty() {
            parts.push(format!(" ({})", module.platform));
        }
        self.make_refnode(
            ctx,
            &module.docname,
            &module.node_id,
            Some(parts.concat()),
            &["xref", "py", "py-mod"],
        )
    }

    /// Upstream `builtin_resolver`: builtin/typing/None types keep their content
    /// without a missing-reference warning. Returns `true` when applied.
    fn suppress_builtin(typ: &str, target: &str) -> bool {
        if !matches!(typ, "class" | "obj" | "exc") {
            return false;
        }
        if target == "None" {
            return true;
        }
        const BUILTIN_CLASSES: &[&str] = &[
            "object", "type", "bool", "int", "float", "complex", "str", "bytes", "bytearray",
            "list", "tuple", "range", "set", "frozenset", "dict", "Exception", "BaseException",
            "ValueError", "TypeError", "KeyError", "IndexError", "RuntimeError", "OSError",
            "IOError", "StopIteration", "Warning", "function", "NoneType",
        ];
        if BUILTIN_CLASSES.contains(&target) {
            return true;
        }
        let bare = target.strip_prefix("typing.").unwrap_or(target);
        const TYPING: &[&str] = &[
            "Any", "Callable", "ClassVar", "Concatenate", "Final", "ForwardRef", "Generic",
            "Literal", "LiteralString", "Never", "NewType", "NoReturn", "NotRequired", "Optional",
            "ParamSpec", "ParamSpecArgs", "ParamSpecKwargs", "Protocol", "Required", "Self",
            "Type", "TypeAlias", "TypeGuard", "TypeVar", "Union", "Unpack", "Literal",
        ];
        TYPING.contains(&bare)
    }

    /// Reads the `py:module` / `py:class` context annotated by
    /// [`prepare_document`] on a pending xref.
    fn xref_context(document: &Document, node_id: NodeId) -> (Option<&str>, Option<&str>) {
        let Ok(node) = document.get(node_id) else {
            return (None, None);
        };
        let module = node.get_str("py:module").ok().flatten();
        let class = node.get_str("py:class").ok().flatten();
        (module, class)
    }
}

/// One `py-modindex` group entry (upstream `IndexEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIndexEntry {
    /// Display name (possibly stripped of `modindex_common_prefix`).
    pub name: String,
    /// `0` top level, `1` package group head / dummy, `2` submodule.
    pub subtype: u8,
    /// Document holding the declaration (empty for dummy entries).
    pub docname: String,
    /// Anchor inside the document.
    pub anchor: String,
    /// Platform qualifier.
    pub extra: String,
    /// "Deprecated" when applicable.
    pub qualifier: String,
    /// Synopsis description.
    pub descr: String,
}

/// Result of [`PythonDomain::py_modindex`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonModuleIndex {
    /// `(first letter lowercase, entries)` groups in sorted order.
    pub groups: Vec<(String, Vec<ModuleIndexEntry>)>,
    /// Upstream collapse heuristic.
    pub collapse: bool,
}

// ----- Domain trait ----------------------------------------------------------

impl Domain for PythonDomain {
    fn name(&self) -> &str {
        "py"
    }

    fn label(&self) -> &str {
        "Python"
    }

    fn object_types(&self) -> &[ObjectTypeAlias] {
        object_types()
    }

    fn directive_names(&self) -> &[&'static str] {
        directive_names()
    }

    fn role_names(&self) -> &[&'static str] {
        role_names()
    }

    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &PythonDomainData::default())
            .unwrap_or_default()
    }

    fn data_version(&self) -> u32 {
        1
    }

    fn clear_doc(&mut self, docname: &str) {
        let mut data = self.data.write();
        data.objects.retain(|_, entry| entry.docname != docname);
        data.modules.retain(|_, entry| entry.docname != docname);
    }

    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<PythonDomainData>() else {
            return;
        };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (name, entry) in other.objects {
            if owned.contains(entry.docname.as_str()) {
                data.objects.insert(name, entry);
            }
        }
        for (name, entry) in other.modules {
            if owned.contains(entry.docname.as_str()) {
                data.modules.insert(name, entry);
            }
        }
    }

    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        let order = document.preorder_ids();
        for id in order {
            let Ok(node) = document.get(id) else {
                continue;
            };
            if !matches!(node.kind, NodeKind::Desc | NodeKind::Target) {
                continue;
            }
            let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else {
                continue;
            };
            if !directive_names().contains(&objtype.as_str()) {
                continue;
            }
            if node.attrs.get_bool("no-index").ok().flatten().unwrap_or(false) {
                continue;
            }
            let Some(fullname) = node.get_str("py:fullname").ok().flatten().map(str::to_owned) else {
                continue;
            };
            let module = node.get_str("py:module").ok().flatten().map(str::to_owned);
            let anchor = node.get_str("py:anchor").ok().flatten().unwrap_or("").to_owned();
            // A `py:module` marker stores its own name in `py:module`; unlike
            // classes/functions it is *not* prefixed with its containing module
            // (upstream `PyModule` calls `note_object(fullname, 'module', ...)`).
            let object_key = if objtype == "module" {
                fullname.clone()
            } else {
                match &module {
                    Some(module) if !module.is_empty() => format!("{module}.{fullname}"),
                    _ => fullname.clone(),
                }
            };
            let span = node.span;

            if objtype == "module" {
                let entry = PythonModuleEntry {
                    docname: ctx.docname.to_owned(),
                    node_id: anchor.clone(),
                    synopsis: node.get_str("py:synopsis").ok().flatten().unwrap_or("").to_owned(),
                    platform: node.get_str("py:platform").ok().flatten().unwrap_or("").to_owned(),
                    deprecated: node.attrs.get_bool("py:deprecated").ok().flatten().unwrap_or(false),
                };
                let deprecated = entry.deprecated;
                let synopsis = entry.synopsis.clone();
                let platform = entry.platform.clone();
                self.note_module(ctx.docname, &object_key, &anchor, &synopsis, &platform, deprecated);
            }

            let autodoc = node.attrs.get_bool("py:autodoc").ok().flatten().unwrap_or(false);
            self.note_object_ex(ctx, &object_key, &objtype, &anchor, false, span, autodoc);

            // `canonical` registers an alias unless this is a `type` directive.
            if objtype != "type" {
                if let Some(canonical) = node.get_str("py:canonical").ok().flatten() {
                    if !canonical.is_empty() {
                        self.note_object_ex(ctx, canonical, &objtype, &anchor, true, span, autodoc);
                    }
                }
            }
        }
        Ok(())
    }

    fn resolve_xref(
        &self,
        ctx: &DomainContext<'_>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        let (module, classname) = Self::xref_context(document, node_id);
        let target = target.strip_suffix("()").unwrap_or(target);
        let specific = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refspecific())
            .unwrap_or(false);
        let searchmode = u8::from(specific);
        let explicit = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refexplicit())
            .unwrap_or(false);

        let mut matches = self.find_obj(module, classname, target, Some(typ), searchmode);

        // Upstream fallbacks between aliased/related object types.
        if matches.is_empty() && typ == "class" {
            for fallback in ["data", "attr"] {
                matches = self.find_obj(module, classname, target, Some(fallback), searchmode);
                if !matches.is_empty() {
                    break;
                }
            }
        }
        if matches.is_empty() && typ == "attr" {
            matches = self.find_obj(module, classname, target, Some("meth"), searchmode);
        }
        if matches.is_empty() && typ == "meth" {
            matches = self.find_obj(module, classname, target, Some("_prop"), searchmode);
        }

        if matches.is_empty() {
            if Self::suppress_builtin(typ, target) {
                return Ok(Some(ResolvedReference::default().passthrough()));
            }
            return Ok(None);
        }
        if matches.len() > 1 {
            let canonical: Vec<_> = matches.iter().filter(|(_, entry)| !entry.aliased).collect();
            if canonical.len() == 1 {
                matches = vec![canonical[0].clone()];
            } else {
                let rendered = matches
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let message = format!("more than one target found for cross-reference '{target}': {rendered}");
                let diagnostic = DomainDiagnostic::malformed(
                    message,
                    Some("ref".to_owned()),
                    Some(target.to_owned()),
                    Some(ctx.docname.to_owned()),
                    ctx.span_of(document, node_id),
                );
                ctx.push_diagnostic(diagnostic);
            }
        }

        let (name, entry) = &matches[0];
        if entry.objtype == "module" {
            let modules = self.snapshot().modules;
            let Some(module) = modules.get(name) else {
                return Ok(None);
            };
            return Ok(Some(self.module_refnode(ctx, name, module)));
        }
        let mut reference = self.make_refnode(
            ctx,
            &entry.docname,
            &entry.node_id,
            Some(name.clone()),
            &["xref", "py", &format!("py-{typ}")],
        );
        // Upstream `XRefRole(fix_parens=True)` appends `()` to implicit func /
        // meth titles (config `add_function_parentheses`).
        if !explicit && matches!(typ, "func" | "meth") {
            let content = document.get(contnode_id).map(|node| node.astext(document)).unwrap_or_default();
            let content = content.trim();
            if !content.is_empty() && !content.ends_with("()") {
                reference.title = Some(format!("{content}()"));
            }
        }
        Ok(Some(reference))
    }

    fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let (module, classname) = Self::xref_context(document, node_id);
        // `:any:` always searches in specific mode with no type constraint.
        let mut matches = self.find_obj(module, classname, target, None, 1);
        let multiple = matches.len() > 1;
        if multiple {
            matches.retain(|(_, entry)| !entry.aliased);
        }
        let _ = contnode_id;
        let mut results = Vec::new();
        let modules = self.snapshot().modules;
        for (name, entry) in matches {
            if entry.objtype == "module" {
                if let Some(module) = modules.get(&name) {
                    results.push(("py:mod".to_owned(), self.module_refnode(ctx, &name, module)));
                }
            } else {
                let short_role = role_for_objtype(&entry.objtype);
                let role = format!("py:{short_role}");
                results.push((
                    role,
                    self.make_refnode(
                        ctx,
                        &entry.docname,
                        &entry.node_id,
                        Some(name.clone()),
                        &["xref", "any", "py", &format!("py-{short_role}")],
                    ),
                ));
            }
        }
        Ok(results)
    }

    fn get_objects(&self) -> Vec<ObjectEntry> {
        let data = self.data.read();
        let mut entries = Vec::new();
        for (name, module) in &data.modules {
            let mut entry = ObjectEntry::new(
                name.clone(),
                name.clone(),
                "module",
                module.docname.clone(),
                module.node_id.clone(),
                0,
            );
            entry.synopsis = module.synopsis.clone();
            entry.platform = module.platform.clone();
            entries.push(entry);
        }
        for (name, object) in &data.objects {
            if object.objtype == "module" {
                continue;
            }
            let priority = if object.aliased { -1 } else { 1 };
            entries.push(ObjectEntry::new(
                name.clone(),
                name.clone(),
                object.objtype.clone(),
                object.docname.clone(),
                object.node_id.clone(),
                priority,
            ));
        }
        entries.sort_by(|a, b| {
            (a.name.as_str(), a.objtype.as_str(), a.docname.as_str(), a.anchor.as_str())
                .cmp(&(b.name.as_str(), b.objtype.as_str(), b.docname.as_str(), b.anchor.as_str()))
        });
        entries
    }

    fn get_full_qualified_name(&self, node: &Node) -> Option<String> {
        if node.kind != NodeKind::PendingXref {
            return None;
        }
        let target = node.pending_xref()?.reftarget()?;
        let module = node.get_str("py:module").ok().flatten().unwrap_or("");
        let class = node.get_str("py:class").ok().flatten().unwrap_or("");
        let parts = [module, class, target].into_iter().filter(|part| !part.is_empty());
        Some(parts.collect::<Vec<_>>().join("."))
    }

    fn export_data(&self) -> DomainResult<DomainData> {
        DomainData::from_payload(self.data_version(), &self.snapshot())
    }

    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()> {
        if data.version != self.data_version() {
            return Ok(());
        }
        let loaded: PythonDomainData = data.decode()?;
        *self.data.write() = loaded;
        Ok(())
    }
}

/// Upstream `find_obj`'s module layering, exposed for unit tests and Phase 14.
pub fn scoped_candidates(module: Option<&str>, classname: Option<&str>, name: &str) -> Vec<String> {
    let mut result = Vec::new();
    if let Some(module) = module {
        if let Some(class) = classname {
            result.push(format!("{module}.{class}.{name}"));
        }
        result.push(format!("{module}.{name}"));
    }
    if let Some(class) = classname {
        result.push(format!("{class}.{name}"));
    }
    result.push(name.to_owned());
    result
}

/// `py:` docname resolution used by the `:py:mod:` role with relative targets.
pub fn resolve_module_doc(ctx: &DomainContext<'_>, refdoc: &str, target: &str) -> Option<String> {
    let docname = docname_join(refdoc, target);
    ctx.env.is_some_and(|env| env.has_doc(&docname)).then_some(docname)
}

/// Anchor generation for a Python object.
///
/// Upstream uses `sphinx.util.nodes.make_id`, whose `_make_id` preserves ASCII
/// case, dots and underscores (unlike docutils' `make_id`). This is why Python
/// anchors look like `alias_module.Handler`, not `alias-module-handler`.
pub fn py_make_id(fullname: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let ascii: String = fullname.nfkd().filter(|ch| ch.is_ascii()).collect();
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in ascii.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    // `_non_id_at_ends = re.compile('^[-0-9._]+|-+$')`.
    let trimmed = out.trim_start_matches(['-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '_']);
    trimmed.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(object_types().len(), 11);
        assert_eq!(object_types()[0].name, "function");
        assert_eq!(object_types()[0].roles, ["func", "obj"]);
        assert_eq!(object_types()[8].roles, ["attr", "_prop", "obj"]);
        assert!(directive_names().contains(&"decoratormethod"));
        assert_eq!(role_names().len(), 11);
        assert_eq!(fix_parens_roles(), &["func", "meth"]);
    }

    #[test]
    fn role_type_mapping() {
        assert_eq!(objtypes_for_role("class"), Some(vec!["class", "exception", "type"]));
        assert_eq!(objtypes_for_role("const"), None);
        assert_eq!(role_for_objtype("property"), "attr");
        assert_eq!(role_for_objtype("classmethod"), "meth");
    }
}
