//! Directory-style HTML builder (Phase 19).
//!
//! `DirectoryHTMLBuilder` is upstream's `StandaloneHTMLBuilder` subclass that
//! writes every page as `<name>/index.html` and computes clean target URIs:
//!
//! * `index` → `""`
//! * a docname ending in `/index` → the docname without that suffix
//! * everything else → `<docname>/`
//!
//! The builder reuses the Phase 17 pipeline through
//! [`crate::html::builder::build_with_flavor`], so translation, assets, search
//! and domain handling are shared byte-for-byte with the standalone builder;
//! only `pathto`/output paths change.

use std::path::{Path, PathBuf};

use scribe_config::Config;
use scribe_core::BuildEnvironment;

use crate::error::Result;
use crate::html::builder::{build_with_flavor, BuildFlavor};
use crate::html::{BuildOptions, BuildResult};

/// Stable identity of this builder.
pub const BUILDER_NAME: &str = "dirhtml";

/// `DirectoryHTMLBuilder.get_target_uri` — exact upstream semantics.
///
/// `SEP` is always `/` in scribe (docs are identified with POSIX separators).
#[must_use]
pub fn target_uri(docname: &str) -> String {
    if docname == "index" {
        String::new()
    } else if docname.ends_with("/index") {
        // Upstream uses `docname[:-5]`, dropping only `index` and keeping the
        // trailing separator: `foo/index` -> `foo/`.
        docname[..docname.len() - 5].to_owned()
    } else {
        format!("{docname}/")
    }
}

/// `DirectoryHTMLBuilder.get_output_path` — exact upstream semantics.
///
/// The trailing `index` component is dropped so `foo/index` and `foo` both
/// resolve to `foo/index.html`.
#[must_use]
pub fn output_path(page_name: &str, out_suffix: &str) -> PathBuf {
    let mut parts: Vec<&str> = page_name.split('/').collect();
    if parts.last() == Some(&"index") {
        parts.pop();
    }
    let mut path = PathBuf::new();
    for part in parts {
        path.push(part);
    }
    path.push(format!("index{out_suffix}"));
    path
}

/// Directory-style HTML builder.
#[derive(Debug, Default)]
pub struct DirectoryHTMLBuilder;

impl DirectoryHTMLBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self { Self }

    /// Run a complete `dirhtml` build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> Result<BuildResult> {
        build_with_flavor(options, config, env, &BuildFlavor::dirhtml())
    }
}

/// Resolve a resource URI relative to the page at `docname` under dirhtml.
///
/// This is used by callers that need a `src`/`href` directly (the theme's
/// `pathto(..., 1)` already does the equivalent through `RenderContext`).
#[must_use]
pub fn relative_resource(docname: &str, resource: &str) -> String {
    if resource.contains("://") || resource.starts_with("data:") {
        return resource.to_owned();
    }
    let base = target_uri(docname);
    scribe_theme::relative_uri(&base, resource)
}

/// Return `true` when `path` is the output file for `docname` under dirhtml.
#[must_use]
pub fn is_page_output(docname: &str, path: &Path) -> bool {
    path == output_path(docname, ".html")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_uri_matches_upstream_rules() {
        assert_eq!(target_uri("index"), "");
        assert_eq!(target_uri("foo/index"), "foo/");
        assert_eq!(target_uri("a/b/index"), "a/b/");
        assert_eq!(target_uri("bar"), "bar/");
        assert_eq!(target_uri("foo/bar"), "foo/bar/");
    }

    #[test]
    fn output_path_matches_upstream_rules() {
        assert_eq!(output_path("index", ".html"), PathBuf::from("index.html"));
        assert_eq!(output_path("bar", ".html"), PathBuf::from("bar/index.html"));
        assert_eq!(output_path("foo/index", ".html"), PathBuf::from("foo/index.html"));
        assert_eq!(output_path("foo/foo_1", ".html"), PathBuf::from("foo/foo_1/index.html"));
        assert_eq!(output_path("genindex", ".fjson"), PathBuf::from("genindex/index.fjson"));
    }

    #[test]
    fn relative_resource_is_page_relative() {
        assert_eq!(relative_resource("index", "_static/a.css"), "_static/a.css");
        assert_eq!(relative_resource("foo/index", "_static/a.css"), "../_static/a.css");
        assert_eq!(relative_resource("bar", "_images/x.png"), "../_images/x.png");
        assert_eq!(relative_resource("index", "https://example.com/a"), "https://example.com/a");
    }
}
