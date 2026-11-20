//! Static Python source extractor (Phase 21).
//!
//! Parses `.py`/`.pyi` sources with `rustpython-parser` and builds the
//! [`ModuleInfo`] tree without importing or executing any Python code. Type
//! annotations, default values and decorator expressions are captured from
//! their exact source byte spans, so generated signatures preserve the source
//! spelling. Parsing is parallel-friendly: [`parse_sources`] uses `rayon` and
//! results are keyed by a BLAKE3 hash of the source text for caching.

use std::path::{Path, PathBuf};

use rustpython_ast::Ranged;
use rustpython_ast::{Alias, ArgWithDefault, Arguments, Constant, Expr, Identifier, Stmt, Suite};
use rustpython_parser::Mode;

use crate::error::{AutodocError, AutodocResult};
use crate::model::{
    AttributeInfo, ClassInfo, FunctionInfo, FunctionKind, ImportInfo, Member, ModuleInfo,
    ParameterCategory, ParameterInfo, SourceSpan,
};

/// Parses one Python source string into a [`ModuleInfo`].
///
/// `name` is the fully-qualified module name and `path` the source file used
/// for diagnostics and cache identity. Malformed Python returns a structured
/// [`AutodocError::Parse`] with a byte span; callers degrade to a warning.
pub fn parse_source(source: &str, name: &str, path: &Path) -> AutodocResult<ModuleInfo> {
    let ast = rustpython_parser::parse(source, Mode::Module, &path.to_string_lossy()).map_err(|error| {
        let offset = error.offset;
        let row = line_of(source, u32::from(offset));
        let start = u32::from(offset);
        AutodocError::Parse {
            path: path.display().to_string(),
            line: Some(row),
            detail: error.to_string(),
            span: Some((start as usize, 1).into()),
        }
    })?;
    let body = match ast {
        rustpython_ast::Mod::Module(module) => module.body,
        other => {
            return Err(AutodocError::Parse {
                path: path.display().to_string(),
                line: None,
                detail: format!("unexpected parse mode: {other:?}"),
                span: None,
            });
        }
    };
    let mut module = ModuleInfo::new(name, path.to_path_buf());
    module.source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    module.docstring = extract_docstring(&body);
    let ctx = ExtractCtx { source };
    module.members = ctx.extract_members(&body, "");
    module.imports = extract_imports(&body, source);
    module.all = extract_all(&body, source);
    Ok(module)
}

/// Parses a group of `(source, name, path)` tuples in parallel.
///
/// Failures are returned per entry in the same order as the input so callers
/// can emit one warning per bad module without losing the good ones.
#[must_use]
pub fn parse_sources(
    inputs: &[(String, String, PathBuf)],
) -> Vec<AutodocResult<ModuleInfo>> {
    use rayon::prelude::*;
    inputs
        .par_iter()
        .map(|(source, name, path)| parse_source(source, name, path))
        .collect()
}

struct ExtractCtx<'a> {
    source: &'a str,
}

impl<'a> ExtractCtx<'a> {
    /// Slices the exact source text covered by an AST node.
    fn slice<T: Ranged>(&self, node: &T) -> Option<String> {
        let range = node.range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        self.source.get(start..end).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    }

    fn span<T: Ranged>(&self, node: &T) -> SourceSpan {
        let range = node.range();
        let start = u32::from(range.start());
        let end = u32::from(range.end());
        SourceSpan::new(start, end, line_of(self.source, start))
    }

