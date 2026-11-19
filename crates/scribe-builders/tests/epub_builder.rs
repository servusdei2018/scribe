//! Structural, ZIP, XML and differential tests for the Phase 24 EPUB builder.

mod output_common;

use std::io::Read;

use scribe_builders::epub::{
    escape_xml, render_nav_xhtml, render_toc_ncx, strip_external_refs, wrap_xhtml,
    EpubMetadata, NavPoint, EPUB_CSS, MIMETYPE,
};
use scribe_config::Config;

use output_common::{normalize_markup, oracle_build, scribe_build, temp_dir, tool, workspace_root};

fn assert_xml_well_formed(data: &[u8], label: &str) {
    let text = std::str::from_utf8(data).unwrap_or_else(|e| panic!("{label} is not UTF-8: {e}"));
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..roxmltree::ParsingOptions::default() };
    roxmltree::Document::parse_with_options(text, options)
        .unwrap_or_else(|e| panic!("{label} is not well-formed XML: {e}"));
}

#[test]
fn escape_xml_covers_all_predefined_entities() {
    assert_eq!(escape_xml("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
}

#[test]
fn strip_external_refs_keeps_internal_links() {
    let html = "<a href=\"https://example.com\">x</a><a href=\"#intro\">y</a><a href=\"other.xhtml\">z</a>";
    let out = strip_external_refs(html);
    assert!(out.contains("href=\"#\""), "{out}");
    assert!(out.contains("href=\"#intro\""), "{out}");
    assert!(out.contains("href=\"other.xhtml\""), "{out}");
    assert!(!out.contains("https://example.com"), "{out}");
}

#[test]
fn wrap_xhtml_inlines_css_and_is_xml() {
    let xhtml = wrap_xhtml("<p>Hello &amp; goodbye</p>", "A & B", "en");
    assert!(xhtml.contains("<style type=\"text/css\">"), "{xhtml}");
    assert!(xhtml.contains(EPUB_CSS.trim()), "{xhtml}");
    assert!(xhtml.contains("A &amp; B"), "{xhtml}");
    assert!(xhtml.contains("xmlns:epub=\"http://www.idpf.org/2007/ops\""), "{xhtml}");
    assert_xml_well_formed(xhtml.as_bytes(), "wrapped XHTML");
}

#[test]
fn nav_and_ncx_are_well_formed_and_nested() {
    let config = Config::new();
    let metadata = EpubMetadata::from_config(&config);
    let nav = vec![NavPoint {
        text: "Chapter & friends".to_owned(),
        href: "index.xhtml".to_owned(),
        children: vec![NavPoint {
            text: "Sub <section>".to_owned(),
            href: "index.xhtml#sub".to_owned(),
            children: Vec::new(),
        }],
    }];
    let xhtml = render_nav_xhtml(&nav, &metadata);
    assert!(xhtml.contains("epub:type=\"toc\""), "{xhtml}");
    assert!(xhtml.contains("Chapter &amp; friends"), "{xhtml}");
    assert_xml_well_formed(xhtml.as_bytes(), "nav.xhtml");

    let ncx = render_toc_ncx(&nav, &metadata);
    assert!(ncx.contains("playOrder=\"1\""), "{ncx}");
    assert!(ncx.contains("playOrder=\"2\""), "{ncx}");
    assert_xml_well_formed(ncx.as_bytes(), "toc.ncx");
}

#[test]
fn empty_document_produces_structured_epub() {
    let config = Config::new();
    let metadata = EpubMetadata::from_config(&config);
    // `Config::new()` resolves the computed `project` default.
    assert_eq!(metadata.basename, "projectnamenotset");
    assert_eq!(metadata.language, "en");
    assert_eq!(MIMETYPE, b"application/epub+zip");
}

/// **Validator gate:** EPUB ZIP structure must be valid: `zip -T` succeeds,
/// `mimetype` is the first entry, STORED, and has the exact expected bytes.
#[test]
fn epub_zip_structure_and_validators() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-toctree");
    let out = temp_dir("epub-validator");
    let result = scribe_build("epub", &fixture, &out, &[]);
    assert!(result.is_ok(), "epub build failed: {:?}", result.err());

    let epub = out.join("projectnamenotset.epub");
    assert!(epub.is_file(), "epub archive missing");
    let checked = tool("/usr/sbin/zip", &["-T", epub.to_str().expect("utf8 path")]);
    assert!(checked.status.success(), "zip -T failed: {}", String::from_utf8_lossy(&checked.stderr));

    // Parse the local file header of the first entry directly so we assert the
    // STORED method and exact bytes without depending on an external tool.
    let mut file = std::fs::File::open(&epub).expect("open epub");
    let mut header = [0u8; 30];
    file.read_exact(&mut header).expect("zip local header");
    assert_eq!(&header[0..4], b"PK\x03\x04");
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as usize;
    let mut name = vec![0u8; name_len];
    file.read_exact(&mut name).expect("entry name");
    assert_eq!(name, b"mimetype", "mimetype must be the first ZIP entry");
    let method = u16::from_le_bytes([header[8], header[9]]);
    assert_eq!(method, 0, "mimetype must be STORED, not deflated");
    let mut extra = vec![0u8; extra_len];
    file.read_exact(&mut extra).expect("entry extra");
    let mut mimetype = vec![0u8; MIMETYPE.len()];
    file.read_exact(&mut mimetype).expect("mimetype bytes");
    assert_eq!(mimetype, MIMETYPE);

    // Every XML/markup file in the archive must be well-formed.
    let python = r#"
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
out = sys.stdout.buffer
for name in z.namelist():
    if name.endswith(('.opf', '.ncx', '.xml', '.xhtml')):
        out.write(name.encode() + b"\0" + z.read(name) + b"\0")
"#;
    let script = temp_dir("epub-xml-script").join("dump.py");
    std::fs::write(&script, python).expect("write python script");
    let dumped = tool("python3", &[script.to_str().expect("utf8 path"), epub.to_str().expect("utf8 path")]);
    assert!(dumped.status.success(), "python XML dump failed: {}", String::from_utf8_lossy(&dumped.stderr));
    let chunks: Vec<&[u8]> = dumped.stdout.split(|b| *b == 0).collect();
    let mut checked_files = 0usize;
    let mut index = 0usize;
    while index + 1 < chunks.len() {
        let name = String::from_utf8_lossy(chunks[index]);
        let data = chunks[index + 1];
        if !name.is_empty() && !data.is_empty() {
            assert_xml_well_formed(data, &name);
            checked_files += 1;
        }
        index += 2;
    }
    assert!(checked_files >= 4, "expected at least container/opf/ncx/nav XML files, got {checked_files}");
}

#[test]
fn differential_epub_structure() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-toctree");
    let oracle_out = temp_dir("oracle-epub-diff");
    let status = oracle_build("epub", &fixture, &oracle_out);
    assert!(status.status.success(), "oracle epub build failed");
    let out = temp_dir("scribe-epub-diff");
    let result = scribe_build("epub", &fixture, &out, &[]);
    assert!(result.is_ok());

    let expected_opf = std::fs::read_to_string(oracle_out.join("content.opf")).expect("oracle opf");
    let actual_opf = std::fs::read_to_string(out.join("OEBPS/content.opf")).expect("scribe opf");
    for marker in ["<package", "<metadata", "dc:title", "dc:creator", "<manifest", "<spine", "toc.ncx", "nav.xhtml"] {
        assert!(expected_opf.contains(marker), "oracle opf lacks {marker}");
        assert!(actual_opf.contains(marker), "scribe opf lacks {marker}");
    }
    assert!(normalize_markup(&actual_opf).contains("application/xhtml+xml"));

    let expected_nav = std::fs::read_to_string(oracle_out.join("nav.xhtml")).expect("oracle nav");
    let actual_nav = std::fs::read_to_string(out.join("OEBPS/nav.xhtml")).expect("scribe nav");
    for marker in ["epub:type=\"toc\"", "<ol>", "<li>", "<a href="] {
        assert!(expected_nav.contains(marker), "oracle nav lacks {marker}");
        assert!(actual_nav.contains(marker), "scribe nav lacks {marker}");
    }

    // Determinism: two builds must produce byte-identical archives.
    let out2 = temp_dir("scribe-epub-diff2");
    let result2 = scribe_build("epub", &fixture, &out2, &[]);
    assert!(result2.is_ok());
    let a = std::fs::read(out.join("projectnamenotset.epub")).expect("epub a");
    let b = std::fs::read(out2.join("projectnamenotset.epub")).expect("epub b");
    assert_eq!(a, b, "EPUB archives must be byte-identical across builds");
}
