//! `sphinx.ext.graphviz`: directives, safe `dot` rendering and HTML metadata.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use miette::SourceSpan;
use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::Config;
use scribe_rst::{Directive, DirectiveContext, DirectiveError, OptionKind, OptionSpec};

use super::EXT_GRAPHVIZ;

/// Everything the HTML writer needs to render a graphviz node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphvizHtmlInfo {
    /// `_images/graphviz-<hash>.<fmt>` (or `None` when `dot` is unavailable).
    pub image_uri: Option<String>,
    /// `png` or `svg`.
    pub format: String,
    /// Alternative text.
    pub alt: String,
    /// Alignment, when the `:align:` option was used.
    pub align: Option<String>,
    /// CSS classes for the image/object.
    pub classes: Vec<String>,
    /// Clickable cmap id, when the `.map` file contains links.
    pub usemap: Option<String>,
    /// The `<map>...</map>` markup extracted from the cmap file.
    pub usemap_html: Option<String>,
    /// Raw dot source, used as a fallback when `dot` is missing.
    pub code: String,
}

/// Rendering state shared by a build (avoids repeated "dot missing" warnings).
#[derive(Debug)]
pub struct GraphvizRender {
    dot: String,
    dot_args: Vec<String>,
    format: String,
    source_dir: PathBuf,
    outdir: PathBuf,
    warned_dot: bool,
}

impl GraphvizRender {
    /// Create a renderer from configuration.
    #[must_use]
    pub fn new(config: &Config, source_dir: &Path, outdir: &Path) -> Self {
        let format = config
            .get_str("graphviz_output_format")
            .unwrap_or("png")
            .to_ascii_lowercase();
        Self {
            dot: config.get_str("graphviz_dot").unwrap_or("dot").to_owned(),
            dot_args: config.get_strings("graphviz_dot_args").unwrap_or_default(),
            format: if matches!(format.as_str(), "png" | "svg") { format } else { "png".to_owned() },
            source_dir: source_dir.to_path_buf(),
            outdir: outdir.to_path_buf(),
            warned_dot: false,
        }
    }

    /// Render every graphviz node in `doc` and return writer metadata keyed by
    /// node id. Failures are downgraded to warnings plus a fallback node.
    #[must_use]
    pub fn render_document(
        &mut self,
        docname: &str,
        doc: &mut Document,
    ) -> (BTreeMap<u32, GraphvizHtmlInfo>, Vec<String>) {
        let nodes: Vec<NodeId> = doc
            .preorder_ids()
            .into_iter()
            .filter(|&id| {
                doc.get(id).is_ok_and(|node| {
                    node.attrs.get_str(super::EXT_MARKER).ok().flatten() == Some(EXT_GRAPHVIZ)
                })
            })
            .collect();
        let mut infos = BTreeMap::new();
        let mut warnings = Vec::new();
        for id in nodes {
            let (code, alt, align, classes, layout, format_override, filename) = {
                let Ok(node) = doc.get(id) else { continue };
                (
                    node.attrs.get_str("gv_code").ok().flatten().unwrap_or("").to_owned(),
                    node.attrs.get_str("gv_alt").ok().flatten().unwrap_or("").to_owned(),
                    node.attrs.get_str("gv_align").ok().flatten().map(str::to_owned),
                    node.classes().unwrap_or_default(),
                    node.attrs.get_str("gv_dot").ok().flatten().map(str::to_owned),
                    node.attrs.get_str("gv_format").ok().flatten().map(str::to_owned),
                    node.attrs.get_str("gv_filename").ok().flatten().map(str::to_owned),
                )
            };
            if code.trim().is_empty() {
                warnings.push(format!("{docname}: ignoring graphviz directive without content"));
                continue;
            }
            let code = if let Some(filename) = filename {
                // Sphinx treats a leading '/' as source-root-relative.
                let relative = filename.trim_start_matches('/');
                let path = self.source_dir.join(relative);
                match fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(_) => {
                        warnings.push(format!(
                            "{docname}: external Graphviz file '{filename}' not found or reading it failed"
                        ));
                        continue;
                    }
                }
            } else {
                code
            };
            let format = format_override.unwrap_or_else(|| self.format.clone());
            let dot = layout.clone().unwrap_or_else(|| self.dot.clone());
            let output = self.render(&code, &dot, &format);
            match output {
                RenderOutcome::Rendered { relative, map } => {
                    let (usemap, usemap_html) = map
                        .as_deref()
                        .and_then(parse_clickable_map)
                        .map_or((None, None), |(id, html)| (Some(id), Some(html)));
                    infos.insert(
                        id.index(),
                        GraphvizHtmlInfo {
                            image_uri: Some(relative),
                            format,
                            alt,
                            align,
                            classes,
                            usemap,
                            usemap_html,
                            code,
                        },
                    );
                }
                RenderOutcome::Unavailable { message } => {
                    warnings.push(format!("{docname}: {message}"));
                    infos.insert(
                        id.index(),
                        GraphvizHtmlInfo {
                            image_uri: None,
                            format,
                            alt,
                            align,
                            classes,
                            usemap: None,
                            usemap_html: None,
                            code,
                        },
                    );
                }
            }
        }
        (infos, warnings)
    }

    fn render(&mut self, code: &str, dot: &str, format: &str) -> RenderOutcome {
        if self.warned_dot {
            return RenderOutcome::Unavailable {
                message: format!("dot command '{dot}' cannot be run (needed for graphviz output)"),
            };
        }
        let mut hash_input = code.to_owned();
        hash_input.push_str(dot);
        for arg in &self.dot_args {
            hash_input.push_str(arg);
        }
        hash_input.push_str(format);
        let digest = blake3::hash(hash_input.as_bytes()).to_hex().to_string();
        let filename = format!("graphviz-{}.{format}", &digest[..12]);
        let relative = format!("_images/{filename}");
        let outfn = self.outdir.join("_images").join(&filename);
        if let Some(parent) = outfn.parent() {
            if fs::create_dir_all(parent).is_err() {
                return RenderOutcome::Unavailable { message: "cannot create _images directory".to_owned() };
            }
        }
        if outfn.is_file() {
            let map = if format == "png" {
                fs::read_to_string(format!("{}.map", outfn.display())).ok()
            } else {
                None
            };
            return RenderOutcome::Rendered { relative, map };
        }

        let mut command = Command::new(dot);
        command.args(&self.dot_args);
        command.arg(format!("-T{format}"));
        command.arg(format!("-o{}", outfn.display()));
        if format == "png" {
            command.arg("-Tcmapx");
            command.arg(format!("-o{}.map", outfn.display()));
        }
        command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.warned_dot = true;
                return RenderOutcome::Unavailable {
                    message: format!(
                        "dot command '{dot}' cannot be run (needed for graphviz output), check the graphviz_dot setting: {error}"
                    ),
                };
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(code.as_bytes()).is_err() {
                let _ = child.kill();
                self.warned_dot = true;
                return RenderOutcome::Unavailable { message: "cannot pass graphviz source to dot".to_owned() };
            }
        }
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                self.warned_dot = true;
                return RenderOutcome::Unavailable { message: format!("dot invocation failed: {error}") };
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return RenderOutcome::Unavailable {
                message: format!("dot exited with error: {stderr}"),
            };
        }
        if !outfn.is_file() {
            return RenderOutcome::Unavailable { message: "dot did not produce an output file".to_owned() };
        }
        let map = if format == "png" {
            fs::read_to_string(format!("{}.map", outfn.display())).ok()
        } else {
            None
        };
        RenderOutcome::Rendered { relative, map }
    }
}