    /// Extracts members from a statement suite, prefixing their qualnames.
    fn extract_members(&self, body: &Suite, prefix: &str) -> Vec<Member> {
        let mut members = Vec::new();
        let mut index = 0usize;
        while index < body.len() {
            let stmt = &body[index];
            match stmt {
                Stmt::FunctionDef(func) => {
                    let qualname = qualify(prefix, func.name.as_str());
                    let docstring = extract_docstring(&func.body);
                    let decorators = decorator_paths(&func.decorator_list, self.source);
                    let kind = classify_function(&decorators, false, !prefix.is_empty());
                    let signature = self.render_function_signature(
                        &func.args,
                        func.returns.as_deref(),
                        &func.name,
                        kind,
                        false,
                    );
                    let is_abstract = decorators.iter().any(|name| name.ends_with("abstractmethod"));
                    let is_final = decorators.iter().any(|name| name.ends_with("final"));
                    let is_overload = decorators.iter().any(|name| name.ends_with("overload"));
                    members.push(Member::Function(FunctionInfo {
                        name: func.name.to_string(),
                        qualname,
                        signature,
                        docstring,
                        decorators,
                        kind,
                        is_abstract,
                        is_final,
                        is_overload,
                        is_public: !func.name.starts_with('_'),
                        targets: Vec::new(),
                        span: self.span(func),
                    }));
                }
                Stmt::AsyncFunctionDef(func) => {
                    let qualname = qualify(prefix, func.name.as_str());
                    let docstring = extract_docstring(&func.body);
                    let decorators = decorator_paths(&func.decorator_list, self.source);
                    let kind = classify_function(&decorators, true, !prefix.is_empty());
                    let signature = self.render_function_signature(
                        &func.args,
                        func.returns.as_deref(),
                        &func.name,
                        kind,
                        true,
                    );
                    let is_abstract = decorators.iter().any(|name| name.ends_with("abstractmethod"));
                    let is_final = decorators.iter().any(|name| name.ends_with("final"));
                    let is_overload = decorators.iter().any(|name| name.ends_with("overload"));
                    members.push(Member::Function(FunctionInfo {
                        name: func.name.to_string(),
                        qualname,
                        signature,
                        docstring,
                        decorators,
                        kind,
                        is_abstract,
                        is_final,
                        is_overload,
                        is_public: !func.name.starts_with('_'),
                        targets: Vec::new(),
                        span: self.span(func),
                    }));
                }
                Stmt::ClassDef(class) => {
                    let qualname = qualify(prefix, class.name.as_str());
                    let docstring = extract_docstring(&class.body);
                    let decorators = decorator_paths(&class.decorator_list, self.source);
                    let child_prefix = qualname.clone();
                    let nested = self.extract_members(&class.body, &child_prefix);
                    let bases = class.bases.iter().filter_map(|base| self.slice(base)).collect();
                    members.push(Member::Class(ClassInfo {
                        name: class.name.to_string(),
                        qualname,
                        bases,
                        docstring,
                        decorators: decorators.clone(),
                        members: nested,
                        is_final: decorators.iter().any(|name| name.ends_with("final")),
                        is_abstract: decorators.iter().any(|name| name.ends_with("abstractmethod"))
                            || has_abstract_member(&class.body),
                        is_public: !class.name.starts_with('_'),
                        span: self.span(class),
                    }));
                }
                Stmt::AnnAssign(assign) => {
                    if let Some(name) = assign_name(&assign.target) {
                        // `__all__` is handled separately; skip private dunders here.
                        if name == "__all__" {
                            index += 1;
                            continue;
                        }
                        let qualname = qualify(prefix, &name);
                        let value = assign.value.as_deref().and_then(|value| self.slice(value));
                        let docstring = following_docstring(body, index + 1, self.source)
                            .or_else(|| alias_docstring(value.as_deref()));
                        members.push(Member::Attribute(AttributeInfo {
                            name: name.clone(),
                            qualname,
                            annotation: self.slice(assign.annotation.as_ref()),
                            value,
                            docstring,
                            is_public: !name.starts_with('_'),
                            span: self.span(assign),
                        }));
                    }
                }
                Stmt::Assign(assign) => {
                    for target in &assign.targets {
                        let Some(name) = assign_name(target) else { continue };
                        if name == "__all__" {
                            continue;
                        }
                        let qualname = qualify(prefix, &name);
                        let value = self.slice(assign.value.as_ref());
                        let docstring = following_docstring(body, index + 1, self.source)
                            .or_else(|| alias_docstring(value.as_deref()));
                        members.push(Member::Attribute(AttributeInfo {
                            name: name.clone(),
                            qualname,
                            annotation: None,
                            value,
                            docstring,
                            is_public: !name.starts_with('_'),
                            span: self.span(assign),
                        }));
                    }
                }
                _ => {}
            }
            index += 1;
        }
        dedup_overloads(&mut members);
        members
    }

