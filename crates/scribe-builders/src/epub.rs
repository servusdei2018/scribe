//! EPUB3 builder (Phase 24).
//!
//! Reuses the Phase 17 HTML5 translator to render XHTML5 content documents,
//! then assembles a standards-compliant EPUB container:
//!
//! * `mimetype` — the **first** ZIP entry, STORED (uncompressed), with the
//!   exact bytes `application/epub+zip`,
//! * `META-INF/container.xml`,
//! * `OEBPS/content.opf` (manifest + spine + Dublin Core metadata),
//! * `OEBPS/toc.ncx` (EPUB 2 navigation) and `OEBPS/nav.xhtml` (EPUB 3),
//! * one `<docname>.xhtml` per source, ordered by the toctree,
//! * a minimal vendored stylesheet with the CSS inlined into each document.
//!
//! Every ZIP entry uses a fixed DOS timestamp so repeated builds are
//! byte-identical. `epubcheck` is not installed in the validation environment;
//! the gate tests therefore verify the ZIP structure with `zip -T` and the
//! EPUB invariants (first STORED mimetype with exact bytes) plus XML
//! well-formedness, which is documented as a deliberate divergence.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use scribe_config::Config;
use scribe_theme::RenderContext;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::build_common::{prepare, extract_title, PreparedBuild};
use crate::error::{OutputError, OutputResult};
use crate::html::{BuildOptions, BuildResult, HtmlContext, HTML5Translator};

/// Stable identity of the EPUB builder.
pub const BUILDER_NAME: &str = "epub";
/// Alias matching upstream's `epub3` builder name.
pub const EPUB3_BUILDER_NAME: &str = "epub3";

/// Exact contents of the `mimetype` file (no trailing newline).
pub const MIMETYPE: &[u8] = b"application/epub+zip";

/// Minimal vendored EPUB stylesheet, inlined into each XHTML document.
pub const EPUB_CSS: &str = r#"body{font-family:serif;line-height:1.4;margin:1em}
h1,h2,h3,h4{font-family:sans-serif;margin-top:1.2em}
pre,code,.pre{font-family:monospace;white-space:pre-wrap}
div.admonition,div.note,div.warning,div.tip{border:1px solid #888;padding:.5em;margin:1em 0}
a{color:#0645ad;text-decoration:none}
table{border-collapse:collapse}td,th{border:1px solid #888;padding:.3em}
"#;

const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#;

/// EPUB metadata derived from configuration.
#[derive(Clone, Debug)]
pub struct EpubMetadata {
    pub title: String,
    pub author: String,
    pub copyright: String,
    pub uid: String,
    pub publisher: String,
    pub language: String,
    pub description: String,
    pub contributor: String,
    pub identifier: String,
    pub scheme: String,
    pub version: String,
    pub epub_version: String,
    pub date: String,
    pub basename: String,
    /// Fixed timestamp used for every ZIP entry.
    pub timestamp: DateTime,
}

impl EpubMetadata {
    /// Derive EPUB metadata from configuration with deterministic defaults.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let project = config.get_str("project").unwrap_or("Project name not set");
        let author = config.get_str("epub_author").unwrap_or_else(|| config.get_str("author").unwrap_or("Author name not set"));
        let timestamp = fixed_timestamp();
        let date = timestamp_to_rfc3339(&timestamp);
        Self {
            title: config.get_str("epub_title").unwrap_or(project).to_owned(),
            author: author.to_owned(),
            copyright: config.get_str("epub_copyright").or_else(|| config.get_str("copyright")).unwrap_or("").to_owned(),
            uid: config.get_str("epub_uid").filter(|s| !s.is_empty()).unwrap_or("unknown").to_owned(),
            publisher: config.get_str("epub_publisher").filter(|s| !s.is_empty()).unwrap_or(author).to_owned(),
            language: config.get_str("epub_language").filter(|s| !s.is_empty()).unwrap_or("en").to_owned(),
            description: config.get_str("epub_description").unwrap_or("unknown").to_owned(),
            contributor: config.get_str("epub_contributor").unwrap_or("unknown").to_owned(),
            identifier: config.get_str("epub_identifier").unwrap_or("unknown").to_owned(),
            scheme: config.get_str("epub_scheme").unwrap_or("unknown").to_owned(),
            version: config.get_str("version").unwrap_or("").to_owned(),
            epub_version: config.get_str("epub_version").unwrap_or("3.0").to_owned(),
            date,
            basename: config
                .get_str("epub_basename")
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| make_filename_from_project(project)),
            timestamp,
        }
    }
}

fn fixed_timestamp() -> DateTime {
    // DOS timestamps cannot represent arbitrary Unix epochs without the
    // `time` crate; a fixed 1980-01-01 value keeps every archive byte-stable.
    DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap_or_default()
}

fn timestamp_to_rfc3339(date: &DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        date.year(),
        date.month(),
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    )
}