enum RenderOutcome {
    Rendered { relative: String, map: Option<String> },
    Unavailable { message: String },
}

/// Extract the clickable `<map>` lines from a cmapx file.
fn parse_clickable_map(content: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = content.lines().collect();
    let first = lines.first()?;
    let open = first.find("<map id=\"")?;
    let rest = &first[open + 9..];
    let end = rest.find('"')?;
    let id = rest[..end].to_owned();
    let clickable: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.contains("href=\""))
        .collect();
    if clickable.is_empty() {
        return None;
    }
    let mut html = String::new();
    html.push_str(first);
    html.push('\n');
    for line in &clickable {
        html.push_str(line);
        html.push('\n');
    }
    if let Some(last) = lines.last() {
        if !clickable.last().is_some_and(|line| **line == **last) {
            html.push_str(last);
            html.push('\n');
        }
    }
    Some((id, html))
}

// ----- directives ----------------------------------------------------------

fn graphviz_options() -> OptionSpec {
    OptionSpec::empty()
        .with("alt", OptionKind::Unchanged)
        .with("align", OptionKind::Choice(vec!["left".to_owned(), "center".to_owned(), "right".to_owned()]))
        .with("caption", OptionKind::Unchanged)
        .with("layout", OptionKind::Unchanged)
        .with("graphviz_dot", OptionKind::Unchanged)
        .with("output-format", OptionKind::Choice(vec!["png".to_owned(), "svg".to_owned()]))
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
}

#[derive(Clone, Copy, Debug)]
pub struct GraphvizDirective;

