//! The Phase 06 resolve pass: `Pending` → concrete doctree (Phase 06).
//!
//! [`resolve_pending`] walks the whole arena repeatedly until no directive
//! `Pending` node remains. Each iteration resolves the nodes that were present
//! at its start, so directives created by nested block parses are handled in
//! the next iteration (this is what makes recursively nested directives work
//! without re-reading the original source).
//!
//! Unknown or invalid directives are reported as recoverable miette
//! diagnostics and the node is left in place: a partially resolved document is
//! still usable by later phases and by writers.

use std::collections::BTreeMap;
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode, SourceSpan};
use scribe_ast::{AstResult, Document, NodeId, NodeKind, Span};
use thiserror::Error;

use crate::directive_registry::{
    split_arguments, Context, DirectiveContext, DirectiveError, DirectiveRegistry,
};
use crate::option_spec::parse_options;
use crate::pending::{fill_pending, PendingDirective};

/// A non-fatal problem encountered while resolving one directive.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct ResolveIssue {
    /// Directive name.
    pub name: String,
    /// 1-based source line.
    pub line: u32,
    /// Byte span in the original source.
    pub span: SourceSpan,
    /// Human-readable failure.
    pub message: String,
    /// Stable diagnostic code.
    pub code: &'static str,
    /// When set, the exact (possibly multi-line) warning text to forward to
    /// the build log, bypassing the usual `{docname}: {message}` prefix. Used
    /// for docutils-compatible directive errors such as the `[docutils]`
    /// invalid-option block.
    pub raw_message: Option<String>,
    /// Source name used by miette.
    pub source_name: Option<String>,
    /// Source text used by miette (may be empty).
    pub source_text: Arc<String>,
}

impl Diagnostic for ResolveIssue {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }
    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Warning)
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some(format!("{} on line {}", self.name, self.line)),
            self.span,
        ))))
    }
    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.source_text)
    }
}

impl ResolveIssue {
    /// Named source wrapper used by [`Diagnostic::source_code`].
    pub fn named_source(&self) -> NamedSource<Arc<String>> {
        NamedSource::new(
            self.source_name.clone().unwrap_or_else(|| "<rst>".to_owned()),
            Arc::clone(&self.source_text),
        )
    }

    /// Warning text forwarded to the build log by builders and `check`.
    pub fn warning_message(&self, docname: &str) -> String {
        self.raw_message
            .clone()
            .unwrap_or_else(|| format!("{docname}: {}", self.message))
    }
}

/// Summary of a resolve pass.
#[derive(Clone, Debug, Default)]
pub struct ResolveReport {
    /// Number of directives successfully resolved.
    pub resolved: usize,
    /// Number of nested block parses performed.
    pub nested_parses: usize,
    /// Recoverable problems (unknown directive, bad options, ...).
    pub unresolved: Vec<ResolveIssue>,
}

impl ResolveReport {
    /// `true` when every directive resolved cleanly.
    pub fn is_clean(&self) -> bool {
        self.unresolved.is_empty()
    }

    /// Number of recoverable problems.
    pub fn issue_count(&self) -> usize {
        self.unresolved.len()
    }

    /// Returns the diagnostics as a vector of miette reports.
    pub fn diagnostics(&self) -> Vec<miette::Report> {
        self.unresolved
            .iter()
            .cloned()
            .map(miette::Report::new)
            .collect()
    }
}

/// Resolves every directive `Pending` in `document`, in place.
///
/// The `registry` decides which handlers exist; `context` supplies tags,
/// config, include resolution and the substitution-directive side table.
pub fn resolve_pending(
    document: &mut Document,
    registry: &DirectiveRegistry,
    context: &Context,
) -> ResolveReport {
    let mut report = ResolveReport::default();
    let mut failed = std::collections::HashSet::new();
    resolve_substitutions(document, registry, context, &mut report);
    loop {
        let pending = directive_pending_ids(document);
        if pending.is_empty() {
            break;
        }
        let mut progress = false;
        for id in pending {
            if failed.contains(&id) {
                continue;
            }
            if resolve_one(document, id, registry, context, &mut report) {
                progress = true;
            } else {
                failed.insert(id);
            }
        }
        if !progress {
            break;
        }
    }
    raise_only_nodes(document);
    report
}