fn make_filename_from_project(project: &str) -> String {
    let project = project.strip_suffix(" Documentation").unwrap_or(project);
    let name: String = project.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if name.is_empty() { "sphinx".to_owned() } else { name.to_lowercase() }
}

/// Escape XML text/attribute values.
#[must_use]
pub fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Strip external `href`/`src` references from a rendered HTML fragment so the
/// EPUB is self-contained. Internal anchors and data URIs are kept.
#[must_use]
pub fn strip_external_refs(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'h' && html[index..].starts_with("href=\"") {
            index += 6;
            let start = index;
            while index < bytes.len() && bytes[index] != b'"' {
                index += 1;
            }
            let value = &html[start..index];
            if value.contains("://") || value.starts_with("mailto:") {
                out.push_str("href=\"#\"");
            } else {
                out.push_str("href=\"");
                out.push_str(value);
                out.push('"');
            }
            if index < bytes.len() {
                index += 1;
            }
            continue;
        }
        let c = html[index..].chars().next().unwrap_or('\0');
        out.push(c);
        index += c.len_utf8();
    }
    out
}

/// Wrap an HTML body fragment in a self-contained XHTML5 document.
#[must_use]
pub fn wrap_xhtml(body: &str, title: &str, language: &str) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE html>\n");
    out.push_str(&format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"{}\" xml:lang=\"{}\">\n",
        escape_xml(language), escape_xml(language)
    ));
    out.push_str("<head>\n");
    out.push_str(&format!("<title>{}</title>\n", escape_xml(title)));
    out.push_str("<style type=\"text/css\">\n");
    out.push_str(EPUB_CSS);
    out.push_str("\n</style>\n");
    out.push_str("</head>\n<body>\n<div class=\"document\">\n");
    out.push_str(&strip_external_refs(body));
    out.push_str("\n</div>\n</body>\n</html>\n");
    out
}

/// One navigation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavPoint {
    /// Link text.
    pub text: String,
    /// Relative XHTML URI with optional fragment.
    pub href: String,
    /// Nested entries.
    pub children: Vec<NavPoint>,
}

/// Build a document-level navigation tree from the toctree hierarchy.
#[must_use]
pub fn build_nav(
    root: &str,
    hierarchy: &scribe_core::ToctreeHierarchy,
    titles: &BTreeMap<String, String>,
    maxdepth: usize,
) -> Vec<NavPoint> {
    fn walk(
        docname: &str,
        hierarchy: &scribe_core::ToctreeHierarchy,
        titles: &BTreeMap<String, String>,
        depth: usize,
        maxdepth: usize,
        seen: &mut std::collections::BTreeSet<String>,
    ) -> Vec<NavPoint> {
        if depth > maxdepth || !seen.insert(docname.to_owned()) {
            return Vec::new();
        }
        let text = titles.get(docname).cloned().unwrap_or_else(|| docname.to_owned());
        let children: Vec<NavPoint> = hierarchy
            .children(docname)
            .iter()
            .flat_map(|child| walk(child, hierarchy, titles, depth + 1, maxdepth, seen))
            .collect();
        vec![NavPoint { text, href: format!("{docname}.xhtml"), children }]
    }
    let mut seen = std::collections::BTreeSet::new();
    walk(root, hierarchy, titles, 1, maxdepth, &mut seen)
}