    /// Renders a function signature from its AST arguments, using source
    /// slices for annotations and defaults.
    fn render_function_signature(
        &self,
        args: &Arguments,
        returns: Option<&Expr>,
        name: &Identifier,
        kind: FunctionKind,
        is_async: bool,
    ) -> String {
        let mut signature = String::new();
        if is_async {
            signature.push_str("async ");
        }
        signature.push_str(name.as_str());
        signature.push('(');

        let mut emitted: Vec<String> = Vec::new();

        // Combine positional-only and positional-or-keyword arguments in
        // source order, inserting `/` after the positional-only group.
        let mut all_positional: Vec<(&ArgWithDefault, bool)> = Vec::new();
        for arg in &args.posonlyargs {
            all_positional.push((arg, true));
        }
        for arg in &args.args {
            all_positional.push((arg, false));
        }
        // Upstream autodoc drops the implicit `self`/`cls` parameter from
        // method signatures. It is always the first positional parameter.
        let skip_first = kind.hides_self() && !all_positional.is_empty();
        let mut slash_at: Option<usize> = None;
        let mut posonly_done = false;
        for (position, (arg, posonly)) in all_positional.into_iter().enumerate() {
            if skip_first && position == 0 {
                continue;
            }
            emitted.push(self.render_arg(arg));
            if posonly && !posonly_done {
                // The `/` goes immediately after the last emitted positional-only arg.
                slash_at = Some(emitted.len());
            } else if !posonly {
                posonly_done = true;
            }
        }
        if let Some(at) = slash_at {
            emitted.insert(at, "/".to_owned());
        }

        if let Some(vararg) = &args.vararg {
            emitted.push(format!("*{}", self.render_arg_def(vararg.as_ref())));
        } else if !args.kwonlyargs.is_empty() {
            emitted.push("*".to_owned());
        }
        for arg in &args.kwonlyargs {
            emitted.push(self.render_arg(arg));
        }
        if let Some(kwarg) = &args.kwarg {
            emitted.push(format!("**{}", self.render_arg_def(kwarg.as_ref())));
        }

        signature.push_str(&emitted.join(", "));
        signature.push(')');
        if let Some(ret) = returns {
            if let Some(text) = self.slice(ret) {
                signature.push_str(" -> ");
                signature.push_str(&text);
            }
        }
        signature
    }

    fn render_arg(&self, arg: &ArgWithDefault) -> String {
        let mut text = self.render_arg_def(&arg.def);
        if let Some(default) = &arg.default {
            if let Some(value) = self.slice(default.as_ref()) {
                text.push_str(" = ");
                text.push_str(&value);
            }
        }
        text
    }

    fn render_arg_def(&self, arg: &rustpython_ast::Arg) -> String {
        let mut text = arg.arg.to_string();
        if let Some(annotation) = &arg.annotation {
            if let Some(value) = self.slice(annotation.as_ref()) {
                text.push_str(": ");
                text.push_str(&value);
            }
        }
        text
    }

}

/// Extracts structured parameter metadata from parsed arguments. Exposed so
/// downstream type-hint rendering and tests can inspect annotations without
/// re-parsing the rendered signature.
#[must_use]
pub fn extract_parameters(source: &str, args: &Arguments) -> Vec<ParameterInfo> {
    fn slice(source: &str, expr: &Expr) -> Option<String> {
        let range = expr.range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        source.get(start..end).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    }
    fn param(source: &str, arg: &ArgWithDefault, category: ParameterCategory) -> ParameterInfo {
        ParameterInfo {
            name: arg.def.arg.to_string(),
            annotation: arg.def.annotation.as_deref().and_then(|value| slice(source, value)),
            default: arg.default.as_deref().and_then(|value| slice(source, value)),
            category,
        }
    }
    fn plain(source: &str, arg: &rustpython_ast::Arg, category: ParameterCategory) -> ParameterInfo {
        ParameterInfo {
            name: arg.arg.to_string(),
            annotation: arg.annotation.as_deref().and_then(|value| slice(source, value)),
            default: None,
            category,
        }
    }
    let mut out = Vec::new();
    for arg in &args.posonlyargs {
        out.push(param(source, arg, ParameterCategory::PositionalOnly));
    }
    for arg in &args.args {
        out.push(param(source, arg, ParameterCategory::PositionalOrKeyword));
    }
    if let Some(vararg) = &args.vararg {
        out.push(plain(source, vararg, ParameterCategory::VarPositional));
    }
    for arg in &args.kwonlyargs {
        out.push(param(source, arg, ParameterCategory::KeywordOnly));
    }
    if let Some(kwarg) = &args.kwarg {
        out.push(plain(source, kwarg, ParameterCategory::VarKeyword));
    }
    out
}