/// Applies Sphinx's `Only.run` section raising: each `only` wrapper tagged
/// with `only_raise` is re-parented that many levels up so its nested sections
/// land at the level of the matching surrounding title style. Nodes are
/// processed in source order and inserted before the first sibling that
/// starts on a later line, preserving document order at the target level.
fn raise_only_nodes(document: &mut Document) {
    let mut pending: Vec<(usize, NodeId, i64)> = document
        .preorder_ids()
        .into_iter()
        .filter_map(|id| {
            let node = document.get(id).ok()?;
            if node.kind != NodeKind::Only {
                return None;
            }
            let raise = node
                .attrs
                .get_int(crate::pending::KEY_ONLY_RAISE)
                .ok()
                .flatten()?;
            if raise <= 0 {
                return None;
            }
            let line = node.source.as_ref().and_then(|pos| pos.line).unwrap_or(0) as usize;
            Some((line, id, raise))
        })
        .collect();
    pending.sort_by_key(|(line, _, _)| *line);
    for (line, id, raise) in pending {
        let mut target = match document.parent(id).ok().flatten() {
            Some(parent) => parent,
            None => continue,
        };
        for _ in 0..raise {
            match document.parent(target).ok().flatten() {
                Some(parent) => target = parent,
                None => break,
            }
        }
        let index = document.children(target).map_or(0, |children| {
            children
                .iter()
                .position(|&child| {
                    document
                        .get(child)
                        .ok()
                        .and_then(|node| node.source.as_ref().and_then(|pos| pos.line))
                        .is_some_and(|other| other as usize > line)
                })
                .unwrap_or(children.len())
        });
        let _ = document.insert_child(target, index, id);
        if let Ok(node) = document.get_mut(id) {
            node.attrs.remove(crate::pending::KEY_ONLY_RAISE);
        }
    }
}

/// Resolves a [`crate::ParseOutput`] and copies its additive side table into
/// the context first, so replacement/image/unicode substitution definitions
/// are interpreted correctly.
pub fn resolve_output(
    output: &mut crate::ParseOutput,
    registry: &DirectiveRegistry,
    context: &Context,
) -> ResolveReport {
    let mut local = context.clone();
    local.substitution_directives = output
        .substitution_directives()
        .iter()
        .map(|(id, info)| (id.0, info.clone()))
        .collect();
    let report = resolve_pending(&mut output.document, registry, &local);
    let live: Vec<bool> = output
        .inline_xrefs()
        .iter()
        .map(|(id, _)| output.document.is_live(*id))
        .collect();
    let xrefs = output.inline_xrefs_mut();
    let mut index = 0usize;
    xrefs.retain(|_| {
        let keep = live.get(index).copied().unwrap_or(false);
        index += 1;
        keep
    });
    process_only_nodes(&mut output.document, Some(local.tags.as_ref()));
    report
}

/// Docutils `sphinx.util.nodes.process_only_nodes` (mirrored by
/// `scribe_core::OnlyNodeTransform`).  Matching `only` wrappers are replaced by
/// their children; non-matching wrappers become empty `comment` nodes so a
/// preceding target cannot lose its ids (docutils' "Losing ids" guard).  The
/// `xml:space="preserve"` attribute mirrors the oracle's pseudo-XML exactly.
fn process_only_nodes(document: &mut Document, tags: Option<&scribe_config::Tags>) {
    let total = document
        .preorder_ids()
        .iter()
        .filter(|&&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Only))
        .count();
    // Re-walk after each replacement so nested `only` nodes are also handled.
    for _ in 0..total {
        let Some(id) = document.preorder_ids().into_iter().find(|&id| {
            document.get(id).is_ok_and(|node| node.kind == NodeKind::Only)
        }) else {
            break;
        };
        let expr = document
            .get(id)
            .ok()
            .and_then(|node| node.get_str("expr").ok().flatten().map(str::to_owned))
            .unwrap_or_default();
        // Upstream keeps the children and logs a warning on an invalid
        // expression; with no diagnostic channel here we still keep them.
        let keep = tags.is_none_or(|tags| tags.eval_condition(&expr).unwrap_or(true));
        if keep {
            let _ = document.replace_with_children(id);
            continue;
        }
        let mut comment = scribe_ast::Node::new(NodeKind::Comment);
        comment.attrs.set_str("xml:space", "preserve");
        if let Ok(node) = document.get(id) {
            for key in ["ids", "names", "dupnames", "classes", "backrefs"] {
                if let Some(value) = node.attrs.get(key) {
                    comment.attrs.insert(key.to_owned(), value.clone());
                }
            }
        }
        let _ = document.replace_node(id, comment);
    }
}