/// Render the EPUB3 navigation document.
#[must_use]
pub fn render_nav_xhtml(nav: &[NavPoint], metadata: &EpubMetadata) -> String {
    fn render_list(nav: &[NavPoint], indent: usize, out: &mut String) {
        let pad = " ".repeat(indent);
        out.push_str(&format!("{pad}<ol>\n"));
        for point in nav {
            out.push_str(&format!("{pad}  <li>\n"));
            out.push_str(&format!(
                "{pad}    <a href=\"{}\">{}</a>\n",
                escape_xml(&point.href),
                escape_xml(&point.text)
            ));
            if !point.children.is_empty() {
                render_list(&point.children, indent + 4, out);
            }
            out.push_str(&format!("{pad}  </li>\n"));
        }
        out.push_str(&format!("{pad}</ol>\n"));
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE html>\n");
    out.push_str(&format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"{}\" xml:lang=\"{}\">\n",
        escape_xml(&metadata.language), escape_xml(&metadata.language)
    ));
    out.push_str("<head>\n<title>Table of Contents</title>\n</head>\n<body>\n");
    out.push_str("<nav epub:type=\"toc\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n");
    out.push_str("<h1>Table of Contents</h1>\n");
    render_list(nav, 2, &mut out);
    out.push_str("</nav>\n</body>\n</html>\n");
    out
}

/// Render the EPUB2 NCX navigation document.
#[must_use]
pub fn render_toc_ncx(nav: &[NavPoint], metadata: &EpubMetadata) -> String {
    fn render_points(nav: &[NavPoint], indent: usize, order: &mut usize, out: &mut String) {
        let pad = " ".repeat(indent);
        for point in nav {
            *order += 1;
            out.push_str(&format!("{pad}<navPoint id=\"navPoint-{}\" playOrder=\"{}\">\n", order, order));
            out.push_str(&format!("{pad}  <navLabel><text>{}</text></navLabel>\n", escape_xml(&point.text)));
            out.push_str(&format!("{pad}  <content src=\"{}\" />\n", escape_xml(&point.href)));
            render_points(&point.children, indent + 2, order, out);
            out.push_str(&format!("{pad}</navPoint>\n"));
        }
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<ncx version=\"2005-1\" xmlns=\"http://www.daisy.org/z3986/2005/ncx/\">\n");
    out.push_str("<head>\n");
    out.push_str(&format!("<meta name=\"dtb:uid\" content=\"{}\"/>\n", escape_xml(&metadata.uid)));
    out.push_str("<meta name=\"dtb:depth\" content=\"6\"/>\n");
    out.push_str("<meta name=\"dtb:totalPageCount\" content=\"0\"/>\n");
    out.push_str("<meta name=\"dtb:maxPageNumber\" content=\"0\"/>\n");
    out.push_str("</head>\n");
    out.push_str(&format!("<docTitle><text>{}</text></docTitle>\n", escape_xml(&metadata.title)));
    out.push_str("<navMap>\n");
    let mut order = 0usize;
    render_points(nav, 2, &mut order, &mut out);
    out.push_str("</navMap>\n</ncx>\n");
    out
}

