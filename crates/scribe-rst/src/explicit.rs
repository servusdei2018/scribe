//! Explicit markup constructs (`.. `): directives, comments, targets and
//! substitution definitions.
//!
//! Directives are deliberately *not* interpreted in Phase 04. They become
//! `NodeKind::Pending` nodes carrying the structured attributes documented in
//! [`crate::pending`] for Phase 06 to resolve.

use std::collections::BTreeMap;

use scribe_ast::NodeKind;

use crate::error::RstError;
use crate::pending::fill_pending;
use crate::names::{make_id, normalize_name, whitespace_normalize_name};
use crate::parser::BlockParser;
use crate::RstResult;

/// Dispatches one explicit-markup construct at the cursor.
impl<'a> BlockParser<'a> {
    pub(crate) fn parse_explicit(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let index = self.pos;
        let content = self.content_at(index, base_indent).to_owned();
        let rest = content.strip_prefix(".. ").unwrap_or("").trim_start();

        // 1. Hyperlink target: `.. _name: uri` or `.. __: uri`.
        if rest.starts_with('_') && rest.len() > 1 && !rest.starts_with("_ ") {
            return self.parse_target(parent, base_indent, index, rest);
        }
        // 2. Substitution definition: `.. |name| replace:: text`.
        if rest.starts_with('|') {
            return self.parse_substitution(parent, base_indent, index, rest);
        }
        // 3. Footnote / citation: `.. [label] body`.
        if rest.starts_with('[') {
            return self.parse_bracket_construct(parent, base_indent, index, rest);
        }
        // 4. Directive: `.. name:: args`.
        if let Some((name, inline_args)) = split_directive(rest) {
            return self.parse_directive(parent, base_indent, index, name, inline_args);
        }
        // 5. Comment (including an empty `..` line).
        self.parse_comment(parent, base_indent, index, rest)
    }

    /// Consumes the explicit-markup body and returns its dedented lines.
    fn take_body(&mut self, index: usize, base_indent: usize) -> Vec<String> {
        let next = index + 1;
        if self.line(next).is_none_or(|line| line.blank) {
            self.pos = next;
            return Vec::new();
        }
        let (start, end, indent) = self.collect_indented_range(next, base_indent);
        let lines: Vec<String> = (start..end)
            .map(|i| self.content_at(i, indent).to_owned())
            .collect();
        self.pos = end;
        lines
    }

    /// Consumes a directive body, allowing blank lines between the marker and
    /// the first indented body line (Docutils permits this) while leaving
    /// truly empty directives alone.
    fn take_directive_body(&mut self, index: usize, base_indent: usize) -> (Vec<String>, bool) {
        let mut next = index + 1;
        let mut had_blank_prefix = false;
        while self.line(next).is_some_and(|line| line.blank) {
            had_blank_prefix = true;
            next += 1;
        }
        if self.line(next).is_none_or(|_| self.indent_at(next) <= base_indent) {
            self.pos = next.min(self.lines.len());
            return (Vec::new(), had_blank_prefix);
        }
        let (start, end, indent) = self.collect_indented_range(next, base_indent);
        let lines: Vec<String> = (start..end)
            .map(|i| self.content_at(i, indent).to_owned())
            .collect();
        self.pos = end;
        (lines, had_blank_prefix)
    }

