//! In-memory Python module model produced by the static extractor (Phase 21).
//!
//! The tree mirrors the constructs autodoc cares about: modules, classes
//! (including nested ones), functions/methods (including `async` and
//! properties), attributes and data, with source spans, decorators, type
//! annotations, base classes and docstrings. It is `serde`-serializable so a
//! parsed module can be cached by source-content hash (BLAKE3).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Byte span of a construct in its source file, plus its 1-based start line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// Byte offset of the first byte, inclusive.
    pub start: u32,
    /// Byte offset one past the last byte, exclusive.
    pub end: u32,
    /// 1-based source line.
    pub line: u32,
}

impl SourceSpan {
    /// Constructs a span.
    #[must_use]
    pub const fn new(start: u32, end: u32, line: u32) -> Self {
        Self { start, end, line }
    }

    /// Whether this span carries real location information.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.end > self.start
    }
}

/// Kind of a callable/member, used to choose the Sphinx directive and
/// signature prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionKind {
    /// A module-level function.
    Function,
    /// A coroutine (`async def`).
    Coroutine,
    /// A regular method inside a class.
    Method,
    /// A `@staticmethod`.
    StaticMethod,
    /// A `@classmethod`.
    ClassMethod,
    /// A `@property` / `@cached_property`.
    Property,
    /// A decorator function (callable object used with `@`).
    Decorator,
}

impl FunctionKind {
    /// The Sphinx Python-domain object type for this kind.
    #[must_use]
    pub const fn objtype(self) -> &'static str {
        match self {
            Self::Function | Self::Coroutine | Self::Decorator => "function",
            Self::Method => "method",
            Self::StaticMethod => "staticmethod",
            Self::ClassMethod => "classmethod",
            Self::Property => "property",
        }
    }

    /// Whether the leading `self`/`cls` parameter is implicit and hidden.
    #[must_use]
    pub const fn hides_self(self) -> bool {
        matches!(self, Self::Method | Self::ClassMethod | Self::Property)
    }
}

/// One parameter of a function signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterInfo {
    /// Parameter name (without `*` prefixes).
    pub name: String,
    /// Type annotation source text, when present.
    pub annotation: Option<String>,
    /// Default-value source text, when present.
    pub default: Option<String>,
    /// Parameter category.
    pub category: ParameterCategory,
}

/// Parameter category, mirroring Python's argument grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterCategory {
    /// Positional-only (before `/`).
    PositionalOnly,
    /// Normal positional-or-keyword argument.
    PositionalOrKeyword,
    /// `*args` variadic positional.
    VarPositional,
    /// Keyword-only (after `*`).
    KeywordOnly,
    /// `**kwargs` variadic keyword.
    VarKeyword,
}

/// A function, method, coroutine or property.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionInfo {
    /// Short name.
    pub name: String,
    /// Dotted name relative to the module (e.g. `Foo.method`).
    pub qualname: String,
    /// Rendered signature, e.g. `bar(x: int, y: str = 'hi') -> int`.
    /// `self`/`cls` are already removed for methods.
    pub signature: String,
    /// Docstring, dedented, or `None`.
    pub docstring: Option<String>,
    /// Decorator expressions (source text, without the `@`).
    pub decorators: Vec<String>,
    /// Function kind.
    pub kind: FunctionKind,
    /// Whether `@abstractmethod` is present.
    pub is_abstract: bool,
    /// Whether `@final` is present.
    pub is_final: bool,
    /// Whether `@overload` is present.
    pub is_overload: bool,
    /// Whether the function is considered public (no leading `_`).
    pub is_public: bool,
    /// Target parameter names from `:target:`-style decorators, if any.
    pub targets: Vec<String>,
    /// Source span.
    pub span: SourceSpan,
}

/// A module-level variable, class attribute or annotated assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeInfo {
    /// Short name.
    pub name: String,
    /// Dotted name relative to the module.
    pub qualname: String,
    /// Type annotation source text, when present.
    pub annotation: Option<String>,
    /// Source text of the assigned value, when present.
    pub value: Option<String>,
    /// Docstring taken from a string literal following the assignment.
    pub docstring: Option<String>,
    /// Whether the name is public (no leading `_`).
    pub is_public: bool,
    /// Source span.
    pub span: SourceSpan,
}

