//! Live-reload client script and HTML injection (Phase 28).
//!
//! The dev server injects a single `<script>` tag into *HTML responses only*.
//! Injection happens at serve time rather than at build time so generated
//! files remain byte-identical to a fresh `scribe build -b html` run (the
//! integration differential test strips the injected tag before comparing).

use super::mime::is_html;

/// Path at which the live-reload client is served. Using an absolute URL keeps
/// it valid for nested pages (`sub/page.html`) without per-page path math.
pub const LIVE_SCRIPT_URL: &str = "/_static/scribe-live.js";

/// Marker used to make injection idempotent (a page already instrumented by a
/// previous proxy layer is not instrumented twice).
const INJECTION_MARKER: &str = "data-scribe-live";

/// The browser client. It opens an `EventSource` (Server-Sent Events) to
/// `/events`; on a `scribe-reload` event carrying the list of changed page
/// paths it reloads only when the current page is in that list (or when no
/// page list is supplied, e.g. a config/asset change). Errors are ignored so a
/// server restart does not produce console noise that looks like a failure.
pub const LIVE_SCRIPT: &str = r#"(function () {
  try {
    var source = new EventSource('/events');
    source.addEventListener('scribe-reload', function (event) {
      var pages = [];
      try { pages = JSON.parse(event.data || '[]'); } catch (e) { pages = []; }
      var current = location.pathname.split('/').pop();
      if (!current) { current = 'index.html'; }
      if (pages.length === 0 || pages.indexOf(current) !== -1 || pages.indexOf(location.pathname) !== -1) {
        location.reload();
      }
    });
  } catch (e) { /* live reload unavailable; page still works without it */ }
})();
"#;

/// Inject the live-reload `<script>` tag into an HTML document.
///
/// The tag is placed immediately before `</body>` when present (case
/// insensitive), otherwise appended. Injection is idempotent: if a script with
/// the scribe marker is already present the input is returned unchanged.
pub fn inject_into_html(html: &str, script_url: &str) -> String {
    if html.contains(INJECTION_MARKER) {
        return html.to_owned();
    }
    let tag = format!("<script src=\"{script_url}\" {INJECTION_MARKER}></script>");
    let lower = html.to_ascii_lowercase();
    if let Some(index) = lower.rfind("</body>") {
        let mut out = String::with_capacity(html.len() + tag.len() + 8);
        out.push_str(&html[..index]);
        out.push_str(&tag);
        out.push_str(&html[index..]);
        out
    } else {
        let mut out = String::with_capacity(html.len() + tag.len() + 1);
        out.push_str(html);
        out.push('\n');
        out.push_str(&tag);
        out
    }
}

/// Apply injection only when the response content type is HTML. Returns the
/// (possibly rewritten) body and whether injection took place.
pub fn maybe_inject(mime: &str, body: &str, script_url: &str) -> (String, bool) {
    if is_html(mime) {
        (inject_into_html(body, script_url), true)
    } else {
        (body.to_owned(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::{inject_into_html, maybe_inject, LIVE_SCRIPT_URL};

    #[test]
    fn injects_before_body_close() {
        let html = "<html><head></head><body><p>hi</p></body></html>";
        let out = inject_into_html(html, LIVE_SCRIPT_URL);
        assert!(out.contains("<p>hi</p><script src=\"/_static/scribe-live.js\""));
        assert!(out.find(LIVE_SCRIPT_URL).unwrap() < out.find("</body>").unwrap());
        assert!(out.ends_with("</body></html>"));
    }

    #[test]
    fn appends_when_no_body_tag() {
        let out = inject_into_html("<!doctype html><p>hi</p>", LIVE_SCRIPT_URL);
        assert!(out.ends_with("<script src=\"/_static/scribe-live.js\" data-scribe-live></script>"));
    }

    #[test]
    fn injection_is_idempotent() {
        let once = inject_into_html("<body>x</body>", LIVE_SCRIPT_URL);
        let twice = inject_into_html(&once, LIVE_SCRIPT_URL);
        assert_eq!(once, twice);
    }

    #[test]
    fn non_html_is_not_injected() {
        let css = "body { color: red; }";
        let (body, injected) = maybe_inject("text/css; charset=utf-8", css, LIVE_SCRIPT_URL);
        assert!(!injected);
        assert_eq!(body, css);
        let (body, injected) = maybe_inject("text/javascript; charset=utf-8", "var x=1;", LIVE_SCRIPT_URL);
        assert!(!injected);
        assert_eq!(body, "var x=1;");
    }

    #[test]
    fn html_is_injected_through_mime_gate() {
        let (body, injected) = maybe_inject("text/html; charset=utf-8", "<body>x</body>", LIVE_SCRIPT_URL);
        assert!(injected);
        assert!(body.contains(LIVE_SCRIPT_URL));
    }
}
