//! Phase 09: toctree collection, hierarchy, numbering and navigation tests.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use common::{default_config, Fixture};
use scribe_config::{Config, Tags};
use scribe_core::{
    collect_toctrees, BuildEnvironment, NavRelation, ToctreeCollection,
};
use scribe_rst::{
    parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions,
};

const INDEX: &str = "\
Welcome
=======

.. toctree::
   :maxdepth: 2
   :numbered:
   :caption: Contents

   foo
   bar

.. toctree::
   :hidden:

   External <https://example.org/>
   self
";

const FOO: &str = "\
Foo
===

Section One
-----------

Subsection
^^^^^^^^^^

Section Two
-----------
";

const BAR: &str = "\
Bar
===
";

fn parse_document(name: &str, source: &str, config: &Config, fixture: &Fixture) -> scribe_ast::Document {
    let mut output = parse_with_options(source, ParseOptions::new(Some(name.to_owned())));
    let mut context = Context::new(fixture.src(), Tags::new());
    context.config = Some(Arc::new(config.clone()));
    context.source_name = Some(name.to_owned());
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "unexpected resolve issues: {:?}", report.unresolved);
    output.document
}

fn collect(fixture: &Fixture, sources: &[(&str, &str)]) -> (BuildEnvironment, Config, ToctreeCollection) {
    for (name, source) in sources {
        fixture.write(&format!("{name}.rst"), source);
    }
    let config = default_config();
    let mut environment = BuildEnvironment::new(fixture.src());
    environment
        .update_defaults(&config, fixture.src())
        .expect("environment update");
    for (name, source) in sources {
        let document = parse_document(name, source, &config, fixture);
        environment.record_document(name, &document, BTreeMap::new());
    }
    let result = collect_toctrees(&mut environment, &config);
    (environment, config, result)
}

#[test]
fn parses_all_toctree_options_and_entry_kinds() {
    let fixture = Fixture::new("options");
    let (env, _config, result) = collect(
        &fixture,
        &[
            ("index", INDEX),
            ("foo", FOO),
            ("bar", BAR),
        ],
    );
    assert!(result.is_clean(), "{:?}", result.warnings);
    assert_eq!(result.toctrees.len(), 2);
    let first = &result.toctrees[0];
    assert_eq!(first.options.maxdepth, 2);
    assert_eq!(first.options.caption.as_deref(), Some("Contents"));
    assert_eq!(first.options.numbered, 999);
    assert!(!first.options.glob && !first.options.hidden && !first.options.titlesonly);
    assert_eq!(first.includefiles, vec!["foo", "bar"]);
    assert_eq!(first.entries.len(), 2);

    let second = &result.toctrees[1];
    assert!(second.options.hidden);
    assert_eq!(second.entries.len(), 2);
    assert!(second.entries[0].is_external());
    assert_eq!(second.entries[0].title.as_deref(), Some("External"));
    assert_eq!(second.entries[0].target.uri(), Some("https://example.org/"));
    assert!(second.entries[1].is_self());
    assert!(second.includefiles.is_empty());

    assert_eq!(
        env.toctree_includes.get("index"),
        Some(&vec!["foo".to_owned(), "bar".to_owned()])
    );
    assert!(env.numbered_toctrees.contains("index"));
    assert!(!env.glob_toctrees.contains("index"));
    assert!(env.files_to_rebuild.get("foo").is_some_and(|parents| parents.contains("index")));
}

#[test]
fn hierarchy_traversal_and_prev_next_match_upstream_preorder() {
    let fixture = Fixture::new("nav");
    let (_env, _config, result) = collect(
        &fixture,
        &[
            ("index", INDEX),
            ("foo", FOO),
            ("bar", BAR),
        ],
    );
    assert_eq!(result.hierarchy.order, vec!["index", "foo", "bar"]);
    assert_eq!(
        result.relations.get("index"),
        Some(&NavRelation {
            parent: None,
            prev: None,
            next: Some("foo".to_owned()),
        })
    );
    assert_eq!(
        result.relations.get("foo"),
        Some(&NavRelation {
            parent: Some("index".to_owned()),
            prev: Some("index".to_owned()),
            next: Some("bar".to_owned()),
        })
    );
    assert_eq!(
        result.relations.get("bar"),
        Some(&NavRelation {
            parent: Some("index".to_owned()),
            prev: Some("foo".to_owned()),
            next: None,
        })
    );
}

