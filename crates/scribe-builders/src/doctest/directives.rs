//! The five doctest directives (Phase 22).
//!
//! Upstream `TestDirective.run()` emits ordinary docutils `literal_block` /
//! `comment` nodes carrying `testnodetype`, `groups`, `options` and friends so
//! other builders ignore them. The frozen §7 AST has both node kinds, so the
//! same representation is used here; only the `options` map is encoded as a
//! JSON string (no tuple/bitmap attribute type exists).

use scribe_ast::{Node, NodeId, NodeKind};
use scribe_rst::directive_registry::{Directive, DirectiveContext, DirectiveError, DirectiveRegistry};
use scribe_rst::option_spec::{OptionKind, OptionSpec};

use super::normalize::parse_option_string;

/// Directive kinds implemented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoctestDirectiveKind {
    /// `.. testsetup::`
    TestSetup,
    /// `.. testcleanup::`
    TestCleanup,
    /// `.. doctest::`
    Doctest,
    /// `.. testcode::`
    TestCode,
    /// `.. testoutput::`
    TestOutput,
}

impl DoctestDirectiveKind {
    /// Upstream `testnodetype` value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TestSetup => "testsetup",
            Self::TestCleanup => "testcleanup",
            Self::Doctest => "doctest",
            Self::TestCode => "testcode",
            Self::TestOutput => "testoutput",
        }
    }

    const fn is_hidden(self) -> bool {
        matches!(self, Self::TestSetup | Self::TestCleanup)
    }
}

/// Handler for one of the five doctest directives.
#[derive(Clone, Copy, Debug)]
pub struct TestDirective {
    /// Which directive this handler implements.
    pub kind: DoctestDirectiveKind,
}

impl Directive for TestDirective {
    fn name(&self) -> &'static str {
        self.kind.as_str()
    }

    fn optional_arguments(&self) -> usize {
        1
    }

    fn final_argument_whitespace(&self) -> bool {
        true
    }

    fn option_spec(&self) -> OptionSpec {
        let mut spec = OptionSpec::empty()
            .with("hide", OptionKind::Flag)
            .with("no-trim-doctest-flags", OptionKind::Flag)
            .with("pyversion", OptionKind::UnchangedRequired)
            .with("skipif", OptionKind::UnchangedRequired)
            .with("trim-doctest-flags", OptionKind::Flag);
        if matches!(self.kind, DoctestDirectiveKind::Doctest | DoctestDirectiveKind::TestOutput) {
            spec = spec.with("options", OptionKind::Unchanged);
        }
        spec
    }

    fn has_content(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let mut code = ctx.content.trim_end_matches('\n').to_owned();
        let mut presentation: Option<String> = None;
        if self.kind == DoctestDirectiveKind::Doctest {
            let stripped_blanklines = strip_matching_lines(&code, "<BLANKLINE>");
            if stripped_blanklines != code {
                presentation = Some(code.clone());
                code = stripped_blanklines;
            }
            if !ctx.flag("no-trim-doctest-flags") {
                let stripped_flags = strip_doctest_flags(&code);
                if stripped_flags != code {
                    presentation.get_or_insert_with(|| code.clone());
                    code = stripped_flags;
                }
            }
        }

        let groups: Vec<String> = match ctx.argument(0) {
            Some(argument) if !argument.trim().is_empty() => argument
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
            _ => vec!["default".to_owned()],
        };
        let (options, warnings) = ctx
            .raw_options
            .get("options")
            .map_or_else(|| (std::collections::BTreeMap::new(), Vec::new()), |raw| parse_option_string(raw));
        for warning in warnings {
            let _ = ctx.parse_block_text(&format!(".. warning:: {warning}"), ctx.parent, ctx.line);
        }

        let hidden = self.kind.is_hidden() || ctx.flag("hide");
        let node_kind = if hidden { NodeKind::Comment } else { NodeKind::LiteralBlock };
        let mut node = Node::new(node_kind);
        node.attrs.set_str("testnodetype", self.kind.as_str());
        node.attrs.set_list("groups", groups);
        node.attrs.set_str(
            "options",
            serde_json::to_string(&options).unwrap_or_else(|_| "{}".to_owned()),
        );
        let language = match self.kind {
            DoctestDirectiveKind::Doctest => "pycon",
            DoctestDirectiveKind::TestCode => "python",
            DoctestDirectiveKind::TestOutput => "none",
            DoctestDirectiveKind::TestSetup | DoctestDirectiveKind::TestCleanup => "python",
        };
        node.attrs.set_str("language", language);
        if let Some(presentation) = presentation {
            node.attrs.set_str("test", presentation);
        }
        if let Some(skipif) = ctx.raw_options.get("skipif") {
            node.attrs.set_str("skipif", skipif.as_str());
        }
        if let Some(pyversion) = ctx.raw_options.get("pyversion") {
            node.attrs.set_str("pyversion", pyversion.as_str());
        }
        if ctx.flag("trim-doctest-flags") {
            node.attrs.set_str("trim_flags", "1");
        } else if ctx.flag("no-trim-doctest-flags") {
            node.attrs.set_str("trim_flags", "0");
        }
        node.source = Some(scribe_ast::SourcePos::new(
            ctx.global.source_name.clone(),
            Some(ctx.line),
            Some(ctx.span.start),
        ));
        node.span = Some(ctx.span);
        let id = ctx.doc.add_node(node, Some(ctx.parent))?;
        // Upstream stores the *presentation* source (with `<BLANKLINE>` and
        // `# doctest:` markers) in the `test` attribute for the doctest builder,
        // while the node's own text is the stripped, highlightable source.
        ctx.doc.add_node(Node::text(code), Some(id))?;
        Ok(vec![id])
    }
}

fn strip_matching_lines(code: &str, token: &str) -> String {
    code.lines()
        .filter(|line| line.trim() != token)
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_doctest_flags(code: &str) -> String {
    code.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#') || !trimmed[1..].trim_start().starts_with("doctest:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Registers all five directives into `registry`.
pub fn register_directives(registry: &DirectiveRegistry) {
    let kinds = [
        DoctestDirectiveKind::TestSetup,
        DoctestDirectiveKind::TestCleanup,
        DoctestDirectiveKind::Doctest,
        DoctestDirectiveKind::TestCode,
        DoctestDirectiveKind::TestOutput,
    ];
    for kind in kinds {
        registry.register(TestDirective { kind });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blankline_and_flag_stripping() {
        assert_eq!(strip_matching_lines(">>> print(1)\n<BLANKLINE>\nx", "<BLANKLINE>"), ">>> print(1)\nx");
        assert_eq!(
            strip_doctest_flags(">>> 1 + 1\n2\n# doctest: +SKIP\n>>> x"),
            ">>> 1 + 1\n2\n>>> x"
        );
    }
}