    /// `.. name:: args` → `Pending` with the Phase 06 attribute contract.
    fn parse_directive(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
        index: usize,
        name: &str,
        inline_args: &str,
    ) -> RstResult<()> {
        let (mut lines, had_blank_prefix) = self.take_directive_body(index, base_indent);
        trim_blank_edges(&mut lines);

        // Strip option lines first so argument promotion only sees real body
        // content. Preserve a copy of those lines for zero-argument directives.
        let mut options = BTreeMap::new();
        // Docutils only recognises an option block when it immediately follows
        // the marker. After a blank line a leading `:field:` list is content
        // (e.g. the typed fields under a `.. py:function::`).
        let mut options_separated = false;
        if !had_blank_prefix {
            while let Some(first) = lines.first() {
                let trimmed = first.trim_end().to_owned();
                let Some((opt_name, marker_len)) = crate::lists::field_marker(&trimmed) else {
                    break;
                };
                let value = trimmed.get(marker_len..).unwrap_or("").trim().to_owned();
                options.insert(normalize_name(&opt_name), value);
                lines.remove(0);
            }
            // Phase 15 remediation: a blank line *after* the option block means
            // the remaining lines are directive content, not a promoted
            // argument. Without this, `.. code-block::` with `:dedent:`/`:class:`
            // and no language argument silently moves the first code lines into
            // the argument slot and corrupts the highlighted source.
            options_separated =
                !options.is_empty() && lines.first().is_some_and(|line| line.trim().is_empty());
        }
        trim_blank_edges(&mut lines);
        let body_lines = lines.clone();

        let mut arguments = inline_args.trim().to_owned();
        if arguments.is_empty() && !had_blank_prefix && !options_separated && !lines.is_empty() {
            let mut i = 0usize;
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            arguments = lines[..i].join(" ").trim().to_owned();
            lines.drain(..i);
            trim_blank_edges(&mut lines);
        }
        let content = lines.join("\n");

        // `.. default-role::` and `.. role::` change Docutils' inliner state
        // *while it scans*, so they affect inline content that follows them in
        // source order. The block parser walks blocks in document order, so
        // updating its inline state here makes subsequent `add_inline` calls
        // observe the declaration. The `Pending` node is still emitted so the
        // Phase 06 resolve pass keeps updating the shared build context.
        let normalized = normalize_name(name);
        if normalized == "default-role" {
            // An empty argument resets to Docutils' default `title-reference`
            // (which is represented by `None` = no explicit default role).
            let selected = arguments
                .split_whitespace()
                .next()
                .map(|role| role.to_ascii_lowercase())
                .filter(|role| !role.is_empty());
            self.options.default_role = selected;
        } else if normalized == "role" {
            let spec = arguments.trim();
            let (role_name, base_role) = match spec.split_once('(') {
                Some((name, rest)) => {
                    let name = name.trim();
                    let base = rest
                        .trim_end()
                        .trim_end_matches(')')
                        .trim()
                        .to_ascii_lowercase();
                    (name, if base.is_empty() { "generic".to_owned() } else { base })
                }
                None => (spec, "generic".to_owned()),
            };
            if !role_name.is_empty() {
                let language = options.get("language").cloned();
                let classes = options
                    .get("class")
                    .map(|value| value.split_whitespace().map(str::to_owned).collect())
                    .unwrap_or_default();
                self.custom_roles.insert(
                    role_name.to_ascii_lowercase(),
                    crate::role_registry::CustomRoleInfo { base: base_role, language, classes },
                );
            }
        }

        let span = self.span_of(index, self.pos.max(index + 1));
        let line_no = self.line_number(index);
        let node = self.add(NodeKind::Pending, parent, span, line_no)?;
        crate::pending::fill_pending_with_body(
            self.doc.get_mut(node)?,
            &normalize_name(name),
            &arguments,
            &options,
            &content,
            line_no,
            &body_lines,
        );
        if normalize_name(name) == "only" {
            // Sphinx's `Only.run` parses nested sections with `match_titles`
            // and then *raises* the `only` node when the first nested title
            // style matches a shallower level of the surrounding document.
            // The block parser still owns the title-style table here, so
            // record the raise count for the resolve phase to apply.
            let raise = only_section_raise(
                &body_lines,
                self.title_styles(),
                self.section_depth(),
            );
            if raise > 0 {
                self.doc.get_mut(node)?.attrs.set_int(
                    crate::pending::KEY_ONLY_RAISE,
                    raise,
                );
            }
        }
        Ok(())
    }

    /// `.. _name: uri` → `Target`.
    fn parse_target(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
        index: usize,
        rest: &str,
    ) -> RstResult<()> {
        // Consume any continuation lines (a target URI may wrap).
        let extra = self.take_body(index, base_indent);
        let mut text = rest.trim().to_owned();
        for line in extra {
            text.push(' ');
            text.push_str(line.trim());
        }
        let text = text.trim().to_owned();
        let after_underscore = &text[1..];
        let (name_raw, target) = match after_underscore.split_once(':') {
            Some((name, uri)) => (name.trim(), uri.trim()),
            None => ("", after_underscore.trim()),
        };
        let name = unquote(name_raw);
        let normalized = normalize_name(&name);
        // `.. _name: url` is a named target; `.. __: url` (a lone underscore
        // after the marker) is an anonymous target.
        let anonymous = name.is_empty() || name == "_";

        let span = self.span_of(index, self.pos.max(index + 1));
        let line_no = self.line_number(index);
        let node = self.add(NodeKind::Target, parent, span, line_no)?;
        if anonymous {
            self.set_bool(node, "anonymous", true)?;
        } else {
            self.set_list(node, "ids", vec![make_id(&normalized)])?;
            self.set_list(node, "names", vec![normalized])?;
        }
        if !target.is_empty() {
            if let Some(refname) = target.strip_suffix('_') {
                if !refname.is_empty() {
                    self.set_str(node, "refname", normalize_name(refname))?;
                }
            } else {
                self.set_str(node, "refuri", target)?;
            }
        }
        Ok(())
    }

