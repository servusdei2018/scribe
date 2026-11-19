//! Napoleon conversion tests covering every documented section (Phase 21).

use scribe_autodoc::napoleon::{convert_docstring, NapoleonOptions};

fn convert(doc: &str) -> String {
    convert_docstring(doc, &NapoleonOptions::default())
}

#[test]
fn google_all_sections() {
    let doc = "Summary.\n\n\
Args:\n    x (int): the x\n    y: the y\n\n\
Keyword Args:\n    k (str): the k\n\n\
Returns:\n    int: the result\n\n\
Yields:\n    str: a chunk\n\n\
Raises:\n    ValueError: bad\n\n\
Attributes:\n    attr (int): an attribute\n\n\
Examples:\n    >>> f()\n\n\
Note:\n    a note\n\n\
See Also:\n    other\n\n\
Warnings:\n    careful\n";
    let rst = convert(doc);
    for needle in [
        ":param x: the x", ":type x: int", ":param y: the y",
        ":keyword k: the k", ":type k: str",
        ":returns: the result", ":rtype: int",
        ":Yields: *str* -- a chunk",
        ":raises ValueError: bad",
        ".. attribute:: attr",
        ".. rubric:: Examples", ".. note:: a note",
        ".. seealso:: other", ".. warning:: careful",
    ] {
        assert!(rst.contains(needle), "missing {needle:?} in:\n{rst}");
    }
}

#[test]
fn numpy_all_sections() {
    let doc = "Summary.\n\n\
Parameters\n----------\nx : int\n    the x\n\n\
Other Parameters\n----------------\nkw : str\n    the kw\n\n\
Returns\n-------\nint\n    the result\n\n\
Raises\n------\nValueError\n    bad\n\n\
Notes\n-----\na note\n\n\
See Also\n--------\nother : thing\n\n\
Warnings\n--------\ncareful\n\n\
Examples\n--------\n>>> f()\n";
    let rst = convert(doc);
    for needle in [
        ":param x: the x", ":type x: int",
        ":param kw: the kw", ":type kw: str",
        ":returns: the result",
        ":raises ValueError: bad",
        ".. rubric:: Notes",
        ".. seealso::", ":py:obj:`other`",
        ".. warning:: careful",
        ".. rubric:: Examples",
    ] {
        assert!(rst.contains(needle), "missing {needle:?} in:\n{rst}");
    }
}

#[test]
fn google_word_boundary_sections() {
    assert!(convert("A.\n\nArguments:\n    a: A\n").contains(":param a: A"));
    assert!(convert("A.\n\nParameters:\n    a: A\n").contains(":param a: A"));
    assert!(convert("A.\n\nWarning:\n    careful\n").contains(".. warning:: careful"));
    assert!(convert("A.\n\nWarns:\n    UserWarning: hmm\n").contains(":Warns:"));
}

#[test]
fn toggles_change_output() {
    let doc = "S.\n\nAttributes:\n    a: A\n\nNotes:\n    n\n";
    let options = NapoleonOptions {
        use_ivar: true,
        use_admonition_for_notes: true,
        ..NapoleonOptions::default()
    };
    let rst = convert_docstring(doc, &options);
    assert!(rst.contains(":ivar a: A"), "{rst}");
    assert!(rst.contains(".. note:: n"), "{rst}");
    assert!(!rst.contains(".. attribute::"), "{rst}");
}

#[test]
fn plain_docstring_is_preserved() {
    let doc = "A plain summary.\n\n:param x: value\n";
    assert_eq!(convert(doc), doc.trim_end());
}