#[test]
fn section_numbering_follows_numbered_toctree_depth() {
    let fixture = Fixture::new("numbers");
    let (_env, _config, result) = collect(
        &fixture,
        &[
            ("index", INDEX),
            ("foo", FOO),
            ("bar", BAR),
        ],
    );
    assert_eq!(result.section_numbers.get("foo", ""), Some(&[1][..]));
    assert_eq!(result.section_numbers.get("foo", "#section-one"), Some(&[1, 1][..]));
    assert_eq!(result.section_numbers.get("foo", "#subsection"), Some(&[1, 1, 1][..]));
    assert_eq!(result.section_numbers.get("foo", "#section-two"), Some(&[1, 2][..]));
    assert_eq!(result.section_numbers.get("bar", ""), Some(&[2][..]));
    assert_eq!(result.section_numbers.display("foo", "#subsection"), "1.1.1");
}


// ---------------------------------------------------------------------------
// Per-option and adversarial coverage
// ---------------------------------------------------------------------------

fn collect_with_config(
    fixture: &Fixture,
    sources: &[(&str, &str)],
    configure: impl FnOnce(&mut Config),
) -> (BuildEnvironment, Config, scribe_core::ToctreeCollection) {
    for (name, source) in sources {
        fixture.write(&format!("{name}.rst"), source);
    }
    let mut config = default_config();
    configure(&mut config);
    let mut environment = BuildEnvironment::new(fixture.src());
    environment
        .update_defaults(&config, fixture.src())
        .expect("environment update");
    for (name, source) in sources {
        let document = parse_document(name, source, &config, fixture);
        environment.record_document(name, &document, BTreeMap::new());
    }
    let collection = collect_toctrees(&mut environment, &config);
    (environment, config, collection)
}

#[test]
fn every_documented_option_is_decoded() {
    let fixture = Fixture::new("all-options");
    let source = "\
Options
=======

.. toctree::
   :maxdepth: 4
   :caption: A caption
   :glob:
   :hidden:
   :includehidden:
   :numbered: 3
   :titlesonly:
   :reversed:
   :name: mytoc
   :class: fancy wide

   a

.. toctree::

   b
";
    let (_env, _config, result) = collect_with_config(
        &fixture,
        &[("index", source), ("a", "A\n=\n"), ("b", "B\n=\n")],
        |_| {},
    );
    assert!(result.is_clean(), "{:?}", result.warnings);
    let first = &result.toctrees[0];
    assert_eq!(first.options.maxdepth, 4);
    assert_eq!(first.options.caption.as_deref(), Some("A caption"));
    assert!(first.options.glob);
    assert!(first.options.hidden);
    assert!(first.options.includehidden);
    assert_eq!(first.options.numbered, 3);
    assert!(first.options.titlesonly);
    assert!(first.options.reversed);
    assert_eq!(first.options.name.as_deref(), Some("mytoc"));
    let index_doc = result.toctrees[0].docname.clone();
    assert_eq!(index_doc, "index");
    let wrapper = {
        let doc = {
            // Re-parse to inspect the wrapper attributes.
            let text = source;
            parse_document("index", text, &default_config(), &fixture)
        };
        doc.preorder_ids()
            .into_iter()
            .find(|&id| doc.get(id).is_ok_and(|n| n.kind == scribe_ast::NodeKind::Compound))
            .map(|id| doc.get(id).unwrap().clone())
            .expect("compound wrapper")
    };
    assert!(wrapper.classes().unwrap().contains(&"toctree-wrapper".to_owned()));
    assert!(wrapper.classes().unwrap().contains(&"fancy".to_owned()));
    assert!(wrapper.ids().unwrap().contains(&"mytoc".to_owned()));

    let second = &result.toctrees[1];
    assert_eq!(second.options.maxdepth, -1);
    assert_eq!(second.options.numbered, 0);
    assert!(!second.options.hidden);
}