/// A class definition, possibly nested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassInfo {
    /// Short name.
    pub name: String,
    /// Dotted name relative to the module (e.g. `Outer.Inner`).
    pub qualname: String,
    /// Base-class expressions as source text.
    pub bases: Vec<String>,
    /// Docstring, dedented, or `None`.
    pub docstring: Option<String>,
    /// Decorators (source text without `@`).
    pub decorators: Vec<String>,
    /// Members declared directly in the class body, in source order.
    pub members: Vec<Member>,
    /// Whether `@final` is present.
    pub is_final: bool,
    /// Whether the class is abstract (`ABCMeta` / `@abstractmethod` member).
    pub is_abstract: bool,
    /// Whether the name is public.
    pub is_public: bool,
    /// Source span.
    pub span: SourceSpan,
}

/// A member of a module or class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Member {
    /// A function, method or property.
    Function(FunctionInfo),
    /// A nested (or top-level) class.
    Class(ClassInfo),
    /// A variable, attribute or annotated assignment.
    Attribute(AttributeInfo),
}

impl Member {
    /// Short member name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Function(info) => &info.name,
            Self::Class(info) => &info.name,
            Self::Attribute(info) => &info.name,
        }
    }

    /// Dotted name relative to the owning module.
    #[must_use]
    pub fn qualname(&self) -> &str {
        match self {
            Self::Function(info) => &info.qualname,
            Self::Class(info) => &info.qualname,
            Self::Attribute(info) => &info.qualname,
        }
    }

    /// Source span of the member.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Function(info) => info.span,
            Self::Class(info) => info.span,
            Self::Attribute(info) => info.span,
        }
    }

    /// Whether the member is public (no leading underscore).
    #[must_use]
    pub fn is_public(&self) -> bool {
        match self {
            Self::Function(info) => info.is_public,
            Self::Class(info) => info.is_public,
            Self::Attribute(info) => info.is_public,
        }
    }

    /// Docstring attached to the member, if any.
    #[must_use]
    pub fn docstring(&self) -> Option<&str> {
        match self {
            Self::Function(info) => info.docstring.as_deref(),
            Self::Class(info) => info.docstring.as_deref(),
            Self::Attribute(info) => info.docstring.as_deref(),
        }
    }

    /// The Sphinx domain object type used for this member.
    #[must_use]
    pub fn objtype(&self) -> &'static str {
        match self {
            Self::Function(info) => info.kind.objtype(),
            Self::Class(_) => "class",
            Self::Attribute(_) => "attribute",
        }
    }
}

/// An `import` / `from ... import ...` statement, retained so base classes can
/// be resolved across parsed files and so `inherited-members` works.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportInfo {
    /// Imported module path (`os.path`) or source module for from-imports.
    pub module: String,
    /// Imported names with their local aliases.
    pub names: Vec<(String, Option<String>)>,
    /// Relative import level (`0` for absolute imports).
    pub level: u32,
}

/// A fully parsed Python module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleInfo {
    /// Fully-qualified module name (`pkg.mod`).
    pub name: String,
    /// Absolute path of the source file.
    pub path: PathBuf,
    /// Module docstring, dedented, or `None`.
    pub docstring: Option<String>,
    /// Explicit `__all__` list, when statically resolvable.
    pub all: Option<Vec<String>>,
    /// Members in source order.
    pub members: Vec<Member>,
    /// Imports in source order.
    pub imports: Vec<ImportInfo>,
    /// BLAKE3 hash of the source text (hex), used for cache keys.
    pub source_hash: String,
}

impl ModuleInfo {
    /// Creates an empty module.
    #[must_use]
    pub fn new(name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            name: name.into(),
            path,
            docstring: None,
            all: None,
            members: Vec::new(),
            imports: Vec::new(),
            source_hash: String::new(),
        }
    }

    /// Looks up every member matching a dotted path (overloads create several
    /// definitions with the same name).
    #[must_use]
    pub fn find_all(&self, qualname: &str) -> Vec<&Member> {
        let mut segments = qualname.split('.');
        let first = segments.next();
        let rest: Vec<&str> = segments.collect();
        self.members
            .iter()
            .filter(|member| Some(member.name()) == first)
            .filter(|member| {
                if rest.is_empty() {
                    true
                } else {
                    let Member::Class(class) = member else { return false };
                    let mut path = class;
                    for (index, segment) in rest.iter().enumerate() {
                        if index + 1 == rest.len() {
                            return path.members.iter().any(|member| member.name() == *segment);
                        }
                        let Some(next) = path.members.iter().find_map(|member| match member {
                            Member::Class(info) if info.name == *segment => Some(info),
                            _ => None,
                        }) else { return false };
                        path = next;
                    }
                    true
                }
            })
            .collect()
    }

    /// Looks up a member by its dotted path relative to this module.
    #[must_use]
    pub fn find(&self, qualname: &str) -> Option<&Member> {
        let mut segments = qualname.split('.');
        let first = segments.next()?;
        let mut current = self.members.iter().find(|member| member.name() == first)?;
        for segment in segments {
            let Member::Class(class) = current else {
                return None;
            };
            current = class.members.iter().find(|member| member.name() == segment)?;
        }
        Some(current)
    }
}