    /// `.. |name| replace:: text` → `SubstitutionDefinition`.
    fn parse_substitution(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
        index: usize,
        rest: &str,
    ) -> RstResult<()> {
        // A substitution body may start after a blank line (`.. |x| raw:: html`
        // followed by an indented raw payload). `take_body` stops at blanks;
        // `take_directive_body` is the Docutils-faithful variant that skips them.
        let (extra, _) = self.take_directive_body(index, base_indent);
        let mut text = rest.to_owned();
        for line in extra {
            // Preserve line boundaries: option lines (`:name: value`) in a
            // replacement directive must remain separable from its arguments.
            text.push('\n');
            text.push_str(line.trim());
        }
        let close = text.find('|').and_then(|start| {
            text[start + 1..].find('|').map(|end| start + 1 + end + 1)
        });
        let Some(close) = close else {
            let span = self.line_span(index);
            self.push_diagnostic(RstError::UnterminatedExplicitMarkup {
                message: "substitution definition name is not closed with '|'".into(),
                span: self.miette_span(span),
            });
            return Ok(());
        };
        let name = whitespace_normalize_name(&text[1..close - 1]);
        let replacement = text[close..].trim_start();
        let (replacement_directive, replacement) = replacement
            .split_once("::")
            .map_or(("", replacement), |(directive, value)| {
                (directive.trim(), value.trim_start())
            });
        let mut sub_options = BTreeMap::new();
        let mut argument_lines = Vec::new();
        for line in replacement.lines() {
            let trimmed = line.trim_end();
            if let Some((opt_name, marker_len)) = crate::lists::field_marker(trimmed) {
                sub_options.insert(
                    normalize_name(&opt_name),
                    trimmed.get(marker_len..).unwrap_or("").trim().to_owned(),
                );
            } else {
                argument_lines.push(trimmed.to_owned());
            }
        }
        let replacement_arguments = argument_lines.join(" ").trim().to_owned();

        let span = self.span_of(index, self.pos.max(index + 1));
        let line_no = self.line_number(index);
        let node = self.add(NodeKind::SubstitutionDefinition, parent, span, line_no)?;
        self.set_list(node, "names", vec![name])?;
        if replacement_directive == "raw" {
            // Docutils builds a `raw` node directly inside the substitution
            // definition (the format is the first argument, the dedented body
            // is the raw text). Phase 05 clones that node into every `|name|`
            // reference, exactly like any other replacement.
            let mut lines = replacement.lines();
            let format = lines
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let body = lines.collect::<Vec<_>>().join("\n");
            let raw = self.add(NodeKind::Raw, node, span, line_no)?;
            self.set_str(raw, "format", format)?;
            self.set_str(raw, "xml:space", "preserve")?;
            self.add_text(body, raw, span, line_no)?;
            return Ok(());
        }
        if matches!(
            replacement_directive,
            "replace" | "unicode" | "image" | "date"
        ) {
            self.substitution_directives.push((
                node,
                crate::parser::SubstitutionDirective {
                    name: replacement_directive.to_ascii_lowercase(),
                    arguments: replacement_arguments.clone(),
                    options: sub_options,
                },
            ));
        }
        // The replacement is parsed for inline markup (Docutils nests a parse
        // of the replacement text); nested substitution references remain as
        // `SubstitutionReference` nodes for a later transform to expand.
        self.add_inline_slice(&replacement_arguments, index, base_indent, node)?;
        Ok(())
    }

