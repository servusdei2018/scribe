//! Vendored LaTeX support files from Sphinx (see ATTRIBUTION.md).
//! These are embedded at compile time and written next to generated `.tex`
//! files so the advanced `use_sphinx_sty` mode stays self-contained.

pub const PYTHON_IST: &str = include_str!("texinputs/python.ist");
pub const SPHINX_STY: &str = include_str!("texinputs/sphinx.sty");
pub const SPHINXHOWTO_CLS: &str = include_str!("texinputs/sphinxhowto.cls");
pub const SPHINXLATEXADMONITIONS_STY: &str = include_str!("texinputs/sphinxlatexadmonitions.sty");
pub const SPHINXLATEXCONTAINERS_STY: &str = include_str!("texinputs/sphinxlatexcontainers.sty");
pub const SPHINXLATEXGRAPHICS_STY: &str = include_str!("texinputs/sphinxlatexgraphics.sty");
pub const SPHINXLATEXINDBIBTOC_STY: &str = include_str!("texinputs/sphinxlatexindbibtoc.sty");
pub const SPHINXLATEXLISTS_STY: &str = include_str!("texinputs/sphinxlatexlists.sty");
pub const SPHINXLATEXLITERALS_STY: &str = include_str!("texinputs/sphinxlatexliterals.sty");
pub const SPHINXLATEXNUMFIG_STY: &str = include_str!("texinputs/sphinxlatexnumfig.sty");
pub const SPHINXLATEXOBJECTS_STY: &str = include_str!("texinputs/sphinxlatexobjects.sty");
pub const SPHINXLATEXSHADOWBOX_STY: &str = include_str!("texinputs/sphinxlatexshadowbox.sty");
pub const SPHINXLATEXSTYLEHEADINGS_STY: &str = include_str!("texinputs/sphinxlatexstyleheadings.sty");
pub const SPHINXLATEXSTYLEPAGE_STY: &str = include_str!("texinputs/sphinxlatexstylepage.sty");
pub const SPHINXLATEXSTYLETEXT_STY: &str = include_str!("texinputs/sphinxlatexstyletext.sty");
pub const SPHINXLATEXTABLES_STY: &str = include_str!("texinputs/sphinxlatextables.sty");
pub const SPHINXMANUAL_CLS: &str = include_str!("texinputs/sphinxmanual.cls");
pub const SPHINXOPTIONSGEOMETRY_STY: &str = include_str!("texinputs/sphinxoptionsgeometry.sty");
pub const SPHINXOPTIONSHYPERREF_STY: &str = include_str!("texinputs/sphinxoptionshyperref.sty");
pub const SPHINXPACKAGEBOXES_STY: &str = include_str!("texinputs/sphinxpackageboxes.sty");
pub const SPHINXPACKAGECYRILLIC_STY: &str = include_str!("texinputs/sphinxpackagecyrillic.sty");
pub const SPHINXPACKAGEFOOTNOTE_STY: &str = include_str!("texinputs/sphinxpackagefootnote.sty");
pub const SPHINXPACKAGESUBSTITUTEFONT_STY: &str = include_str!("texinputs/sphinxpackagesubstitutefont.sty");

/// All vendored support files, as `(file_name, contents)` pairs.
pub static SUPPORT_FILES: &[(&str, &str)] = &[
    ("python.ist", PYTHON_IST),
    ("sphinx.sty", SPHINX_STY),
    ("sphinxhowto.cls", SPHINXHOWTO_CLS),
    ("sphinxlatexadmonitions.sty", SPHINXLATEXADMONITIONS_STY),
    ("sphinxlatexcontainers.sty", SPHINXLATEXCONTAINERS_STY),
    ("sphinxlatexgraphics.sty", SPHINXLATEXGRAPHICS_STY),
    ("sphinxlatexindbibtoc.sty", SPHINXLATEXINDBIBTOC_STY),
    ("sphinxlatexlists.sty", SPHINXLATEXLISTS_STY),
    ("sphinxlatexliterals.sty", SPHINXLATEXLITERALS_STY),
    ("sphinxlatexnumfig.sty", SPHINXLATEXNUMFIG_STY),
    ("sphinxlatexobjects.sty", SPHINXLATEXOBJECTS_STY),
    ("sphinxlatexshadowbox.sty", SPHINXLATEXSHADOWBOX_STY),
    ("sphinxlatexstyleheadings.sty", SPHINXLATEXSTYLEHEADINGS_STY),
    ("sphinxlatexstylepage.sty", SPHINXLATEXSTYLEPAGE_STY),
    ("sphinxlatexstyletext.sty", SPHINXLATEXSTYLETEXT_STY),
    ("sphinxlatextables.sty", SPHINXLATEXTABLES_STY),
    ("sphinxmanual.cls", SPHINXMANUAL_CLS),
    ("sphinxoptionsgeometry.sty", SPHINXOPTIONSGEOMETRY_STY),
    ("sphinxoptionshyperref.sty", SPHINXOPTIONSHYPERREF_STY),
    ("sphinxpackageboxes.sty", SPHINXPACKAGEBOXES_STY),
    ("sphinxpackagecyrillic.sty", SPHINXPACKAGECYRILLIC_STY),
    ("sphinxpackagefootnote.sty", SPHINXPACKAGEFOOTNOTE_STY),
    ("sphinxpackagesubstitutefont.sty", SPHINXPACKAGESUBSTITUTEFONT_STY),
];
