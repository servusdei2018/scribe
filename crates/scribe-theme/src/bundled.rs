//! Location of the vendored built-in themes.
//!
//! The theme assets are shipped inside the crate source tree so that the same
//! files are available to unit tests, integration tests and the Phase 17 HTML
//! writer without relying on a separate install step.  `theme_static_files`
//! returns real filesystem paths into this directory, which is what asset
//! copying needs.

use std::path::PathBuf;

use once_cell::sync::Lazy;

/// Absolute path of the bundled `themes/` directory, resolved at compile time.
pub static BUNDLED_THEME_ROOT: Lazy<PathBuf> =
    Lazy::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes"));

/// Names of the themes bundled with scribe, in deterministic order.
pub const BUNDLED_THEME_NAMES: [&str; 4] = ["alabaster", "basic", "classic", "nature"];

/// Return the directory of a bundled theme by name.
#[must_use]
pub fn bundled_theme_dir(name: &str) -> Option<PathBuf> {
    if BUNDLED_THEME_NAMES.contains(&name) {
        Some(BUNDLED_THEME_ROOT.join(name))
    } else {
        None
    }
}