fn directive_pending_ids(document: &Document) -> Vec<NodeId> {
    document
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            document
                .get(id)
                .is_ok_and(|node| node.kind == NodeKind::Pending && PendingDirective::new(node).is_some())
        })
        .collect()
}

fn resolve_one(
    document: &mut Document,
    id: NodeId,
    registry: &DirectiveRegistry,
    context: &Context,
    report: &mut ResolveReport,
) -> bool {
    let (name, arguments, raw_options, content, body_lines, line, span, parent) = {
        let Ok(node) = document.get(id) else {
            return false;
        };
        let Some(view) = PendingDirective::new(node) else {
            return false;
        };
        (
            view.name().ok().flatten().unwrap_or("").to_owned(),
            view.arguments().ok().flatten().unwrap_or("").to_owned(),
            view.options(),
            view.content().ok().flatten().unwrap_or("").to_owned(),
            view.body_lines(),
            view.line().ok().flatten().unwrap_or(1).max(1) as u32,
            node.span.unwrap_or_default(),
            document.parent(id).ok().flatten(),
        )
    };
    let Some(parent) = parent else {
        return false;
    };

    // Sphinx resolves unprefixed directive names against the active default
    // domain *first*, then falls back to the standard/Python aliases. The
    // `default-domain` directive (resolved earlier in document order)
    // populates this state. Preferring the domain binding is essential when a
    // bare name such as `function` or `class` is also registered by the Python
    // domain's compatibility aliases.
    let domain_directive = if name.contains(':') {
        None
    } else {
        context
            .default_domain
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .and_then(|domain| registry.get(&format!("{domain}:{name}")))
    };
    let directive = domain_directive.or_else(|| registry.get(&name));
    let Some(directive) = directive else {
        push_issue(
            report,
            &name,
            line,
            span,
            format!("unknown directive '{name}'"),
            "scribe::rst::unknown_directive",
            context,
        );
        return false;
    };

    let miette_span = to_miette_span(span, line, context);
    let required = directive.required_arguments();
    let maximum = required + directive.optional_arguments();
    let mut arguments = arguments;
    let mut content = content;
    let mut split = split_arguments(&arguments, directive.final_argument_whitespace(), maximum);
    // Zero-argument directives (`toctree`, `contents`, `note`, ...) never take
    // an argument. Phase 04 promotes the first dedented body line when the
    // marker has no blank after it; restore the original option-free lines so
    // multi-line bodies such as toctree entries survive intact.
    if maximum == 0 && !arguments.trim().is_empty() && !body_lines.is_empty() {
        content.clone_from(&body_lines.join("\n"));
        arguments.clear();
        split.clear();
    }
    // `productionlist` takes the whole multi-line body as its single final
    // argument. When the marker carries no inline argument, Phase 04 promotes
    // the body lines and space-joins them (explicit.rs); restore the original
    // line breaks so each `name: tokens` rule is parsed separately.
    if name == "productionlist"
        && !body_lines.is_empty()
        && arguments == body_lines.join(" ")
    {
        arguments = body_lines.join("\n");
        split = split_arguments(&arguments, directive.final_argument_whitespace(), maximum);
    }
    // Docutils accepts a directive argument on the first content line even
    // when a blank line separates it from the marker. Phase 04 cannot know a
    // directive's arity, so apply that fallback here where the spec is known.
    if split.is_empty() && required > 0 && !content.trim().is_empty() {
        let first = content.lines().next().unwrap_or("").trim().to_owned();
        if !first.is_empty() {
            arguments.clone_from(&first);
            content = content
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
                .trim_start_matches('\n')
                .to_owned();
            split = split_arguments(&arguments, directive.final_argument_whitespace(), maximum);
        }
    }
    // Docutils folds the whole indented body into the final argument when a
    // directive declares `final_argument_whitespace`, `has_content = False`
    // and takes at least one argument (e.g. `productionlist`). Phase 04 splits
    // the marker-line argument from the body, so rejoin them here before the
    // "content not allowed" check. This prevents spurious failures and lets
    // the handler see every definition line.
    if !directive.has_content()
        && directive.final_argument_whitespace()
        && required > 0
        && !content.trim().is_empty()
    {
        let joined = if arguments.trim().is_empty() {
            content.trim_end_matches('\n').to_owned()
        } else {
            format!("{}\n{}", arguments.trim_end_matches('\n'), content.trim_end_matches('\n'))
        };
        arguments.clone_from(&joined);
        content.clear();
        split = split_arguments(&arguments, directive.final_argument_whitespace(), maximum);
    }
    let validate = if split.len() < required {
        Err(DirectiveError::MissingArgument {
            name: name.clone(),
            required,
            given: split.len(),
            span: miette_span,
        })
    } else if split.len() > maximum {
        Err(DirectiveError::TooManyArguments {
            name: name.clone(),
            maximum,
            given: split.len(),
            span: miette_span,
        })
    } else if !directive.has_content() && !content.trim().is_empty() {
        Err(DirectiveError::Failed {
            name: name.clone(),
            detail: "directive does not accept content".to_owned(),
            span: miette_span,
        })
    } else {
        parse_options(&raw_options, &directive.option_spec(), miette_span)
            .map_err(DirectiveError::from)
    };
    let options = match validate {
        Ok(options) => options,
        Err(error) => {
            let raw = if let crate::directive_registry::DirectiveError::Option(option_error) = &error {
                docutils_option_error(&name, line, option_error, &raw_options, context)
            } else {
                None
            };
            // Docutils replaces a directive whose options fail validation with
            // a `system_message` that the normal writer filters out. Remove
            // the placeholder so it does not leak a `pending` node into the
            // exported doctree.
            let drop_pending = raw.is_some();
            push_issue(
                report,
                &name,
                line,
                span,
                error.to_string(),
                error.code(),
                context,
            );
            if let Some(raw) = raw {
                if let Some(issue) = report.unresolved.last_mut() {
                    issue.raw_message = Some(raw);
                }
            }
            if drop_pending {
                let _ = document.remove_node(id);
            }
            return false;
        }
    };

    // Snapshot the parent's child list *before* the handler runs: handlers
    // attach their result to the same parent, so reading it afterwards would
    // include the produced nodes twice when we splice.
    let children_before = document.children(parent).map(|c| c.to_vec()).unwrap_or_default();
    let only_raise = document
        .get(id)
        .ok()
        .and_then(|node| node.attrs.get_int(crate::pending::KEY_ONLY_RAISE).ok().flatten())
        .unwrap_or(0);
    let mut ctx = DirectiveContext {
        doc: document,
        parent,
        raw_arguments: &arguments,
        arguments: split,
        raw_options: &raw_options,
        options,
        content: &content,
        line,
        span,
        only_raise,
        global: context,
    };
    let produced = match directive.run(&mut ctx) {
        Ok(produced) => produced,
        Err(error) => {
            push_issue(
                report,
                &name,
                line,
                span,
                error.to_string(),
                error.code(),
                context,
            );
            return false;
        }
    };

    // Splice the produced nodes into the pending node's position.
    let splice: AstResult<()> = (|| {
        let mut replacement = Vec::with_capacity(children_before.len() + produced.len());
        for child in children_before {
            if child == id {
                replacement.extend(produced.iter().copied());
            } else {
                replacement.push(child);
            }
        }
        document.replace_children(parent, replacement)?;
        document.remove_node(id)?;
        Ok(())
    })();
    if let Err(error) = splice {
        push_issue(
            report,
            &name,
            line,
            span,
            error.to_string(),
            "scribe::rst::ast",
            context,
        );
        return false;
    }
    report.resolved += 1;
    true
}