fn render_content_opf(
    metadata: &EpubMetadata,
    spine_order: &[String],
    manifest: &[(String, String, String)],
) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"{}\" xml:lang=\"{}\" unique-identifier=\"{}\" prefix=\"ibooks: http://vocabulary.itunes.apple.com/rdf/ibooks/vocabulary-extensions-1.0/\">\n",
        escape_xml(&metadata.epub_version), escape_xml(&metadata.language), escape_xml(&metadata.uid)
    ));
    out.push_str("  <metadata xmlns:opf=\"http://www.idpf.org/2007/opf\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n");
    out.push_str(&format!("    <dc:language>{}</dc:language>\n", escape_xml(&metadata.language)));
    out.push_str(&format!("    <dc:title>{}</dc:title>\n", escape_xml(&metadata.title)));
    out.push_str(&format!("    <dc:description>{}</dc:description>\n", escape_xml(&metadata.description)));
    out.push_str(&format!("    <dc:creator>{}</dc:creator>\n", escape_xml(&metadata.author)));
    out.push_str(&format!("    <dc:contributor>{}</dc:contributor>\n", escape_xml(&metadata.contributor)));
    out.push_str(&format!("    <dc:publisher>{}</dc:publisher>\n", escape_xml(&metadata.publisher)));
    out.push_str(&format!("    <dc:rights>{}</dc:rights>\n", escape_xml(&metadata.copyright)));
    out.push_str(&format!("    <dc:identifier id=\"{}\">{}</dc:identifier>\n", escape_xml(&metadata.uid), escape_xml(&metadata.identifier)));
    out.push_str(&format!("    <dc:date>{}</dc:date>\n", escape_xml(&metadata.date)));
    out.push_str(&format!("    <meta property=\"dcterms:modified\">{}</meta>\n", escape_xml(&metadata.date)));
    out.push_str(&format!("    <meta property=\"ibooks:version\">{}</meta>\n", escape_xml(&metadata.version)));
    out.push_str("  </metadata>\n");
    out.push_str("  <manifest>\n");
    out.push_str("    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\" />\n");
    out.push_str("    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\" />\n");
    for (id, href, media_type) in manifest {
        out.push_str(&format!(
            "    <item id=\"{}\" href=\"{}\" media-type=\"{}\" />\n",
            escape_xml(id), escape_xml(href), escape_xml(media_type)
        ));
    }
    out.push_str("  </manifest>\n");
    out.push_str("  <spine toc=\"ncx\" page-progression-direction=\"ltr\">\n");
    for docname in spine_order {
        out.push_str(&format!("    <itemref idref=\"epub-{}\" />\n", escape_xml(docname)));
    }
    out.push_str("  </spine>\n");
    out.push_str("  <guide>\n");
    out.push_str("    <reference type=\"toc\" title=\"Table of Contents\" href=\"nav.xhtml\" />\n");
    out.push_str("  </guide>\n");
    out.push_str("</package>\n");
    out
}

/// Write a deterministic EPUB ZIP archive from in-memory entries.
///
/// `mimetype` is always the first entry, STORED and with the exact byte
/// contents `application/epub+zip`. All entries share a fixed timestamp.
pub fn write_epub(path: &std::path::Path, entries: &BTreeMap<String, Vec<u8>>, timestamp: DateTime) -> OutputResult<()> {
    let file = std::fs::File::create(path).map_err(|e| OutputError::io(BUILDER_NAME, path, e))?;
    let mut writer = ZipWriter::new(file);
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(timestamp)
        .unix_permissions(0o644);
    let deflated = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(timestamp)
        .unix_permissions(0o644);

    writer.start_file("mimetype", stored).map_err(OutputError::Zip)?;
    writer.write_all(MIMETYPE).map_err(|e| OutputError::io(BUILDER_NAME, path, e))?;

    let mut names: Vec<&String> = entries.keys().filter(|name| name.as_str() != "mimetype").collect();
    names.sort();
    for name in names {
        let data = &entries[name];
        writer.start_file(name.as_str(), deflated).map_err(OutputError::Zip)?;
        writer.write_all(data).map_err(|e| OutputError::io(BUILDER_NAME, path, e))?;
    }
    writer.finish().map_err(OutputError::Zip)?;
    Ok(())
}

fn html_context(docname: &str, config: &Config, env: &scribe_core::BuildEnvironment, all_docs: &[String]) -> HtmlContext {
    let mut target_uris = BTreeMap::new();
    for name in all_docs {
        target_uris.insert(name.clone(), format!("{name}.xhtml"));
    }
    target_uris.entry(docname.to_owned()).or_insert_with(|| format!("{docname}.xhtml"));
    HtmlContext {
        docname: docname.to_owned(),
        config: Arc::new(config.clone()),
        env: Arc::new(env.clone()),
        theme_ctx: RenderContext::for_builder(BUILDER_NAME, docname, config.clone()),
        target_uris,
        all_docs: all_docs.iter().cloned().collect(),
        add_permalinks: false,
        permalinks_icon: String::new(),
        image_uris: BTreeMap::new(),
        language: config.get_str("language").unwrap_or("en").to_owned(),
        singlefile: None,
        graphviz: BTreeMap::new(),
    }
}

/// The EPUB (EPUB3) builder.
#[derive(Debug, Default)]
pub struct EpubBuilder;

