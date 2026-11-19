//! MIME type table for the Phase 28 static HTTP server.
//!
//! Kept dependency-free (no `mime_guess`) so the mapping is deterministic,
//! auditable and cheap to unit-test. Only the asset types Sphinx emits are
//! covered; everything else falls back to `application/octet-stream`.

use std::path::Path;

/// Return the HTTP `Content-Type` for `path` ( lowercase extension ).
///
/// Textual types carry an explicit `charset=utf-8`, matching what browsers
/// expect for generated Sphinx HTML/CSS/JS and avoiding locale-dependent
/// decoding.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "fjson" | "inv" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "txt" | "rst" | "md" | "py" | "tex" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "js.map" => "application/json",
        "wasm" => "application/wasm",
        "zip" => "application/zip",
        "epub" => "application/epub+zip",
        _ => "application/octet-stream",
    }
}

/// Whether a response with this content type should receive live-reload
/// injection. Only HTML pages are instrumented; CSS/JS/JSON/images are left
/// byte-identical.
pub fn is_html(mime: &str) -> bool {
    mime.starts_with("text/html")
}

#[cfg(test)]
mod tests {
    use super::{is_html, mime_for_path};
    use std::path::Path;

    #[test]
    fn html_css_and_js_have_exact_types() {
        assert_eq!(mime_for_path(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(mime_for_path(Path::new("_static/alabaster.css")), "text/css; charset=utf-8");
        assert_eq!(mime_for_path(Path::new("_static/doctools.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime_for_path(Path::new("searchindex.js")), "text/javascript; charset=utf-8");
    }

    #[test]
    fn images_fonts_and_data_have_types() {
        assert_eq!(mime_for_path(Path::new("a.SVG")), "image/svg+xml");
        assert_eq!(mime_for_path(Path::new("a.PNG")), "image/png");
        assert_eq!(mime_for_path(Path::new("objects.inv")), "application/json");
        assert_eq!(mime_for_path(Path::new("a.pdf")), "application/pdf");
        assert_eq!(mime_for_path(Path::new("a.unknown")), "application/octet-stream");
    }

    #[test]
    fn only_html_is_injected() {
        assert!(is_html(mime_for_path(Path::new("index.html"))));
        assert!(!is_html(mime_for_path(Path::new("style.css"))));
        assert!(!is_html(mime_for_path(Path::new("app.js"))));
        assert!(!is_html(mime_for_path(Path::new("objects.inv"))));
    }
}