/// Upstream documents a bare alias assignment (`my_name = Foo`) as
/// "alias of Foo". Reproduce that sentence statically when the value is a
/// single identifier.
fn alias_docstring(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
        && value.chars().next().is_some_and(|ch| ch.is_alphabetic() || ch == '_')
    {
        Some(format!("alias of {value}"))
    } else {
        None
    }
}

/// Upstream documents `@overload` declarations but hides the concrete
/// implementation that follows them. Remove the implementation when at least
/// one overload for the same name exists.
fn dedup_overloads(members: &mut Vec<Member>) {
    let overloaded: Vec<String> = members
        .iter()
        .filter_map(|member| match member {
            Member::Function(info) if info.is_overload => Some(info.qualname.clone()),
            _ => None,
        })
        .collect();
    members.retain(|member| match member {
        Member::Function(info) => !overloaded.contains(&info.qualname) || info.is_overload,
        _ => true,
    });
}

fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

fn line_of(source: &str, offset: u32) -> u32 {
    1 + source.as_bytes()[..(offset as usize).min(source.len())]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count() as u32
}

fn assign_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attr) => Some(attr.attr.to_string()),
        _ => None,
    }
}

/// Returns the dotted decorator expression names (without call arguments).
fn decorator_paths(exprs: &[Expr], source: &str) -> Vec<String> {
    exprs.iter().filter_map(|expr| expr_dotted_name(expr).or_else(|| slice_expr(expr, source))).collect()
}

fn slice_expr(expr: &Expr, source: &str) -> Option<String> {
    let range = expr.range();
    let start = u32::from(range.start()) as usize;
    let end = u32::from(range.end()) as usize;
    source.get(start..end).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Extracts a dotted name from `foo`, `foo.bar`, `foo(...)` and `@foo.bar(...)`.
fn expr_dotted_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attr) => {
            let base = expr_dotted_name(&attr.value)?;
            Some(format!("{}.{}", base, attr.attr))
        }
        Expr::Call(call) => expr_dotted_name(&call.func),
        Expr::Subscript(subscript) => expr_dotted_name(&subscript.value),
        _ => None,
    }
}

fn classify_function(decorators: &[String], is_async: bool, in_class: bool) -> FunctionKind {
    let has = |suffix: &str| decorators.iter().any(|decorator| {
        decorator == suffix
            || decorator.ends_with(&format!(".{suffix}"))
            || decorator.starts_with(&format!("{suffix}("))
    });
    if has("property") || has("cached_property") {
        FunctionKind::Property
    } else if has("staticmethod") {
        FunctionKind::StaticMethod
    } else if has("classmethod") {
        FunctionKind::ClassMethod
    } else if in_class {
        FunctionKind::Method
    } else if is_async {
        FunctionKind::Coroutine
    } else {
        FunctionKind::Function
    }
}

fn has_abstract_member(body: &Suite) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::FunctionDef(func) => func.decorator_list.iter().any(|expr| {
            expr_dotted_name(expr).is_some_and(|name| name.ends_with("abstractmethod"))
        }),
        Stmt::AsyncFunctionDef(func) => func.decorator_list.iter().any(|expr| {
            expr_dotted_name(expr).is_some_and(|name| name.ends_with("abstractmethod"))
        }),
        _ => false,
    })
}

/// Extracts the module/class/function docstring from the first statement.
fn extract_docstring(body: &Suite) -> Option<String> {
    let first = body.first()?;
    let Stmt::Expr(expr) = first else { return None };
    string_constant(&expr.value).map(|value| dedent_docstring(&value))
}

