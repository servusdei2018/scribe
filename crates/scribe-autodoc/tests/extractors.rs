//! Static extractor unit tests (Phase 21).

use std::path::Path;

use scribe_autodoc::discovery::{resolve_in_root, resolve_module};
use scribe_autodoc::model::{FunctionKind, Member, ModuleSet};
use scribe_autodoc::parser::{dedent_docstring, extract_parameters, parse_source};

fn parse(source: &str) -> scribe_autodoc::model::ModuleInfo {
    parse_source(source, "sample", Path::new("sample.py")).expect("valid Python")
}

#[test]
fn extracts_module_docstring_all_and_members() {
    let module = parse(
        "\"\"\"Module doc.\"\"\"\n\
         __all__ = [\"Foo\", \"f\"]\n\
         VALUE: int = 1\n\
         def f(x: str = \"a\") -> bool:\n    \"\"\"F doc.\"\"\"\n    return True\n\
         class Foo:\n    \"\"\"Foo doc.\"\"\"\n    pass\n",
    );
    assert_eq!(module.docstring.as_deref(), Some("Module doc."));
    assert_eq!(module.all.as_deref(), Some(&["Foo".to_owned(), "f".to_owned()][..]));
    assert_eq!(module.members.len(), 3);
    assert!(matches!(module.members[0], Member::Attribute(_)));
    assert!(matches!(module.members[1], Member::Function(_)));
    assert!(matches!(module.members[2], Member::Class(_)));
}

#[test]
fn extracts_annotations_and_defaults_from_source() {
    let module = parse("def f(x: 'list[int]' = [1, 2], *, y: dict[str, int] = {}) -> 'tuple[int, ...]': pass\n");
    let Member::Function(info) = &module.members[0] else { panic!("expected function") };
    assert!(info.signature.contains("x: 'list[int]' = [1, 2]"), "{}", info.signature);
    assert!(info.signature.contains("y: dict[str, int] = {}"), "{}", info.signature);
    assert!(info.signature.ends_with("'tuple[int, ...]'"), "{}", info.signature);
}

#[test]
fn classifies_decorators_and_hides_self() {
    let module = parse(
        "class C:\n\
         \x20   @property\n\
         \x20   def prop(self) -> int: return 1\n\
         \x20   @staticmethod\n\
         \x20   def stat(x): pass\n\
         \x20   @classmethod\n\
         \x20   def cm(cls, x): pass\n\
         \x20   @abc.abstractmethod\n\
         \x20   def ab(self): pass\n",
    );
    let Member::Class(class) = &module.members[0] else { panic!("expected class") };
    let kinds: Vec<FunctionKind> = class
        .members
        .iter()
        .filter_map(|member| match member {
            Member::Function(info) => Some(info.kind),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![
        FunctionKind::Property,
        FunctionKind::StaticMethod,
        FunctionKind::ClassMethod,
        FunctionKind::Method,
    ]);
    let prop = class.members[0].qualname();
    assert_eq!(prop, "C.prop");
    let Member::Function(method) = &class.members[1] else { panic!("static") };
    assert!(method.signature.contains("stat(x)"), "{}", method.signature);
    let Member::Function(cm) = &class.members[2] else { panic!("classmethod") };
    assert!(cm.signature.contains("cm(x)"), "{}", cm.signature);
    let Member::Function(ab) = &class.members[3] else { panic!("abstract") };
    assert!(ab.is_abstract);
}

#[test]
fn async_functions_are_marked() {
    let module = parse("async def afunc(x: int) -> None:\n    \"\"\"Async doc.\"\"\"\n    pass\n");
    let Member::Function(info) = &module.members[0] else { panic!("function") };
    assert_eq!(info.kind, FunctionKind::Coroutine);
    assert!(info.signature.starts_with("async afunc"), "{}", info.signature);
}

#[test]
fn nested_classes_and_inheritance_bases() {
    let module = parse(
        "class Outer(Base, Generic[T]):\n\
         \x20   class Inner:\n\
         \x20       def deep(self): pass\n",
    );
    let Member::Class(outer) = &module.members[0] else { panic!("class") };
    assert_eq!(outer.bases, vec!["Base", "Generic[T]"]);
    assert_eq!(outer.qualname, "Outer");
    let Member::Class(inner) = &outer.members[0] else { panic!("nested class") };
    assert_eq!(inner.qualname, "Outer.Inner");
    assert_eq!(inner.members[0].qualname(), "Outer.Inner.deep");
}

#[test]
fn parses_pyi_and_malformed_python_without_panicking() {
    let stub = parse("def f(x: int) -> str: ...\nclass C:\n    attr: int\n");
    assert_eq!(stub.members.len(), 2);
    let bad = parse_source("def broken(:\n", "bad", Path::new("bad.py"));
    assert!(bad.is_err());
}

#[test]
fn dedent_preserves_first_line() {
    assert_eq!(dedent_docstring("  Summary\n    detail\n"), "Summary\ndetail");
}

#[test]
fn structured_parameters_include_categories() {
    let source = "def f(a, /, b: int = 1, *args: str, c, **kw): pass\n";
    let module = parse_source(source, "m", Path::new("m.py")).unwrap();
    let rustpython_ast::Mod::Module(ast) =
        rustpython_parser::parse(source, rustpython_parser::Mode::Module, "m.py").unwrap()
    else { panic!("module") };
    let rustpython_ast::Stmt::FunctionDef(func) = &ast.body[0] else { panic!("function") };
    let params = extract_parameters(source, &func.args);
    assert_eq!(params.len(), 5);
    assert_eq!(params[0].name, "a");
    assert_eq!(params[1].name, "b");
    assert_eq!(params[1].annotation.as_deref(), Some("int"));
    assert_eq!(params[1].default.as_deref(), Some("1"));
    assert_eq!(params[2].name, "args");
    assert_eq!(params[3].name, "c");
    assert_eq!(params[4].name, "kw");
    assert!(module.members.len() == 1);
}

#[test]
fn module_discovery_handles_packages_and_stubs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tree");
    assert!(resolve_in_root("pkg", &root).is_some_and(|path| path.ends_with("pkg/__init__.py")));
    assert!(resolve_in_root("pkg.mod", &root).is_some_and(|path| path.ends_with("pkg/mod.py")));
    assert!(resolve_module("missing", &[root]).is_none());
}

#[test]
fn cross_file_inheritance_index() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tree");
    let report = scribe_autodoc::discovery::load_modules(
        &["pkg".to_owned(), "pkg.mod".to_owned()],
        &[root],
        true,
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let base_module = report.modules.module("pkg.base").expect("base module");
    assert!(base_module.find("Base").is_some());
    let mod_module = report.modules.module("pkg.mod").expect("mod module");
    let mut set = ModuleSet::new();
    for module in report.modules.modules.values() {
        set.insert(module.clone());
    }
    // `pkg.mod` does `from .base import Base`; the alias resolves to the class
    // defined in the other parsed file.
    assert!(set.resolve_class("Base", mod_module).is_some());
}