#[test]
fn glob_expansion_sorts_and_respects_exclude_patterns() {
    let fixture = Fixture::new("glob-exclude");
    let source = "\
Root
====

.. toctree::
   :glob:

   sub/*
";
    let (_env, config, result) = collect_with_config(
        &fixture,
        &[
            ("index", source),
            ("sub/a", "A\n=\n"),
            ("sub/b", "B\n=\n"),
            ("sub/skip", "Skip\n====\n"),
        ],
        |config| {
            config
                .set("exclude_patterns", scribe_config::ConfigValue::List(vec![
                    scribe_config::ConfigValue::Str("sub/skip.rst".to_owned()),
                ]))
                .expect("set exclude_patterns");
        },
    );
    assert!(config.get_strings("exclude_patterns").unwrap().contains(&"sub/skip.rst".to_owned()));
    assert_eq!(result.toctrees[0].includefiles, vec!["sub/a", "sub/b"]);
    assert!(result.warnings.is_empty());
}

#[test]
fn reversed_expands_glob_before_reversing() {
    let fixture = Fixture::new("reversed");
    let source = "\
Root
====

.. toctree::
   :glob:
   :reversed:

   sub/*
";
    let (_env, _config, result) = collect_with_config(
        &fixture,
        &[
            ("index", source),
            ("sub/a", "A\n=\n"),
            ("sub/b", "B\n=\n"),
        ],
        |_| {},
    );
    assert_eq!(result.toctrees[0].includefiles, vec!["sub/b", "sub/a"]);
    assert_eq!(result.hierarchy.order, vec!["index", "sub/b", "sub/a"]);
}

#[test]
fn empty_glob_reports_recoverable_diagnostic() {
    let fixture = Fixture::new("empty-glob");
    let source = "Root\n====\n\n.. toctree::\n   :glob:\n\n   nomatch/*\n";
    let (_env, _config, result) = collect(&fixture, &[("index", source)]);
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0].code, "scribe::toc::empty_glob");
    assert!(result.warnings[0].message.contains("nomatch/*"));
    assert!(!result.diagnostics().is_empty());
}

#[test]
fn cyclic_toctrees_are_broken_with_a_cycle_path_diagnostic() {
    let fixture = Fixture::new("cycle");
    let index = "Index\n=====\n\n.. toctree::\n\n   a\n";
    let a = "A\n=\n\n.. toctree::\n\n   index\n";
    for (name, source) in [("index", index), ("a", a)] {
        fixture.write(&format!("{name}.rst"), source);
    }
    let config = default_config();
    let mut env = BuildEnvironment::new(fixture.src());
    env.update_defaults(&config, fixture.src()).unwrap();
    let sources: BTreeMap<String, Arc<String>> = [("index", index), ("a", a)]
        .into_iter()
        .map(|(name, text)| (name.to_owned(), Arc::new(text.to_owned())))
        .collect();
    for (name, text) in &sources {
        let document = parse_document(name, text, &config, &fixture);
        env.record_document(name, &document, BTreeMap::new());
    }
    let result = scribe_core::collect_toctrees_with_sources(&mut env, &config, &sources);
    let cycle = result
        .warnings
        .iter()
        .find(|warning| warning.code == "scribe::toc::circular")
        .expect("cycle diagnostic");
    assert!(cycle.message.contains("index") && cycle.message.contains("a"));
    assert!(!cycle.span.is_empty(), "source span must point at the toctree");
    let rendered = format!("{:?}", result.diagnostics());
    assert!(rendered.contains("circular toctree"), "{rendered}");
    // The graph is finite and both docs are still visited exactly once.
    assert_eq!(result.hierarchy.order.len(), 2);
    assert_eq!(result.hierarchy.order.iter().collect::<BTreeSet<_>>().len(), 2);
}

#[test]
fn missing_and_duplicate_entries_produce_warnings_without_panicking() {
    let fixture = Fixture::new("bad-entries");
    let source = "\
Root
====

.. toctree::

   ghost
   a
   a
";
    let (_env, _config, result) = collect(&fixture, &[("index", source), ("a", "A\n=\n")]);
    let missing = result
        .warnings
        .iter()
        .find(|warning| warning.code == "scribe::toc::missing_document")
        .expect("missing doc warning");
    assert!(missing.message.contains("ghost"));
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.code == "scribe::toc::duplicate_entry"));
    assert_eq!(result.toctrees[0].includefiles, vec!["a", "a"]);
    assert_eq!(result.hierarchy.order, vec!["index", "a"]);
}

#[test]
fn unreachable_documents_are_warned_about() {
    let fixture = Fixture::new("orphan");
    let source = "Root\n====\n\n.. toctree::\n\n   a\n";
    let (_env, _config, result) = collect(
        &fixture,
        &[("index", source), ("a", "A\n=\n"), ("lonely", "Lonely\n======\n")],
    );
    let warning = result
        .warnings
        .iter()
        .find(|warning| warning.code == "scribe::toc::not_included")
        .expect("unreachable warning");
    assert!(warning.message.contains("lonely"));
}

#[test]
fn document_toc_reports_local_sections() {
    let fixture = Fixture::new("local-toc");
    let (_env, _config, result) = collect(
        &fixture,
        &[("index", "Index\n=====\n"), ("a", "A\n=\n\nOne\n---\n\nTwo\n---\n")],
    );
    let local = result.hierarchy.document_toc(&_env, "a");
    assert_eq!(local.len(), 2);
    assert_eq!(local[0].title, "One");
    assert_eq!(local[1].title, "Two");
}

#[test]
fn toctree_edges_drive_incremental_invalidation() {
    use scribe_core::UpdateOptions;

    let fixture = Fixture::new("invalidate");
    let index = "Index\n=====\n\n.. toctree::\n\n   child\n";
    let child = "Child\n=====\n";
    fixture.write("index.rst", index);
    fixture.write("child.rst", child);
    let config = default_config();
    let options = UpdateOptions {
        cache_dir: Some(fixture.cache().to_path_buf()),
        persist: true,
        ..UpdateOptions::default()
    };
    let mut env = BuildEnvironment::new(fixture.src());
    env.update(&config, fixture.src(), &options).unwrap();
    for (name, text) in [("index", index), ("child", child)] {
        let document = parse_document(name, text, &config, &fixture);
        env.record_document(name, &document, BTreeMap::new());
    }
    let collection = collect_toctrees(&mut env, &config);
    assert_eq!(collection.hierarchy.order, vec!["index", "child"]);
    env.save(fixture.cache()).expect("save environment with edges");

    fixture.append("child.rst", "\nExtra paragraph.\n");
    let report = env.update(&config, fixture.src(), &options).unwrap();
    // Phase 08's frozen env treats the `files_to_rebuild` edge as an
    // invalidation edge, so the toctree parent follows its changed child.
    assert_eq!(
        report.changed,
        ["child".to_owned(), "index".to_owned()].into_iter().collect()
    );
    // `note_file_to_rebuild` is the edge that makes the parent follow the child.
    assert!(env.direct_dependents("child").contains("index"));
}

#[test]
fn numfig_numbers_figures_tables_and_code_blocks() {
    use scribe_ast::{Node, NodeKind};

    let fixture = Fixture::new("numfig");
    fixture.write("index.rst", "Index\n=====\n");
    let mut config = default_config();
    config.set("numfig", scribe_config::ConfigValue::Bool(true)).unwrap();
    let mut env = BuildEnvironment::new(fixture.src());
    env.update_defaults(&config, fixture.src()).unwrap();

    let mut document = scribe_ast::Document::new(None);
    let section = document
        .add(NodeKind::Section, Some(document.root))
        .expect("section");
    document.get_mut(section).unwrap().set_list("ids", vec!["intro".to_owned()]);
    let title = document.add(NodeKind::Title, Some(section)).expect("title");
    let _ = document.add_node(Node::text("Intro"), Some(title)).expect("title text");
    for (kind, id) in [
        (NodeKind::Figure, "fig-one"),
        (NodeKind::Table, "table-one"),
        (NodeKind::Container, "code-one"),
    ] {
        let node = document.add(kind, Some(section)).expect("enumerable node");
        document.get_mut(node).unwrap().set_list("ids", vec![id.to_owned()]);
        if kind == NodeKind::Container {
            let literal = document.add(NodeKind::LiteralBlock, Some(node)).unwrap();
            let _ = document.add_node(Node::text("let x = 1"), Some(literal)).unwrap();
        }
    }
    env.record_document("index", &document, BTreeMap::new());

    let collection = collect_toctrees(&mut env, &config);
    assert_eq!(
        collection.figure_numbers.get("index", "figure", "fig-one"),
        Some(&[1][..])
    );
    assert_eq!(
        collection.figure_numbers.get("index", "table", "table-one"),
        Some(&[1][..])
    );
    assert_eq!(
        collection.figure_numbers.get("index", "code-block", "code-one"),
        Some(&[1][..])
    );
    let stored = env.get_doctree("index").unwrap();
    assert!(stored
        .preorder_ids()
        .iter()
        .any(|&id| stored.get(id).is_ok_and(|node| node.attrs.get("fignumber").is_some())));
}

#[test]
fn sectnum_placeholders_receive_the_enclosing_section_number() {
    let fixture = Fixture::new("sectnum");
    let index = "\
Index
=====

.. toctree::
   :numbered:

   a
";
    let a = "\
A
=

One
---

.. sectnum::

Two
---
";
    let (_env, _config, result) = collect(&fixture, &[("index", index), ("a", a)]);
    assert!(result.is_clean(), "{:?}", result.warnings);
    let doc = _env.get_doctree("a").expect("doctree");
    let placeholders: Vec<_> = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            doc.get(id).is_ok_and(|node| {
                node.kind == scribe_ast::NodeKind::Pending
                    && matches!(
                        node.attrs.get("__directive"),
                        Some(scribe_ast::AttributeValue::Str(marker)) if marker.as_ref() == "sectnum"
                    )
            })
        })
        .collect();
    assert_eq!(placeholders.len(), 1);
    let number = doc
        .get(placeholders[0])
        .unwrap()
        .attrs
        .get("secnumber")
        .and_then(|value| match value {
            scribe_ast::AttributeValue::Str(text) => Some(text.as_ref()),
            _ => None,
        });
    assert_eq!(number, Some("1.1"));
}

#[test]
fn global_toctree_honours_hidden_and_includehidden() {
    let fixture = Fixture::new("global-toc");
    let index = "\
Index
=====

.. toctree::

   a

.. toctree::
   :hidden:
   :includehidden:

   b
";
    let (_env, _config, result) = collect(
        &fixture,
        &[("index", index), ("a", "A\n=\n"), ("b", "B\n=\n")],
    );
    let with_hidden = result.global_toctree_for_doc("index", true).expect("global toc");
    assert_eq!(
        with_hidden.children.iter().map(|node| node.docname.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let without_hidden = result.global_toctree_for_doc("index", false).expect("global toc");
    // `:includehidden:` overrides the caller's prune, matching upstream.
    assert_eq!(without_hidden.children.len(), 2);
    // Navigation still follows both toctrees.
    assert_eq!(result.hierarchy.order, vec!["index", "a", "b"]);
}