/// Extracts a docstring from the statement immediately after `index`.
fn following_docstring(body: &Suite, index: usize, source: &str) -> Option<String> {
    let _ = source;
    let stmt = body.get(index)?;
    let Stmt::Expr(expr) = stmt else { return None };
    string_constant(&expr.value).map(|value| dedent_docstring(&value))
}

fn string_constant(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Constant(constant) => match &constant.value {
            Constant::Str(value) => Some(value.to_string()),
            _ => None,
        },
        // Implicit concatenation of adjacent string literals.
        Expr::BinOp(binop) => {
            let left = string_constant(&binop.left)?;
            let right = string_constant(&binop.right)?;
            Some(format!("{left}{right}"))
        }
        _ => None,
    }
}

/// Dedents a docstring following PEP 257: the first line is kept verbatim and
/// the minimum indentation of subsequent non-blank lines is removed.
#[must_use]
pub fn dedent_docstring(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim();
    let rest: Vec<&str> = lines.collect();
    let min_indent = rest
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<String> = Vec::new();
    if !first.is_empty() {
        out.push(first.to_owned());
    }
    for line in rest {
        let trimmed = if line.len() >= min_indent { &line[min_indent..] } else { line.trim_start() };
        out.push(trimmed.trim_end().to_owned());
    }
    while out.first().is_some_and(|line| line.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn extract_imports(body: &Suite, source: &str) -> Vec<ImportInfo> {
    let _ = source;
    let mut out = Vec::new();
    for stmt in body {
        match stmt {
            Stmt::Import(import) => {
                for alias in &import.names {
                    out.push(ImportInfo {
                        module: alias.name.to_string(),
                        names: vec![(alias.name.to_string(), alias.asname.as_ref().map(Identifier::to_string))],
                        level: 0,
                    });
                }
            }
            Stmt::ImportFrom(import) => {
                let module = import.module.as_ref().map(Identifier::to_string).unwrap_or_default();
                let level = import.level.as_ref().map(|value| value.to_u32()).unwrap_or(0);
                let names = import
                    .names
                    .iter()
                    .map(|alias: &Alias| (alias.name.to_string(), alias.asname.as_ref().map(Identifier::to_string)))
                    .collect();
                out.push(ImportInfo { module, names, level });
            }
            // Type-only imports inside `if TYPE_CHECKING:` are exactly the
            // bindings annotation names refer to in static sources.
            Stmt::If(branch) => {
                out.extend(extract_imports(&branch.body, source));
                out.extend(extract_imports(&branch.orelse, source));
            }
            _ => {}
        }
    }
    out
}

fn extract_all(body: &Suite, source: &str) -> Option<Vec<String>> {
    let _ = source;
    for stmt in body {
        let Stmt::Assign(assign) = stmt else { continue };
        if !assign.targets.iter().any(|target| assign_name(target).as_deref() == Some("__all__")) {
            continue;
        }
        return strings_in_expr(&assign.value);
    }
    None
}

fn strings_in_expr(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::List(list) => Some(list.elts.iter().filter_map(string_constant).collect()),
        Expr::Tuple(tuple) => Some(tuple.elts.iter().filter_map(string_constant).collect()),
        Expr::Set(set) => Some(set.elts.iter().filter_map(string_constant).collect()),
        Expr::BinOp(binop) => {
            let mut left = strings_in_expr(&binop.left)?;
            left.extend(strings_in_expr(&binop.right)?);
            Some(left)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedent_handles_pep257() {
        let raw = "Summary.\n\n    Detail one.\n      Detail two.\n";
        assert_eq!(dedent_docstring(raw), "Summary.\n\nDetail one.\n  Detail two.");
    }

    #[test]
    fn parse_simple_module() {
        let source = "\"\"\"Doc.\"\"\"\n__all__ = ['a']\ndef a(x: int = 1) -> str: \"\"\"A.\"\"\"\n";
        let module = parse_source(source, "m", Path::new("m.py")).unwrap();
        assert_eq!(module.docstring.as_deref(), Some("Doc."));
        assert_eq!(module.all.as_deref(), Some(&["a".to_owned()][..]));
        assert_eq!(module.members.len(), 1);
        assert!(matches!(module.members[0], Member::Function(_)));
    }
}