/// Resolves substitution definitions whose replacement text is a directive
/// (`image`, `unicode`, `date`, ...). Phase 04 records the directive name in a
/// side table; `replace` definitions are already inline-parsed and need no
/// work here.
fn resolve_substitutions(
    document: &mut Document,
    registry: &DirectiveRegistry,
    context: &Context,
    report: &mut ResolveReport,
) {
    let definitions: Vec<NodeId> = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::SubstitutionDefinition))
        .collect();
    let mut rewrites: Vec<(u32, String, String, Vec<scribe_ast::Node>)> = Vec::new();
    for id in definitions {
        let Some(info) = context.substitution_directives.get(&id.0).cloned() else {
            continue;
        };
        if info.name == "replace" {
            continue;
        }
        let (line, span, parent, previous_text) = {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let previous_text = node
                .children
                .first()
                .and_then(|&child| document.get(child).ok())
                .and_then(|child| child.attrs.get_str("text").ok().flatten().map(str::to_owned))
                .unwrap_or_default();
            (
                node.source.as_ref().and_then(|pos| pos.line).unwrap_or(1),
                node.span.unwrap_or_default(),
                document.parent(id).ok().flatten(),
                previous_text,
            )
        };
        let Some(parent) = parent else {
            continue;
        };

        // Run the replacement directive through the same registry path a
        // normal directive uses, then move the produced nodes into the
        // substitution definition.
        let mut payload = scribe_ast::Node::new(NodeKind::Pending);
        fill_pending(
            &mut payload,
            &info.name,
            &info.arguments,
            &info.options,
            "",
            line,
        );
        payload.span = Some(span);
        let children_before = document.children(parent).map(|c| c.to_vec()).unwrap_or_default();
        let pending_id = match document.add_node(payload, Some(parent)) {
            Ok(id) => id,
            Err(_) => continue,
        };
        if !resolve_one(document, pending_id, registry, context, report) {
            continue;
        }

        let children_after = document.children(parent).map(|c| c.to_vec()).unwrap_or_default();
        let produced: Vec<NodeId> = children_after
            .into_iter()
            .filter(|&child| child != pending_id && !children_before.contains(&child))
            .collect();
        if produced.is_empty() {
            continue;
        }
        let new_nodes: Vec<scribe_ast::Node> = produced
            .iter()
            .filter_map(|&child| document.get(child).ok().cloned())
            .collect();
        let new_text = new_nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Text)
            .map(|node| node.attrs.get_str("text").ok().flatten().unwrap_or(""))
            .collect::<String>();
        let current = document.get(id).map(|node| node.children.clone()).unwrap_or_default();
        for child in current {
            let _ = document.remove_node(child);
        }
        for child in produced {
            if document.parent(child).ok().flatten() == Some(id) {
                continue;
            }
            let _ = document.append_child(id, child);
        }
        rewrites.push((id.0, info.name, previous_text, new_nodes));
        let _ = new_text;
    }

    // Phase 05 already expanded `|name|` references using the raw replacement
    // text. Re-apply those expansions now that definitions are concrete.
    for (_, _, previous_text, new_nodes) in rewrites {
        if previous_text.is_empty() || new_nodes.is_empty() {
            continue;
        }
        let text_ids: Vec<NodeId> = document
            .preorder_ids()
            .into_iter()
            .filter(|&id| {
                document.get(id).is_ok_and(|node| {
                    node.kind == NodeKind::Text
                        && node.attrs.get_str("text").ok().flatten() == Some(previous_text.as_str())
                        && !document.ancestors(id).any(|ancestor| {
                            document.get(ancestor).is_ok_and(|n| n.kind == NodeKind::SubstitutionDefinition)
                        })
                })
            })
            .collect();
        for id in text_ids {
            let parent = document.parent(id).ok().flatten();
            let Some(parent) = parent else {
                continue;
            };
            if new_nodes.len() == 1 && new_nodes[0].kind == NodeKind::Text {
                if let Ok(node) = document.get_mut(id) {
                    node.attrs.set_str(
                        "text",
                        new_nodes[0].attrs.get_str("text").ok().flatten().unwrap_or(""),
                    );
                }
                continue;
            }
            let index = document
                .children(parent)
                .map(|children| children.iter().position(|&child| child == id))
                .ok()
                .flatten();
            let Some(index) = index else {
                continue;
            };
            let mut replacements = Vec::new();
            for node in &new_nodes {
                let mut clone = node.clone();
                clone.children.clear();
                if let Ok(new_id) = document.add_node(clone, None) {
                    replacements.push(new_id);
                }
            }
            let _ = document.splice_children(parent, index..index + 1, replacements);
            let _ = document.remove_node(id);
        }
    }
}