    /// `.. [label] ...` → `Pending` with `directive_name` set for Phase 06.
    fn parse_bracket_construct(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
        index: usize,
        rest: &str,
    ) -> RstResult<()> {
        // Docutils fixes a footnote/citation's body indentation from the first
        // indented line after the marker, allowing blank lines in between.
        // `take_body` stops at the first blank and would leave a genuinely
        // indented body (e.g. a nested `.. code-block::`) outside the note as a
        // stray block quote, so use the blank-tolerant directive body reader.
        let (mut lines, _) = self.take_directive_body(index, base_indent);
        let close = rest.find(']');
        let label = close.map_or("", |pos| &rest[1..pos]);
        let kind = if label.starts_with('#') || label == "*" || label.bytes().all(|b| b.is_ascii_digit()) {
            "footnote"
        } else {
            "citation"
        };
        let inline_body = close.map_or("", |pos| rest[pos + 1..].trim());
        if !inline_body.is_empty() {
            lines.insert(0, inline_body.to_owned());
        }
        let span = self.span_of(index, self.pos.max(index + 1));
        let line_no = self.line_number(index);
        let node = self.add(NodeKind::Pending, parent, span, line_no)?;
        let options = BTreeMap::new();
        fill_pending(
            self.doc.get_mut(node)?,
            kind,
            label,
            &options,
            &lines.join("\n"),
            line_no,
        );
        Ok(())
    }

    /// Anything else after `.. ` is a comment.
    fn parse_comment(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
        index: usize,
        rest: &str,
    ) -> RstResult<()> {
        let mut lines = self.take_body(index, base_indent);
        if !rest.is_empty() {
            lines.insert(0, rest.trim_end().to_owned());
        }
        trim_blank_edges(&mut lines);
        let span = self.span_of(index, self.pos.max(index + 1));
        let line_no = self.line_number(index);
        let node = self.add(NodeKind::Comment, parent, span, line_no)?;
        self.set_str(node, "xml:space", "preserve")?;
        self.add_text(lines.join("\n"), node, span, line_no)?;
        Ok(())
    }
}

/// Splits the directive name and inline arguments from `.. name:: args`.
///
/// Returns `None` unless the `::` delimiter is preceded by a simple name and
/// followed by whitespace or end of line (Docutils' `directive` pattern).
fn split_directive(rest: &str) -> Option<(&str, &str)> {
    let pos = rest.find("::")?;
    let name = rest[..pos].trim_end();
    // Docutils' directive names also allow `:` for domain-qualified spellings
    // such as `py:function` (Phase 12).
    if name.is_empty()
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b':'))
    {
        return None;
    }
    let after = &rest[pos + 2..];
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some((name, after.trim()))
}

fn unquote(text: &str) -> String {
    if text.len() >= 2 && text.starts_with('`') && text.ends_with('`') {
        text[1..text.len() - 1].to_owned()
    } else {
        text.to_owned()
    }
}

fn trim_blank_edges(lines: &mut Vec<String>) {
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
}

/// Returns the adornment character of the first section title in a directive
/// body (the first non-blank line followed by a matching adornment line).
fn first_body_section_style(body: &[String]) -> Option<String> {
    for pair in body.windows(2) {
        let title = pair[0].trim();
        if title.is_empty() {
            continue;
        }
        if let Some((ch, width)) = crate::lexer::adornment_of(pair[1].trim()) {
            if width >= crate::lexer::column_width(title) {
                return Some(ch.to_string());
            }
        }
    }
    None
}

/// Number of ancestor sections an `only` wrapper must be raised through so a
/// section parsed inside it lands at the same level as the surrounding
/// document's matching title style. This is `n_sects_to_raise` from Sphinx's
/// `sphinx.directives.other.Only.run`:
/// `current_depth - nested_depth + 1`, where `current_depth` is
/// ``section_stack.len() - 1`` here (docutils subtracts 2 after walking to the
/// document root). A nested style absent from the surrounding styles needs no
/// raising — the content stays at the current level.
fn only_section_raise(body: &[String], title_styles: &[String], section_depth: usize) -> i64 {
    let Some(nested_style) = first_body_section_style(body) else {
        return 0;
    };
    let Some(nested_depth) = title_styles.iter().position(|style| *style == nested_style) else {
        return 0;
    };
    (section_depth as i64).saturating_sub(nested_depth as i64).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_splitting() {
        assert_eq!(split_directive("note:: hello"), Some(("note", "hello")));
        assert_eq!(split_directive("code-block:: python"), Some(("code-block", "python")));
        assert_eq!(split_directive("toctree::"), Some(("toctree", "")));
        assert_eq!(split_directive("note::x"), None);
        assert_eq!(split_directive("not a directive"), None);
    }

    #[test]
    fn quoted_names() {
        assert_eq!(unquote("`my target`"), "my target");
        assert_eq!(unquote("plain"), "plain");
    }
}
