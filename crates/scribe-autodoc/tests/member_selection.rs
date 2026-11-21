//! Member filtering/ordering tests (Phase 21).

use std::collections::BTreeSet;
use std::path::Path;

use scribe_autodoc::config::{AutoOptions, MemberOrder};
use scribe_autodoc::discovery::{load_modules, resolve_module, search_roots};
use scribe_autodoc::model::Member;
use scribe_autodoc::{select_members, ModuleInfo};

fn fixture_module() -> ModuleInfo {
    let source = "\
\"\"\"Doc.\"\"\"\n\
__all__ = ['zebra', 'alpha']\n\
def zebra(): \"\"\"Z.\"\"\"\n\
def alpha(): \"\"\"A.\"\"\"\n\
def _private(): \"\"\"P.\"\"\"\n\
def __special__(): \"\"\"S.\"\"\"\n\
def undocumented(): pass\n\
class Beta:\n    \"\"\"B.\"\"\"\n    def method(self): \"\"\"M.\"\"\"\n";
    scribe_autodoc::parse_source(source, "fixture", Path::new("fixture.py")).unwrap()
}

fn names(members: Vec<&Member>) -> Vec<String> {
    members.into_iter().map(|member| member.name().to_owned()).collect()
}

#[test]
fn default_documents_no_members_unless_opted_in() {
    // Upstream's `autoclass`/`automodule` document only the object itself
    // unless `:members:` (or `autodoc_default_options`) opts in.
    let module = fixture_module();
    assert!(select_members(&module.members, &AutoOptions::default(), None).is_empty());
    let options = AutoOptions { members: Some(Vec::new()), ..AutoOptions::default() };
    assert_eq!(names(select_members(&module.members, &options, None)), vec!["Beta", "alpha", "zebra"]);
}

#[test]
fn all_members_and_private_toggles() {
    let module = fixture_module();
    let options = AutoOptions {
        members: Some(Vec::new()),
        undoc_members: true,
        private_members: true,
        special_members: true,
        member_order: MemberOrder::BySource,
        ..AutoOptions::default()
    };
    let selected = select_members(&module.members, &options, None);
    assert_eq!(names(selected), vec![
        "zebra", "alpha", "_private", "__special__", "undocumented", "Beta",
    ]);
}

#[test]
fn explicit_members_and_exclusion() {
    let module = fixture_module();
    let options = AutoOptions {
        members: Some(vec!["alpha".to_owned(), "_private".to_owned()]),
        exclude_members: BTreeSet::from(["alpha".to_owned()]),
        ..AutoOptions::default()
    };
    assert_eq!(names(select_members(&module.members, &options, None)), vec!["_private"]);
}

#[test]
fn all_list_filters_when_present() {
    let module = fixture_module();
    let all = module.all.as_deref();
    // `__all__` only filters members that were opted in with `:members:`.
    assert!(select_members(&module.members, &AutoOptions::default(), all).is_empty());
    let options = AutoOptions { members: Some(Vec::new()), ..AutoOptions::default() };
    assert_eq!(names(select_members(&module.members, &options, all)), vec!["alpha", "zebra"]);
    let options = AutoOptions {
        members: Some(Vec::new()),
        ignore_module_all: true,
        ..AutoOptions::default()
    };
    assert_eq!(names(select_members(&module.members, &options, all)), vec!["Beta", "alpha", "zebra"]);
}

#[test]
fn alphabetical_and_groupwise_orders() {
    let module = fixture_module();
    let mut options = AutoOptions {
        members: Some(Vec::new()),
        undoc_members: true,
        member_order: MemberOrder::Alphabetical,
        ..AutoOptions::default()
    };
    assert_eq!(names(select_members(&module.members, &options, None)), vec![
        "Beta", "alpha", "undocumented", "zebra",
    ]);
    options.member_order = MemberOrder::Groupwise;
    assert_eq!(names(select_members(&module.members, &options, None)), vec![
        "Beta", "alpha", "undocumented", "zebra",
    ]);
}

#[test]
fn inherited_member_discovery_across_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tree");
    let roots = search_roots(Some(&root), None);
    assert!(resolve_module("pkg.mod", &roots).is_some());
    let report = load_modules(&["pkg".to_owned(), "pkg.mod".to_owned()], &roots, true);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let module = report.modules.module("pkg.mod").unwrap();
    let child = module.find("Child").unwrap();
    let Member::Class(class) = child else { panic!("class") };
    assert_eq!(class.bases, vec!["Base".to_owned()]);
    // The local `own` method is always present; the inherited one is resolved
    // via ModuleSet when `inherited-members` is enabled.
    assert!(class.members.iter().any(|member| member.name() == "own"));
    let base = report.modules.module("pkg.base").unwrap();
    assert!(base.find("Base.inherited").is_some());
}
