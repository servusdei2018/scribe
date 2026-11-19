//! Phase 41 end-to-end parity checks for UTF-8 BOM stripping, `.. title::`
//! metadata and Sphinx docinfo field stripping.
//!
//! These drive the real `scribe` binary on a minimal project and assert the
//! same observable outcomes as `sphinx-build -b pseudoxml` / `-b html`:
//! * the BOM never reaches the doctree;
//! * an explicit `.. title::` becomes `<document title="...">` and the HTML
//!   `<title>` tag, without leaving a stray node;
//! * the leading docinfo field list is collected into document metadata and
//!   removed from both the doctree and the rendered page.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-phase41-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).expect("create temp project");
        fs::write(
            root.join("src/conf.py"),
            "project = 'Probe'\nextensions = []\nmaster_doc = 'index'\nexclude_patterns = []\n",
        )
        .expect("write conf.py");
        Self { root }
    }

    fn source(&self) -> PathBuf {
        self.root.join("src")
    }

    fn output(&self, builder: &str) -> PathBuf {
        self.root.join(format!("out-{builder}"))
    }

    fn build(&self, builder: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_scribe"))
            .args([
                "build",
                "-b",
                builder,
                "-q",
                "-E",
                self.source().to_str().expect("utf-8 path"),
                self.output(builder).to_str().expect("utf-8 path"),
            ])
            .output()
            .expect("run scribe binary")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn bom_title_and_docinfo_match_upstream_observables() {
    let project = TempProject::new("parity");
    // A literal UTF-8 BOM prefix, an explicit title, and Sphinx docinfo fields.
    let source = concat!(
        "\u{feff}.. title:: My Override Title\n",
        "\n",
        ":tocdepth: 2\n",
        ":orphan:\n",
        ":nocomments:\n",
        ":nosearch:\n",
        "\n",
        "My Section Title\n",
        "================\n",
        "\n",
        "Body text.\n",
    );
    fs::write(project.source().join("index.rst"), source).expect("write index.rst");

    // --- pseudo-XML doctree -------------------------------------------------
    let out = project.build("pseudoxml");
    assert!(out.status.success(), "pseudoxml build failed: {}", String::from_utf8_lossy(&out.stderr));
    let pseudo = fs::read_to_string(project.output("pseudoxml").join("index.pseudoxml"))
        .expect("read pseudoxml");
    assert!(!pseudo.contains('\u{feff}'), "BOM leaked into doctree: {pseudo}");
    assert!(pseudo.contains("title=\"My Override Title\""), "{pseudo}");
    assert!(pseudo.contains("My Section Title"), "{pseudo}");
    assert!(!pseudo.contains("tocdepth"), "{pseudo}");
    assert!(!pseudo.contains("field_list"), "{pseudo}");

    // --- HTML ---------------------------------------------------------------
    let out = project.build("html");
    assert!(out.status.success(), "html build failed: {}", String::from_utf8_lossy(&out.stderr));
    let html = fs::read_to_string(project.output("html").join("index.html")).expect("read html");
    assert!(
        html.contains("<title>My Override Title "),
        "HTML title did not use the directive override: {html}"
    );
    assert!(!html.contains("tocdepth"), "{html}");
    assert!(!html.contains("nocomments"), "{html}");
    assert!(!html.contains("nosearch"), "{html}");
}

#[test]
fn bom_prefix_does_not_change_doctree_structure() {
    let a = TempProject::new("plain");
    fs::write(a.source().join("index.rst"), "Title\n=====\n\nBody.\n").expect("write");
    assert!(a.build("pseudoxml").status.success());
    let plain = fs::read_to_string(a.output("pseudoxml").join("index.pseudoxml")).expect("read");

    let b = TempProject::new("bom");
    fs::write(b.source().join("index.rst"), "\u{feff}Title\n=====\n\nBody.\n").expect("write");
    assert!(b.build("pseudoxml").status.success());
    let with_bom = fs::read_to_string(b.output("pseudoxml").join("index.pseudoxml")).expect("read");

    // The two builds use different temp directories, so ignore the absolute
    // `source="..."` attribute (everything else must be byte-identical).
    let normalize = |text: &str| {
        text.lines()
            .map(|line| {
                if let Some(rest) = line.strip_prefix("<document source=\"") {
                    if let Some(end) = rest.find('"') {
                        return format!("<document source=\"<path>{}", &rest[end..]);
                    }
                }
                line.to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(normalize(&plain), normalize(&with_bom));
}