impl Directive for GraphvizDirective {
    fn name(&self) -> &'static str {
        "graphviz"
    }

    fn optional_arguments(&self) -> usize {
        1
    }

    fn option_spec(&self) -> OptionSpec {
        graphviz_options()
    }

    fn has_content(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let code = if let Some(argument) = ctx.argument(0) {
            if !ctx.content.trim().is_empty() {
                return Err(DirectiveError::Failed {
                    name: "graphviz".to_owned(),
                    detail: "Graphviz directive cannot have both content and a filename argument".to_owned(),
                    span: SourceSpan::new((ctx.span.start as usize).into(), (ctx.span.end.saturating_sub(ctx.span.start)) as usize),
                });
            }
            // A leading '/' is source-root-relative in Sphinx, not filesystem
            // absolute (e.g. `.. graphviz:: /_static/diagrams/flow.dot`).
            let relative = argument.trim_start_matches('/');
            let path = ctx.global.source_dir.as_ref().map_or_else(|| PathBuf::from(relative), |dir| dir.join(relative));
            match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => {
                    return Err(DirectiveError::Failed {
                        name: "graphviz".to_owned(),
                        detail: format!("External Graphviz file '{argument}' not found or reading it failed"),
                        span: SourceSpan::new((ctx.span.start as usize).into(), (ctx.span.end.saturating_sub(ctx.span.start)) as usize),
                    });
                }
            }
        } else {
            ctx.content.to_owned()
        };
        create_graphviz_node(ctx, code, ctx.argument(0).map(str::to_owned))
    }
}

macro_rules! simple_directive {
    ($name:ident, $keyword:literal) => {
        #[derive(Clone, Copy, Debug)]
        pub struct $name;

        impl Directive for $name {
            fn name(&self) -> &'static str {
                $keyword
            }

            fn required_arguments(&self) -> usize {
                1
            }

            fn option_spec(&self) -> OptionSpec {
                graphviz_options()
            }

            fn has_content(&self) -> bool {
                true
            }

            fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
                let argument = ctx.argument(0).unwrap_or("").to_owned();
                let code = format!("{} {} {{\n{}\n}}\n", $keyword, argument, ctx.content);
                create_graphviz_node(ctx, code, None)
            }
        }
    };
}

simple_directive!(GraphDirective, "graph");
simple_directive!(DigraphDirective, "digraph");

fn create_graphviz_node(
    ctx: &mut DirectiveContext<'_>,
    code: String,
    filename: Option<String>,
) -> Result<Vec<NodeId>, DirectiveError> {
    let node = ctx.add(NodeKind::Generated, ctx.parent)?;
    let alt = ctx.option_str("alt").map(str::to_owned);
    let align = ctx.option_str("align").map(str::to_owned);
    let layout = ctx.option_str("layout").or_else(|| ctx.option_str("graphviz_dot")).map(str::to_owned);
    let format = ctx.option_str("output-format").map(str::to_owned);
    let caption = ctx.option_str("caption").map(str::to_owned);
    let mut classes = ctx.classes();
    classes.push("graphviz".to_owned());
    {
        let target = ctx.doc.get_mut(node)?;
        target.attrs.set_str(super::EXT_MARKER, EXT_GRAPHVIZ);
        target.attrs.set_str("gv_code", code);
        if let Some(alt) = alt { target.attrs.set_str("gv_alt", alt); }
        // With a caption Sphinx's `figure_wrapper` moves `align` onto the
        // figure, so the graphviz node itself must not emit an alignment div.
        if caption.is_none() {
            if let Some(align) = &align { target.attrs.set_str("gv_align", align); }
        }
        if let Some(layout) = layout { target.attrs.set_str("gv_dot", layout); }
        if let Some(format) = format { target.attrs.set_str("gv_format", format); }
        if let Some(name) = filename.filter(|value| !value.is_empty()) {
            target.attrs.set_str("gv_filename", name);
        }
        if let Some(caption) = &caption { target.attrs.set_str("gv_caption", caption); }
        target.attrs.set_list("classes", classes);
    }
    ctx.set_common(node)?;
    if let Some(caption) = caption {
        let figure = ctx.add(NodeKind::Figure, ctx.parent)?;
        {
            let figure_node = ctx.doc.get_mut(figure)?;
            let align_class = align.as_ref().map(|value| format!("align-{value}"));
            figure_node.attrs.set_list(
                "classes",
                vec![align_class.unwrap_or_else(|| "align-default".to_owned())],
            );
        }
        let _ = ctx.doc.append_child(figure, node);
        // Sphinx parses the caption's inline markup (`parse_inline`) and places
        // the resulting nodes directly inside the `caption` element. The HTML
        // writer then supplies the single `<p><span class="caption-text">`
        // wrapper, so adding another paragraph/inline layer here would emit
        // duplicate wrappers (and lose emphasis such as `:caption: on *right*`).
        let caption_node = ctx.add(NodeKind::Caption, figure)?;
        ctx.parse_inline_into(&caption, caption_node, ctx.line)?;
        Ok(vec![figure])
    } else {
        Ok(vec![node])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_are_registered() {
        let registry = scribe_rst::DirectiveRegistry::new();
        registry.register(GraphvizDirective);
        registry.register(GraphDirective);
        registry.register(DigraphDirective);
        for name in ["graphviz", "graph", "digraph"] {
            assert!(registry.contains(name));
        }
    }

    #[test]
    fn clickable_map_extraction_is_defensive() {
        assert!(parse_clickable_map("").is_none());
        let map = "<map id=\"test\">\n<area href=\"#x\">\n</map>";
        let (id, html) = parse_clickable_map(map).unwrap();
        assert_eq!(id, "test");
        assert!(html.contains("href=\"#x\""));
    }
}