impl EpubBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run a complete EPUB build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut scribe_core::BuildEnvironment,
    ) -> OutputResult<BuildResult> {
        let PreparedBuild { docnames, parsed, toctrees, warnings } =
            prepare(options, config, env, BUILDER_NAME)?;
        let metadata = EpubMetadata::from_config(config);

        // Titles for the navigation tree.
        let mut titles = BTreeMap::new();
        for docname in &docnames {
            let title = parsed
                .get(docname)
                .and_then(|doc| doc.metadata.title.clone().or_else(|| extract_title(doc)))
                .unwrap_or_else(|| docname.clone());
            titles.insert(docname.clone(), title);
        }

        // Spine order: toctree pre-order from the master document, then any
        // remaining documents in sorted order.
        let root = config.master_doc().to_owned();
        let mut order: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        fn collect_order(
            name: &str,
            hierarchy: &scribe_core::ToctreeHierarchy,
            seen: &mut std::collections::BTreeSet<String>,
            order: &mut Vec<String>,
        ) {
            if !seen.insert(name.to_owned()) {
                return;
            }
            order.push(name.to_owned());
            for child in hierarchy.children(name) {
                collect_order(child, hierarchy, seen, order);
            }
        }
        if toctrees.hierarchy.contains(&root) {
            collect_order(&root, &toctrees.hierarchy, &mut seen, &mut order);
        }
        for docname in &docnames {
            if seen.insert(docname.clone()) {
                order.push(docname.clone());
            }
        }

        // Render each XHTML document with the reused HTML translator.
        let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        entries.insert("mimetype".to_owned(), MIMETYPE.to_vec());
        entries.insert("META-INF/container.xml".to_owned(), CONTAINER_XML.as_bytes().to_vec());
        let mut manifest: Vec<(String, String, String)> = Vec::new();
        let mut spine_order: Vec<String> = Vec::new();
        for (index, docname) in order.iter().enumerate() {
            let Some(document) = parsed.get(docname) else { continue };
            let ctx = html_context(docname, config, env, &docnames);
            let body = HTML5Translator::translate(document, &ctx);
            let title = titles.get(docname).cloned().unwrap_or_else(|| docname.clone());
            let xhtml = wrap_xhtml(&body, &title, &metadata.language);
            let href = format!("{docname}.xhtml");
            entries.insert(format!("OEBPS/{href}"), xhtml.into_bytes());
            manifest.push((format!("epub-{index}"), href, "application/xhtml+xml".to_owned()));
            spine_order.push(index.to_string());
        }

        // Nav / NCX from the toctree.
        let nav = build_nav(&root, &toctrees.hierarchy, &titles, config.get_int("epub_tocdepth").unwrap_or(6).max(1) as usize);
        let nav_xhtml = render_nav_xhtml(&nav, &metadata);
        let toc_ncx = render_toc_ncx(&nav, &metadata);
        entries.insert("OEBPS/nav.xhtml".to_owned(), nav_xhtml.into_bytes());
        entries.insert("OEBPS/toc.ncx".to_owned(), toc_ncx.into_bytes());

        // Vendored CSS asset, also inlined into every document above.
        entries.insert("OEBPS/_static/epub.css".to_owned(), EPUB_CSS.as_bytes().to_vec());
        manifest.push(("epub-css".to_owned(), "_static/epub.css".to_owned(), "text/css".to_owned()));

        let opf = render_content_opf(&metadata, &spine_order, &manifest);
        entries.insert("OEBPS/content.opf".to_owned(), opf.into_bytes());

        let epub_path = options.output_dir.join(format!("{}.epub", metadata.basename));
        write_epub(&epub_path, &entries, metadata.timestamp)?;

        // Also materialize the staging files so the build output is inspectable
        // (and matches the oracle's loose-file layout).
        let mut written = vec![std::path::PathBuf::from(format!("{}.epub", metadata.basename))];
        let mut staging: Vec<(String, Vec<u8>)> = entries.into_iter().collect();
        staging.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, data) in staging {
            let path = options.output_dir.join(&name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| OutputError::io(BUILDER_NAME, parent, e))?;
            }
            std::fs::write(&path, data).map_err(|e| OutputError::io(BUILDER_NAME, &path, e))?;
            written.push(std::path::PathBuf::from(name));
        }

        Ok(BuildResult { written, built_docnames: docnames, warnings })
    }
}