fn push_issue(
    report: &mut ResolveReport,
    name: &str,
    line: u32,
    span: Span,
    message: String,
    code: &'static str,
    context: &Context,
) {
    let source = context
        .source_text
        .as_ref()
        .map(Arc::clone)
        .unwrap_or_else(|| Arc::new(String::new()));
    let byte_span = to_miette_span(span, line, context);
    report.unresolved.push(ResolveIssue {
        name: name.to_owned(),
        line,
        span: byte_span,
        message,
        code,
        raw_message: None,
        source_name: context.source_name.clone(),
        source_text: source,
    });
}

/// Reconstruct docutils' multi-line `Error in "<directive>" directive:`
/// invalid-option block, including the quoted source lines and `[docutils]`
/// suffix emitted by `sphinx-build` with a pseudoxml writer.
fn docutils_option_error(
    name: &str,
    line: u32,
    error: &crate::option_spec::OptionError,
    raw_options: &BTreeMap<String, String>,
    context: &Context,
) -> Option<String> {
    use crate::option_spec::OptionError;
    let OptionError::InvalidValue { name: option_name, expected, .. } = error else {
        return None;
    };
    let raw_value = raw_options.get(option_name)?.clone();
    let candidates: Vec<String> = expected
        .strip_prefix("one of ")?
        .split(", ")
        .map(|candidate| format!("\"{candidate}\""))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let choices = if candidates.len() == 1 {
        candidates[0].clone()
    } else {
        let tail = candidates.last().map_or("", String::as_str);
        format!("{}, or {}", candidates[..candidates.len() - 1].join(", "), tail)
    };
    let source_name = context.source_name.as_deref().unwrap_or("index");
    let candidate = context.source_dir.as_ref()?.join(format!("{source_name}.rst"));
    // Sphinx reports canonical absolute paths; canonicalise so the harness's
    // non-canonical workspace path (with `..` components) does not diverge.
    let path = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    let source = context.source_text.as_ref()?;
    let lines: Vec<&str> = source.lines().collect();
    let start = (line as usize).saturating_sub(1);
    if start >= lines.len() {
        return None;
    }
    let mut block = vec![lines[start].to_owned()];
    for text in lines.iter().skip(start + 1) {
        if text.trim().is_empty() || !text.starts_with(char::is_whitespace) {
            break;
        }
        block.push((*text).to_owned());
    }
    let last = block.len() - 1;
    block[last].push_str(" [docutils]");
    Some(format!(
        "{}:{}: ERROR: Error in \"{}\" directive:\n\
         invalid option value: (option: \"{}\"; value: '{}')\n\
         \"{}\" unknown; choose from {}.\n\n{}",
        path.display(),
        line,
        name,
        option_name,
        raw_value,
        raw_value,
        choices,
        block.join("\n"),
    ))
}

fn to_miette_span(span: Span, line: u32, context: &Context) -> SourceSpan {
    if !span.is_empty() {
        return SourceSpan::new((span.start as usize).into(), span.len() as usize);
    }
    let start = context
        .source_text
        .as_deref()
        .map(|source| {
            source
                .lines()
                .take(line.saturating_sub(1) as usize)
                .map(|text| text.len() + 1)
                .sum::<usize>()
        })
        .unwrap_or(0);
    SourceSpan::new(start.into(), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_defaults_are_clean() {
        let report = ResolveReport::default();
        assert!(report.is_clean());
        assert_eq!(report.issue_count(), 0);
    }

    #[test]
    fn line_span_falls_back_to_source_lines() {
        let context = Context {
            source_text: Some(Arc::new("a\nb\nc\n".to_owned())),
            ..Context::default()
        };
        assert_eq!(to_miette_span(Span::new(0, 0), 3, &context).offset(), 4);
    }
}
