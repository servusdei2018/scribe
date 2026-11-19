//! Deterministic Pygments-compatible stylesheet generation (Phase 15).
//!
//! Sphinx writes a `pygments.css` file whose rules are produced by Pygments'
//! `HtmlFormatter.get_style_defs('.highlight')`. Calling Python at build time
//! would defeat the purpose of a native reimplementation, so the exact CSS
//! emitted by the pinned oracle (Pygments 2.21 / Sphinx 9.3) is captured at
//! development time and embedded verbatim as static files under [`css/`].
//! [`pygments_css`] performs a pure, deterministic lookup — no Python, no
//! filesystem access, no nondeterminism (no timestamps, no hashes).
//!
//! The emitted selectors (`.k`, `.c1`, `.s2`, `.nf`, `.hll`, `.linenos`, …) are
//! exactly the class names produced by [`crate::highlight`], so a Phase 17 HTML
//! writer can link the returned string as-is.

/// Styles with a captured stylesheet, in deterministic (sorted) order.
pub const STYLES: &[&str] = &[
    "autumn", "borland", "colorful", "default", "emacs", "friendly", "fruity", "lovelace",
    "monokai", "native", "none", "pastie", "sphinx", "tango", "trac", "vim",
];

/// Returns the styles understood by [`pygments_css`].
pub fn available_styles() -> &'static [&'static str] {
    STYLES
}

/// Returns the complete `pygments.css` text for `style`.
///
/// `style` is matched case-insensitively; an empty name and `"sphinx"` both
/// select the Sphinx default style (Sphinx maps a missing/`None`
/// `pygments_style` to `SphinxStyle`). Unknown style names fall back to
/// `"sphinx"` rather than failing a build, mirroring the highlighter's
/// graceful handling of unknown lexers.
///
/// The result is a pure function of `style`: calling it twice with the same
/// input always yields byte-identical output.
pub fn pygments_css(style: &str) -> String {
    let normalized = style.trim().to_ascii_lowercase();
    let selected = match normalized.as_str() {
        "" | "sphinx" => "sphinx",
        other if STYLES.contains(&other) => other,
        _ => "sphinx",
    };
    stylesheet(selected).to_owned()
}

fn stylesheet(style: &str) -> &'static str {
    match style {
        "autumn" => include_str!("css/autumn.css"),
        "borland" => include_str!("css/borland.css"),
        "colorful" => include_str!("css/colorful.css"),
        "default" => include_str!("css/default.css"),
        "emacs" => include_str!("css/emacs.css"),
        "friendly" => include_str!("css/friendly.css"),
        "fruity" => include_str!("css/fruity.css"),
        "lovelace" => include_str!("css/lovelace.css"),
        "monokai" => include_str!("css/monokai.css"),
        "native" => include_str!("css/native.css"),
        "none" => include_str!("css/none.css"),
        "pastie" => include_str!("css/pastie.css"),
        "sphinx" => include_str!("css/sphinx.css"),
        "tango" => include_str!("css/tango.css"),
        "trac" => include_str!("css/trac.css"),
        "vim" => include_str!("css/vim.css"),
        _ => include_str!("css/sphinx.css"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_prefixed() {
        let a = pygments_css("sphinx");
        let b = pygments_css("sphinx");
        assert_eq!(a, b);
        assert!(a.starts_with("pre { line-height: 125%; }"));
        assert!(a.contains(".highlight .k "));
        assert!(a.contains(".highlight .c1 "));
        assert!(a.contains(".highlight .hll "));
    }

    #[test]
    fn aliases_and_unknown_fall_back_to_sphinx() {
        assert_eq!(pygments_css(""), pygments_css("sphinx"));
        assert_eq!(pygments_css("SPHINX"), pygments_css("sphinx"));
        assert_eq!(pygments_css("does-not-exist"), pygments_css("sphinx"));
        assert_ne!(pygments_css("monokai"), pygments_css("sphinx"));
    }

    #[test]
    fn every_advertised_style_is_renderable() {
        for style in available_styles() {
            let css = pygments_css(style);
            assert!(css.contains(".highlight"), "style {style}");
        }
    }
}
