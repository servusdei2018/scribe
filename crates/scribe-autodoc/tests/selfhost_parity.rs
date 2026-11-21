//! Regression tests for the autodoc fixes that make `sphinx/doc` self-host
//! warning-free: class context for standalone auto directives, public names
//! for re-exported objects, and registration of members whose Python source
//! is not available offline.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};

use common::{config, object_set, scribe_pseudoxml};

static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture(files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "scribe-autodoc-selfhost-{}-{}",
        std::process::id(),
        FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (name, source) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, source).expect("write");
    }
    root
}

#[test]
fn automethod_inside_handwritten_class_uses_class_context() {
    let source = fixture(&[
        ("mypkg/__init__.py", ""),
        (
            "mypkg/mod.py",
            "class C:\n    \"\"\"Doc.\"\"\"\n\n    def f(self):\n        \"\"\"F.\"\"\"\n",
        ),
    ]);
    let cfg = config(&["sphinx.ext.autodoc"], &[]);
    let rst = "\
.. currentmodule:: mypkg.mod

.. class:: C

   .. automethod:: f
";
    let xml = scribe_pseudoxml(rst, &source, &cfg);
    let facts = object_set(&xml);
    assert!(
        facts.iter().any(|(module, class, fullname)| module == "mypkg.mod"
            && class == "C"
            && fullname == "C.f"),
        "class member was not registered: {facts:?}\n{xml}"
    );
}

#[test]
fn reexported_function_keeps_public_canonical_name() {
    let source = fixture(&[
        ("mypkg/__init__.py", "from mypkg.impl import func\n"),
        ("mypkg/impl.py", "def func():\n    \"\"\"Doc.\"\"\"\n"),
    ]);
    let cfg = config(&["sphinx.ext.autodoc"], &[]);
    let rst = "\
.. currentmodule:: mypkg

.. autofunction:: func
";
    let xml = scribe_pseudoxml(rst, &source, &cfg);
    let facts = object_set(&xml);
    // The defining module is registered as the object's home; the public name
    // is kept as a canonical alias so references to `mypkg.func` resolve.
    assert!(
        facts.iter().any(|(module, _, fullname)| module == "mypkg.impl" && fullname == "func"),
        "re-exported object was not registered: {facts:?}"
    );
}

#[test]
fn missing_source_member_is_registered_from_directive() {
    // `sphinxcontrib.websupport` is a separate, optionally installed package.
    // A fully qualified `automethod` must still register its object so refs
    // resolve, while a bare typo inside an existing module keeps warning.
    let source = fixture(&[("mypkg/__init__.py", "")]);
    let cfg = config(&["sphinx.ext.autodoc"], &[]);
    let rst = "\
.. currentmodule:: absentpkg

.. class:: WebSupport

   .. automethod:: build
";
    let xml = scribe_pseudoxml(rst, &source, &cfg);
    let facts = object_set(&xml);
    assert!(
        facts.iter().any(|(module, class, fullname)| module == "absentpkg"
            && class == "WebSupport"
            && fullname == "WebSupport.build"),
        "missing-source member was not registered: {facts:?}\n{xml}"
    );
}