/// A set of parsed modules with a class index for cross-file inheritance.
#[derive(Clone, Debug, Default)]
pub struct ModuleSet {
    /// Parsed modules keyed by fully-qualified module name.
    pub modules: std::collections::BTreeMap<String, ModuleInfo>,
}

impl ModuleSet {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a module, replacing any previous entry with the same name.
    pub fn insert(&mut self, module: ModuleInfo) {
        self.modules.insert(module.name.clone(), module);
    }

    /// Looks up a module by name.
    #[must_use]
    pub fn module(&self, name: &str) -> Option<&ModuleInfo> {
        self.modules.get(name)
    }

    /// Resolves a base class name to its parsed `ClassInfo`.
    ///
    /// `base` may be a short name (`Base`), a dotted path relative to the
    /// current module/class (`pkg.mod.Base`) or an absolute path. Imports in
    /// the owning module are consulted for aliases and from-imports.
    #[must_use]
    pub fn resolve_class<'a>(&'a self, base: &str, owner: &'a ModuleInfo) -> Option<&'a ClassInfo> {
        let base = base.trim();
        // Exact dotted path within the owning module first.
        let candidate = format!("{}.{}", owner.name, base);
        if let Some(class) = find_class_path(owner, base) {
            return Some(class);
        }
        if let Some(module) = self.modules.get(&candidate) {
            if let Some(class) = find_class_path(module, base.strip_prefix(&format!("{}.", module.name)).unwrap_or(base)) {
                return Some(class);
            }
        }
        // Absolute module path.
        if let Some((module_name, class_path)) = split_module_class(base, &self.modules) {
            if let Some(module) = self.modules.get(module_name) {
                if let Some(class) = find_class_path(module, class_path) {
                    return Some(class);
                }
            }
        }
        // `from mod import Base` aliases.
        for import in &owner.imports {
            for (name, alias) in &import.names {
                let local = alias.as_deref().unwrap_or(name);
                if local != base {
                    continue;
                }
                let module_name = if import.level == 0 && !import.module.is_empty() {
                    import.module.clone()
                } else {
                    qualify_relative(&owner.name, &import.module, import.level)
                };
                if let Some(module) = self.modules.get(&module_name) {
                    if let Some(class) = find_class_path(module, name) {
                        return Some(class);
                    }
                }
            }
        }
        None
    }
}

/// Finds a class by its dotted path inside a module.
fn find_class_path<'a>(module: &'a ModuleInfo, path: &str) -> Option<&'a ClassInfo> {
    let mut segments = path.split('.');
    let first = segments.next()?;
    let mut class = module.members.iter().find_map(|member| match member {
        Member::Class(info) if info.name == first => Some(info),
        _ => None,
    })?;
    for segment in segments {
        class = class.members.iter().find_map(|member| match member {
            Member::Class(info) if info.name == segment => Some(info),
            _ => None,
        })?;
    }
    Some(class)
}

/// Splits `pkg.mod.Class` into the longest known module prefix and the rest.
fn split_module_class<'a>(
    path: &'a str,
    modules: &'a std::collections::BTreeMap<String, ModuleInfo>,
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&str, &str)> = None;
    for name in modules.keys() {
        if path == name.as_str() {
            continue;
        }
        if let Some(rest) = path.strip_prefix(name) {
            if let Some(rest) = rest.strip_prefix('.') {
                if best.is_none_or(|(module, _)| name.len() > module.len()) {
                    best = Some((name.as_str(), rest));
                }
            }
        }
    }
    best
}

/// Resolves a relative import to an absolute module name.
fn qualify_relative(current: &str, module: &str, level: u32) -> String {
    if level == 0 {
        return module.to_owned();
    }
    let package = current.rsplit_once('.').map_or("", |(parent, _)| parent);
    let mut parts: Vec<&str> = package.split('.').filter(|part| !part.is_empty()).collect();
    let up = (level as usize).saturating_sub(1);
    for _ in 0..up {
        parts.pop();
    }
    if !module.is_empty() {
        parts.extend(module.split('.'));
    }
    parts.join(".")
}
