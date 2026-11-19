//! List-family block constructs: bullet, enumerated, definition, field,
//! option and line blocks.
//!
//! Marker patterns are transcribed from Docutils `Body.patterns`
//! (`docutils/parsers/rst/states.py`):
//!
//! * bullet: `[-+*•‣⁃]( +|$)`
//! * enumerator: `(parens|rparen|period)( +|$)` over
//!   `arabic|loweralpha|upperalpha|lowerroman|upperroman|#`
//! * field: `:(?![: ])([^:\]|\\.|:(?!([ `]|$)))*(?<! ):( +|$)`
//! * option: `%(option)s(, %(option)s)*(  +| ?$)`
//! * line block: `\|( +|$)`

use scribe_ast::NodeKind;

use crate::error::RstError;
use crate::parser::{is_line_block, BlockParser};
use crate::RstResult;

/// A recognized bullet marker: the bullet character and the marker's byte
/// length, including the whitespace after it.
pub(crate) struct BulletMarker {
    pub bullet: char,
    pub len: usize,
}

/// Recognizes a Docutils bullet marker at the start of `content`.
pub(crate) fn bullet_marker(content: &str) -> Option<BulletMarker> {
    let mut chars = content.chars();
    let first = chars.next()?;
    if !matches!(first, '-' | '+' | '*' | '\u{2022}' | '\u{2023}' | '\u{2043}') {
        return None;
    }
    let rest = &content[first.len_utf8()..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let spaces = rest.bytes().take_while(|&b| b == b' ').count();
    Some(BulletMarker {
        bullet: first,
        len: first.len_utf8() + spaces,
    })
}

/// A recognized enumerator marker.
#[derive(Clone, Debug)]
pub(crate) struct EnumMarker {
    pub enumtype: String,
    pub prefix: String,
    pub suffix: String,
    pub start: i64,
    pub len: usize,
}

/// Recognizes a Docutils enumerated-list marker at the start of `content`.
pub(crate) fn enumerator_marker(content: &str) -> Option<EnumMarker> {
    let formats = [
        ("(", ")", "parens"),
        ("", ")", "rparen"),
        ("", ".", "period"),
    ];
    for (prefix, suffix, format) in formats {
        if !content.starts_with(prefix) {
            continue;
        }
        let after_prefix = &content[prefix.len()..];
        if !suffix.is_empty() && !after_prefix.contains(suffix) {
            continue;
        }
        let token_end = if suffix.is_empty() {
            after_prefix
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
                .unwrap_or(after_prefix.len())
        } else {
            after_prefix.find(suffix)?
        };
        let token = &after_prefix[..token_end];
        if token.is_empty() {
            continue;
        }
        let after_suffix_start = prefix.len() + token_end + suffix.len();
        let after_suffix = &content[after_suffix_start..];
        if !after_suffix.is_empty() && !after_suffix.starts_with(' ') {
            continue;
        }
        let spaces = after_suffix.bytes().take_while(|&b| b == b' ').count();

        let (enumtype, start) = classify_enumerator(token)?;
        let _ = format;
        return Some(EnumMarker {
            enumtype,
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
            start,
            len: after_suffix_start + spaces,
        });
    }
    None
}

/// Classifies an enumerator token into a Docutils sequence and ordinal.
fn classify_enumerator(token: &str) -> Option<(String, i64)> {
    if token == "#" {
        return Some(("arabic".to_owned(), 1));
    }
    if token.bytes().all(|b| b.is_ascii_digit()) {
        return token.parse::<i64>().ok().map(|n| ("arabic".to_owned(), n));
    }
    if token == "i" {
        return Some(("lowerroman".to_owned(), 1));
    }
    if token == "I" {
        return Some(("upperroman".to_owned(), 1));
    }
    let bytes = token.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        return Some(("loweralpha".to_owned(), i64::from(bytes[0] - b'a') + 1));
    }
    if bytes.len() == 1 && bytes[0].is_ascii_uppercase() {
        return Some(("upperalpha".to_owned(), i64::from(bytes[0] - b'A') + 1));
    }
    if token.bytes().all(|b| matches!(b, b'i' | b'v' | b'x' | b'l' | b'c' | b'd' | b'm')) {
        return roman_to_int(token).map(|n| ("lowerroman".to_owned(), n));
    }
    if token.bytes().all(|b| matches!(b, b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M')) {
        return roman_to_int(token).map(|n| ("upperroman".to_owned(), n));
    }
    None
}

/// Converts a Roman numeral to an integer (Docutils `RomanNumeral.from_string`).
fn roman_to_int(token: &str) -> Option<i64> {
    let value = |b: u8| -> Option<i64> {
        Some(match b.to_ascii_uppercase() {
            b'I' => 1,
            b'V' => 5,
            b'X' => 10,
            b'L' => 50,
            b'C' => 100,
            b'D' => 500,
            b'M' => 1000,
            _ => return None,
        })
    };
    let bytes = token.as_bytes();
    let mut total = 0i64;
    let mut prev = 0i64;
    for &b in bytes.iter().rev() {
        let current = value(b)?;
        if current < prev {
            total -= current;
        } else {
            total += current;
            prev = current;
        }
    }
    (total > 0).then_some(total)
}

/// Recognizes a field marker, returning `(name, marker_len)`.
pub(crate) fn field_marker(content: &str) -> Option<(String, usize)> {
    let bytes = content.as_bytes();
    if bytes.first() != Some(&b':') || bytes.len() < 3 {
        return None;
    }
    if matches!(bytes[1], b':' | b' ') {
        return None;
    }
    let mut i = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2, // escaped char: skip it
            b'|' => return None, // substitution / other construct
            b':' => {
                let next = bytes.get(i + 1).copied();
                if next.is_none() || next == Some(b' ') {
                    let name = content[1..i].trim_end().to_owned();
                    if name.is_empty() {
                        return None;
                    }
                    let mut len = i + 1;
                    while bytes.get(len) == Some(&b' ') {
                        len += 1;
                    }
                    return Some((name, len));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// One option parsed from an option-list marker.
#[derive(Clone, Debug)]
pub(crate) struct ParsedOption {
    pub name: String,
    pub argument: Option<String>,
    pub delimiter: String,
}

/// Recognizes an option-list marker, returning `(options, marker_len)`.
pub(crate) fn option_marker(content: &str) -> Option<(Vec<ParsedOption>, usize)> {
    // Find the run of 2+ spaces that separates the marker from the description.
    let bytes = content.as_bytes();
    let mut in_angle = false;
    let mut split = None;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => in_angle = true,
            b'>' => in_angle = false,
            b' ' if !in_angle => {
                let mut j = i;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j - i >= 2 {
                    split = Some(i);
                    break;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let (marker, len) = match split {
        Some(pos) => (&content[..pos], pos + (bytes[pos..].iter().take_while(|&&b| b == b' ').count())),
        None => {
            let trimmed = content.trim_end();
            (trimmed, trimmed.len())
        }
    };
    if marker.is_empty() {
        return None;
    }

    let mut options = Vec::new();
    for piece in split_options(marker) {
        options.push(parse_option_string(piece)?);
    }
    (!options.is_empty()).then_some((options, len))
}

/// Splits `", "`-separated options, ignoring commas inside `<...>`.
fn split_options(marker: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_angle = false;
    let bytes = marker.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'<' => in_angle = true,
            b'>' => in_angle = false,
            b',' if !in_angle && bytes[i + 1] == b' ' => {
                out.push(&marker[start..i]);
                start = i + 2;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&marker[start..]);
    out
}

/// Parses a single option string into its name and optional argument.
fn parse_option_string(piece: &str) -> Option<ParsedOption> {
    let mut tokens: Vec<String> = piece.split_whitespace().map(str::to_owned).collect();
    let first = tokens.first()?.clone();
    let mut delimiter = " ".to_owned();

    if let Some((left, right)) = first.split_once('=') {
        tokens[0] = left.to_owned();
        tokens.insert(1, right.to_owned());
        delimiter = "=".to_owned();
    } else if first.len() > 2
        && ((first.starts_with('-') && !first.starts_with("--")) || first.starts_with('+'))
        // Docutils' `shortopt` pattern requires an ASCII alphanumeric option
        // character; the guard also keeps the byte-2 slice on a char boundary.
        && first.as_bytes()[1].is_ascii_alphanumeric()
    {
        tokens[0] = first[..2].to_owned();
        tokens.insert(1, first[2..].to_owned());
        delimiter = String::new();
    }

    if tokens.len() > 1 && tokens[1].starts_with('<') && tokens.last().is_some_and(|t| t.ends_with('>')) {
        let joined = tokens[1..].join(" ");
        tokens.truncate(1);
        tokens.push(joined);
    }

    if tokens.is_empty() || tokens.len() > 2 {
        return None;
    }
    let name = tokens[0].clone();
    let argument = tokens.get(1).cloned();
    if !is_valid_option_name(&name) {
        return None;
    }
    Some(ParsedOption {
        name,
        argument,
        delimiter,
    })
}

fn is_valid_option_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("--").or_else(|| name.strip_prefix('/')) {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    } else if let Some(rest) = name.strip_prefix('-').or_else(|| name.strip_prefix('+')) {
        rest.len() == 1 && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    } else {
        false
    }
}

impl<'a> BlockParser<'a> {
    /// Parses a run of bullet list items.
    pub(crate) fn parse_bullet_list(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let bullet = bullet_marker(self.content_at(start, base_indent))
            .map(|m| m.bullet)
            .ok_or_else(|| RstError::Indentation {
                message: "expected bullet marker".into(),
                span: self.miette_span(self.line_span(start)),
            })?;
        let span = self.span_of(start, start + 1);
        let line_no = self.line_number(start);
        let list = self.add(NodeKind::BulletList, parent, span, line_no)?;
        self.set_str(list, "bullet", bullet.to_string())?;

        let (end, tight) = self.parse_list_items(list, base_indent, |p, this| {
            bullet_marker(this.content_at(p, base_indent)).is_some_and(|m| m.bullet == bullet)
        })?;
        self.tight_lists.insert(list.0, tight);
        self.pos = end;
        Ok(())
    }

    /// Parses a run of enumerated list items.
    pub(crate) fn parse_enumerated_list(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let first = enumerator_marker(self.content_at(start, base_indent)).ok_or_else(|| {
            RstError::Indentation {
                message: "expected enumerator marker".into(),
                span: self.miette_span(self.line_span(start)),
            }
        })?;
        let prefix = first.prefix.clone();
        let suffix = first.suffix.clone();
        let enumtype = first.enumtype.clone();
        let start_value = first.start;

        let span = self.span_of(start, start + 1);
        let line_no = self.line_number(start);
        let list = self.add(NodeKind::EnumeratedList, parent, span, line_no)?;
        self.set_str(list, "enumtype", enumtype.clone())?;
        self.set_str(list, "prefix", prefix.clone())?;
        self.set_str(list, "suffix", suffix.clone())?;
        if start_value != 1 {
            self.set_int(list, "start", start_value)?;
        }

        let (end, tight) = self.parse_list_items(list, base_indent, |p, this| {
            enumerator_marker(this.content_at(p, base_indent)).is_some()
        })?;
        self.tight_lists.insert(list.0, tight);
        self.pos = end;
        Ok(())
    }

    /// Shared driver for bullet/enumerated items. Returns `(end, is_tight)`.
    fn parse_list_items<F>(
        &mut self,
        list: scribe_ast::NodeId,
        base_indent: usize,
        is_item: F,
    ) -> RstResult<(usize, bool)>
    where
        F: Fn(usize, &Self) -> bool,
    {
        let first = self.pos;
        let mut last_item_end = first;
        let mut saw_blank_between = false;

        loop {
            let blank_start = self.pos;
            self.skip_blank();
            let had_blank = self.pos > blank_start;
            if self.pos >= self.limit {
                break;
            }
            if self.indent_at(self.pos) != base_indent || !is_item(self.pos, self) {
                break;
            }
            if had_blank {
                saw_blank_between = true;
            }
            let marker_index = self.pos;
            let marker_len = bullet_marker(self.content_at(marker_index, base_indent))
                .map(|m| m.len)
                .or_else(|| enumerator_marker(self.content_at(marker_index, base_indent)).map(|m| m.len))
                .unwrap_or(1);
            let content_col = base_indent + marker_len;
            let marker_span = self.line_span(marker_index);
            let line_no = self.line_number(marker_index);
            let item = self.add(NodeKind::ListItem, list, marker_span, line_no)?;

            // Continuation lines are indented relative to the list marker;
            // the first line's content already starts at `content_col`.
            let (cont_start, cont_end, cont_indent) =
                self.collect_indented_range(marker_index + 1, base_indent);
            let has_inline = self
                .line(marker_index)
                .is_some_and(|line| line.text.len() > content_col);
            let (parse_start, parse_base, body_end) = if has_inline {
                (marker_index, content_col, cont_end.max(marker_index + 1))
            } else {
                (cont_start, cont_indent, cont_end.max(marker_index + 1))
            };
            // A blank line between blocks inside one item also makes the list
            // loose (Docutils `is_tight_list` heuristic).
            if (marker_index + 1..body_end).any(|i| self.lines[i].blank) {
                saw_blank_between = true;
            }

            let saved_limit = self.limit;
            let saved_override = self.indent_override;
            self.limit = body_end;
            self.pos = parse_start;
            self.indent_override = if has_inline {
                Some((marker_index, content_col))
            } else {
                None
            };
            let result = self.parse_blocks(item, parse_base);
            self.indent_override = saved_override;
            self.limit = saved_limit;
            result?;
            self.pos = self.pos.max(body_end);
            last_item_end = self.pos;
        }
        // Restore cursor to just after the final item, leaving trailing blanks
        // for the caller's loop to skip.
        self.pos = last_item_end;
        Ok((last_item_end, !saw_blank_between))
    }

    /// Parses a definition list (term + classifier(s) + indented definition).
    pub(crate) fn parse_definition_list(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let span = self.span_of(start, start + 1);
        let line_no = self.line_number(start);
        let list = self.add(NodeKind::DefinitionList, parent, span, line_no)?;

        while self.pos < self.limit {
            self.skip_blank();
            if self.pos >= self.limit || self.indent_at(self.pos) != base_indent {
                break;
            }
            let term_index = self.pos;
            let term_text = self.content_at(term_index, base_indent).trim_end().to_owned();
            let next = term_index + 1;
            if self
                .line(next)
                .is_none_or(|line| line.blank || line.indent_cols <= base_indent)
            {
                break;
            }
            let (_, body_end, body_indent) = self.collect_indented_range(next, base_indent);

            let item = self.add(
                NodeKind::DefinitionListItem,
                list,
                self.span_of(term_index, body_end),
                self.line_number(term_index),
            )?;
            self.add_term_and_classifiers(item, &term_text, term_index, base_indent)?;

            let def = self.add(
                NodeKind::Definition,
                item,
                self.span_of(next, body_end),
                self.line_number(next),
            )?;
            let saved_limit = self.limit;
            self.limit = body_end;
            self.pos = next;
            let result = self.parse_blocks(def, body_indent);
            self.limit = saved_limit;
            result?;
            self.pos = self.pos.max(body_end);
        }
        Ok(())
    }

    /// Adds a `Term` plus `Classifier` nodes for a definition-list term line.
    ///
    /// Each classifier is a first-class upstream `classifier` node (matching
    /// `sphinx-build -b pseudoxml`), not an `inline` with a CSS class.
    fn add_term_and_classifiers(
        &mut self,
        item: scribe_ast::NodeId,
        term_text: &str,
        line_index: usize,
        base_indent: usize,
    ) -> RstResult<()> {
        let parts = split_classifiers(term_text);
        let span = self.line_span(line_index);
        let line_no = self.line_number(line_index);
        let term = self.add(NodeKind::Term, item, span, line_no)?;
        self.add_inline_slice(parts[0].trim_end(), line_index, base_indent, term)?;
        for classifier in parts.iter().skip(1) {
            let node = self.add(NodeKind::Classifier, item, span, line_no)?;
            self.add_inline_slice(classifier.trim(), line_index, base_indent, node)?;
        }
        Ok(())
    }

    /// Parses a field list (`:name: body`).
    pub(crate) fn parse_field_list(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let list = self.add(
            NodeKind::FieldList,
            parent,
            self.span_of(start, start + 1),
            self.line_number(start),
        )?;

        while self.pos < self.limit {
            self.skip_blank();
            if self.pos >= self.limit || self.indent_at(self.pos) != base_indent {
                break;
            }
            let idx = self.pos;
            let content = self.content_at(idx, base_indent);
            let Some((name, marker_len)) = field_marker(content) else {
                break;
            };
            let content_col = base_indent + marker_len;
            let field = self.add(
                NodeKind::Field,
                list,
                self.line_span(idx),
                self.line_number(idx),
            )?;
            let span = self.line_span(idx);
            let line_no = self.line_number(idx);
            let name_node = self.add(NodeKind::FieldName, field, span, line_no)?;
            self.add_inline_slice(&name, idx, base_indent, name_node)?;

            let body = self.add(NodeKind::FieldBody, field, span, line_no)?;
            let (cont_start, cont_end, cont_indent) =
                self.collect_indented_range(idx + 1, base_indent);
            let has_inline = self.line(idx).is_some_and(|line| line.text.len() > content_col);
            let (parse_start, parse_base, body_end) = if has_inline {
                (idx, content_col, cont_end.max(idx + 1))
            } else {
                (cont_start, cont_indent, cont_end.max(idx + 1))
            };
            let saved_limit = self.limit;
            let saved_override = self.indent_override;
            self.limit = body_end;
            self.pos = parse_start;
            self.indent_override = if has_inline {
                Some((idx, content_col))
            } else {
                None
            };
            let result = self.parse_blocks(body, parse_base);
            self.indent_override = saved_override;
            self.limit = saved_limit;
            result?;
            self.pos = self.pos.max(body_end);
        }
        Ok(())
    }

    /// Parses an option list (`-a  description`, `--long  description`, ...).
    pub(crate) fn parse_option_list(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let list = self.add(
            NodeKind::OptionList,
            parent,
            self.span_of(start, start + 1),
            self.line_number(start),
        )?;

        while self.pos < self.limit {
            self.skip_blank();
            if self.pos >= self.limit || self.indent_at(self.pos) != base_indent {
                break;
            }
            let idx = self.pos;
            let content = self.content_at(idx, base_indent);
            let Some((options, marker_len)) = option_marker(content) else {
                break;
            };
            let content_col = base_indent + marker_len;
            let has_desc = self.line(idx).is_some_and(|line| line.text.len() > content_col);
            if !has_desc {
                break;
            }
            let item = self.add(
                NodeKind::OptionListItem,
                list,
                self.line_span(idx),
                self.line_number(idx),
            )?;
            let group = self.add(
                NodeKind::OptionGroup,
                item,
                self.line_span(idx),
                self.line_number(idx),
            )?;
            for option in &options {
                let opt = self.add(
                    NodeKind::Option,
                    group,
                    self.line_span(idx),
                    self.line_number(idx),
                )?;
                let name_node = self.add(
                    NodeKind::OptionString,
                    opt,
                    self.line_span(idx),
                    self.line_number(idx),
                )?;
                self.add_text(option.name.clone(), name_node, self.line_span(idx), self.line_number(idx))?;
                if let Some(argument) = &option.argument {
                    let arg_node = self.add(
                        NodeKind::OptionArgument,
                        opt,
                        self.line_span(idx),
                        self.line_number(idx),
                    )?;
                    self.set_str(arg_node, "delimiter", option.delimiter.clone())?;
                    self.add_text(argument.clone(), arg_node, self.line_span(idx), self.line_number(idx))?;
                }
            }

            let desc = self.add(
                NodeKind::Description,
                item,
                self.line_span(idx),
                self.line_number(idx),
            )?;
            let (_, continuation_end, _) = self.collect_indented_range(idx + 1, base_indent);
            let body_end = continuation_end.max(idx + 1);
            let saved_limit = self.limit;
            let saved_override = self.indent_override;
            self.limit = body_end;
            self.pos = idx;
            self.indent_override = Some((idx, content_col));
            let result = self.parse_blocks(desc, content_col);
            self.indent_override = saved_override;
            self.limit = saved_limit;
            result?;
            self.pos = self.pos.max(body_end);
        }
        Ok(())
    }

    /// Parses a line block (`| line`).
    pub(crate) fn parse_line_block(
        &mut self,
        parent: scribe_ast::NodeId,
        base_indent: usize,
    ) -> RstResult<()> {
        let start = self.pos;
        let block = self.add(
            NodeKind::LineBlock,
            parent,
            self.span_of(start, start + 1),
            self.line_number(start),
        )?;
        // Stack of `(indent_after_bar, line_block_node_id)`.
        let mut stack: Vec<(usize, scribe_ast::NodeId)> = Vec::new();
        let mut first_indent: Option<usize> = None;

        while self.pos < self.limit {
            let idx = self.pos;
            let content = self.content_at(idx, base_indent);
            if self.indent_at(idx) != base_indent || !is_line_block(content) {
                break;
            }
            let after_bar = &content[1..];
            let spaces = after_bar.bytes().take_while(|&b| b == b' ').count();
            let indent = spaces.saturating_sub(1); // Docutils: len(group(1)) - 1
            let text = after_bar[spaces..].to_owned();
            if first_indent.is_none() {
                first_indent = Some(indent);
            }
            while stack.last().is_some_and(|(level, _)| *level >= indent) {
                stack.pop();
            }
            let target = stack.last().map_or(block, |(_, id)| *id);
            let line = self.add(
                NodeKind::Line,
                target,
                self.line_span(idx),
                self.line_number(idx),
            )?;
            self.add_inline_slice(&text, idx, base_indent, line)?;
            stack.push((indent, block));
            self.pos += 1;
        }
        Ok(())
    }
}

/// Splits a definition-list term line at Docutils' ` +: +` classifier delimiter.
fn split_classifiers(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && bytes.get(j + 1) == Some(&b' ') {
                parts.push(std::mem::take(&mut current));
                i = j + 2;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                continue;
            }
            current.push(' ');
            i = j;
            continue;
        }
        current.push(bytes[i] as char);
        i += 1;
    }
    parts.push(current);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullet_markers() {
        assert_eq!(bullet_marker("* item").map(|m| (m.bullet, m.len)), Some(('*', 2)));
        assert_eq!(bullet_marker("-  item").map(|m| (m.bullet, m.len)), Some(('-', 3)));
        assert_eq!(bullet_marker("• item").map(|m| (m.bullet, m.len)), Some(('\u{2022}', 4)));
        assert!(bullet_marker("*item").is_none());
    }

    #[test]
    fn enumerator_markers() {
        let m = enumerator_marker("1. item").expect("arabic");
        assert_eq!((m.enumtype.as_str(), m.prefix.as_str(), m.suffix.as_str(), m.start), ("arabic", "", ".", 1));
        let m = enumerator_marker("(3) item").expect("parens");
        assert_eq!((m.enumtype.as_str(), m.prefix.as_str(), m.suffix.as_str(), m.start), ("arabic", "(", ")", 3));
        assert_eq!(enumerator_marker("a) item").map(|m| (m.enumtype, m.start)), Some(("loweralpha".into(), 1)));
        assert_eq!(enumerator_marker("iv. item").map(|m| (m.enumtype, m.start)), Some(("lowerroman".into(), 4)));
        assert_eq!(enumerator_marker("#. item").map(|m| m.enumtype), Some("arabic".into()));
    }

    #[test]
    fn field_and_option_markers() {
        assert_eq!(field_marker(":name: value"), Some(("name".into(), 7)));
        assert_eq!(field_marker("::not a field"), None);
        assert_eq!(field_marker(":a b: x"), Some(("a b".into(), 6)));
        let (opts, len) = option_marker("-a  description").expect("short opt");
        assert_eq!(len, 4);
        assert_eq!(opts[0].name, "-a");
        let (opts, _) = option_marker("--long=VALUE  desc").expect("long opt");
        assert_eq!(opts[0].name, "--long");
        assert_eq!(opts[0].argument.as_deref(), Some("VALUE"));
        assert_eq!(opts[0].delimiter, "=");
    }

    #[test]
    fn classifiers_split() {
        assert_eq!(split_classifiers("term : one : two"), vec!["term", "one", "two"]);
        assert_eq!(split_classifiers("just a term"), vec!["just a term"]);
    }
}
