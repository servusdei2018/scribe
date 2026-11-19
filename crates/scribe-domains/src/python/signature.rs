//! Python signature parser and `desc_*` node construction.
//!
//! Hand-written, allocation-light tokenizers mirror the upstream regexes and
//! `ast`-based parsers in `sphinx/domains/python/{_object,_annotations}.py`:
//!
//! * [`parse_sig_re`] is `py_sig_re` without backtracking blowups;
//! * [`parse_annotation`] tokenizes a PEP 484/604 type expression and rewrites
//!   `Optional[X]` / `Union[X, Y]` the way `_parse_annotation` does;
//! * [`build_desc_signature`] emits the frozen `desc_*`/`desc_sig_*` subtree;
//! * [`rewrite_content_fields`] turns typed field bodies into xref annotations;
//! * [`prepare_document`] expands every skeleton `desc` node and annotates
//!   pending `py` xrefs with their `py:module` / `py:class` read context.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, Node, NodeId, NodeKind, Span};

use crate::error::{DomainError, DomainResult};
use crate::python::py_make_id;

/// Directive options relevant to Python object descriptions.
///
/// Serialized to JSON on the skeleton `desc` node under `py:options` by the
/// `scribe-rst` directive handler.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PyOptions {
    pub module: Option<String>,
    pub canonical: Option<String>,
    pub annotation: Option<String>,
    pub r#type: Option<String>,
    pub value: Option<String>,
    pub synopsis: Option<String>,
    pub platform: Option<String>,
    pub no_index: bool,
    pub no_index_entry: bool,
    pub no_contents_entry: bool,
    pub no_typesetting: bool,
    pub deprecated: bool,
    pub async_: bool,
    pub final_: bool,
    pub abstract_: bool,
    pub abstractmethod: bool,
    pub classmethod: bool,
    pub staticmethod: bool,
    pub single_line_parameter_list: bool,
    pub single_line_type_parameter_list: bool,
    pub use_unqualified_type_names: bool,
    pub add_module_names: bool,
}

impl PyOptions {
    /// Decodes the JSON payload stored on a skeleton desc node.
    pub fn from_node(node: &Node) -> Self {
        let Some(raw) = node.get_str("py:options").ok().flatten() else {
            return Self::default();
        };
        serde_json::from_str(raw).unwrap_or_default()
    }
}

/// Read-time module/class context, mirroring `env.ref_context['py:module']`
/// and `env.ref_context['py:class']`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SigContext {
    pub module: Option<String>,
    pub class: Option<String>,
}

/// Result of signature/name resolution, stored on the `desc` node for
/// [`crate::PythonDomain::process_doc`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureInfo {
    pub fullname: String,
    pub classname: String,
    pub module: Option<String>,
    pub anchor: String,
}

// ----- py_sig_re -------------------------------------------------------------

/// The five capture groups of upstream's `py_sig_re`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedSig {
    /// Dotted class-name prefix including the trailing dot.
    pub prefix: String,
    /// The object's own name.
    pub name: String,
    /// Type parameter list text (without brackets).
    pub type_params: String,
    /// Argument list text (without parentheses).
    pub arglist: String,
    /// Return annotation text (after `->`).
    pub retann: String,
}

fn is_word(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// Hand-written equivalent of `py_sig_re` (no regex backtracking).
pub fn parse_sig_re(sig: &str) -> Option<ParsedSig> {
    let sig = sig.trim();
    if sig.is_empty() {
        return None;
    }
    let chars: Vec<(usize, char)> = sig.char_indices().collect();
    let mut pos = 0usize;

    // `([\w.]*\.)?(\w+)`
    let start = pos;
    while pos < chars.len() && (is_word(chars[pos].1) || chars[pos].1 == '.') {
        pos += 1;
    }
    if pos == start {
        return None;
    }
    let head: String = chars[start..pos].iter().map(|(_, ch)| *ch).collect();
    let dot = head.rfind('.');
    let (prefix, name) = match dot {
        Some(index) => (head[..=index].to_owned(), head[index + 1..].to_owned()),
        None => (String::new(), head.clone()),
    };
    if name.is_empty() {
        return None;
    }

    skip_ws(&chars, &mut pos);

    // optional `[ type parameters ]`
    let mut type_params = String::new();
    if pos < chars.len() && chars[pos].1 == '[' {
        let (inner, next) = read_balanced(&chars, pos, '[', ']')?;
        type_params = inner;
        pos = next;
        skip_ws(&chars, &mut pos);
    }

    // optional `( args )` with optional `-> retann`
    let mut arglist = String::new();
    let mut retann = String::new();
    if pos < chars.len() && chars[pos].1 == '(' {
        let (inner, next) = read_balanced(&chars, pos, '(', ')')?;
        arglist = inner;
        pos = next;
        skip_ws(&chars, &mut pos);
        if pos + 1 < chars.len() && chars[pos].1 == '-' && chars[pos + 1].1 == '>' {
            pos += 2;
            let rest: String = chars[pos..].iter().map(|(_, ch)| *ch).collect::<String>();
            retann = rest.trim().to_owned();
            pos = chars.len();
        }
    }
    if pos != chars.len() {
        return None;
    }
    Some(ParsedSig { prefix, name, type_params, arglist, retann })
}

fn skip_ws(chars: &[(usize, char)], pos: &mut usize) {
    while *pos < chars.len() && chars[*pos].1.is_whitespace() {
        *pos += 1;
    }
}

/// Reads from an opening delimiter to its matching close, tracking nesting and
/// quoted strings. Returns the inner text and the index just after the close.
fn read_balanced(
    chars: &[(usize, char)],
    open: usize,
    left: char,
    right: char,
) -> Option<(String, usize)> {
    if chars.get(open).map(|(_, ch)| *ch) != Some(left) {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut index = open;
    while index < chars.len() {
        let ch = chars[index].1;
        if let Some(q) = quote {
            if ch == '\\' {
                index += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
        } else {
            match ch {
                '\'' | '"' => quote = Some(ch),
                c if c == left => depth += 1,
                c if c == right => {
                    depth -= 1;
                    if depth == 0 {
                        let inner: String = chars[open + 1..index].iter().map(|(_, c)| *c).collect();
                        return Some((inner, index + 1));
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

// ----- annotation tokenizer -------------------------------------------------

/// One token of a type annotation, ready to be materialized as a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnToken {
    /// A (possibly dotted) name → `pending_xref`.
    Name(String),
    /// A name with a leading `~` (display last segment, refspecific).
    ShortName(String),
    /// A name with a leading `.` (search specific namespaces first).
    SpecificName(String),
    /// Brackets / comma / colon / pipe / ellipsis.
    Punct(String),
    /// Whitespace.
    Space,
    /// `True` / `False` keyword constants.
    Keyword(String),
    /// Numeric literal.
    Number(String),
    /// Quoted string literal (raw, with quotes).
    Str(String),
    /// `*`, `**`, `=`.
    Operator(String),
}

/// Upstream `parse_reftarget`: `(reftype, target, title, refspecific)`.
pub fn parse_reftarget(
    target: &str,
    suppress_prefix: bool,
) -> (&'static str, String, String, bool) {
    let (reftarget, title, refspecific) = if let Some(rest) = target.strip_prefix('.') {
        (rest.to_owned(), rest.to_owned(), true)
    } else if let Some(rest) = target.strip_prefix('~') {
        (rest.to_owned(), rest.rsplit('.').next().unwrap_or(rest).to_owned(), false)
    } else if let Some(rest) = target.strip_prefix("typing.") {
        (target.to_owned(), rest.to_owned(), false)
    } else if suppress_prefix {
        (target.to_owned(), target.rsplit('.').next().unwrap_or(target).to_owned(), false)
    } else {
        (target.to_owned(), target.to_owned(), false)
    };
    if reftarget == "None" || reftarget.starts_with("typing.") {
        ("obj", reftarget, title, refspecific)
    } else {
        ("class", reftarget, title, refspecific)
    }
}

/// Tokenizes a type annotation, applying the upstream `Optional` / `Union`
/// rewrites and `~` / `.` prefix handling.
pub fn parse_annotation(text: &str) -> Vec<AnnToken> {
    let raw = tokenize_annotation(text);
    let expanded = expand_aliases(&raw);
    expanded
        .into_iter()
        .map(|token| match token {
            AnnToken::Name(name) if name == "True" || name == "False" => AnnToken::Keyword(name),
            other => other,
        })
        .collect()
}

fn tokenize_annotation(text: &str) -> Vec<AnnToken> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            if !matches!(tokens.last(), Some(AnnToken::Space)) {
                tokens.push(AnnToken::Space);
            }
            i += 1;
        } else if ch == '.' && i + 2 < chars.len() && chars[i + 1] == '.' && chars[i + 2] == '.' {
            tokens.push(AnnToken::Punct("...".to_owned()));
            i += 3;
        } else if ch == '~' {
            tokens.push(AnnToken::Punct("~".to_owned()));
            i += 1;
        } else if ch == '\'' || ch == '"' {
            let quote = ch;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(AnnToken::Str(chars[start..i.min(chars.len())].iter().collect()));
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            tokens.push(AnnToken::Number(chars[start..i].iter().collect()));
        } else if is_word(ch) || (ch == '.' && i + 1 < chars.len() && is_word(chars[i + 1])) {
            let start = i;
            while i < chars.len() && (is_word(chars[i]) || chars[i] == '.') {
                // A lone dot is punctuation unless followed by a word char.
                if chars[i] == '.' && !(i + 1 < chars.len() && is_word(chars[i + 1])) {
                    break;
                }
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            tokens.push(AnnToken::Name(name));
        } else if matches!(ch, '[' | ']' | '(' | ')' | ',' | ':' | '|') {
            tokens.push(AnnToken::Punct(ch.to_string()));
            i += 1;
        } else if matches!(ch, '*' | '=') {
            tokens.push(AnnToken::Operator(ch.to_string()));
            i += 1;
        } else {
            // Unknown punctuation: preserve it verbatim.
            tokens.push(AnnToken::Punct(ch.to_string()));
            i += 1;
        }
    }
    // `~Name` becomes a short name, `.Name` a specific name.
    let mut result = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            AnnToken::Punct(punct) if punct == "~" => {
                if let Some(AnnToken::Name(name)) = tokens.get(index + 1) {
                    result.push(AnnToken::ShortName(name.clone()));
                    index += 2;
                    continue;
                }
            }
            AnnToken::Punct(punct) if punct == "." => {
                if let Some(AnnToken::Name(name)) = tokens.get(index + 1) {
                    result.push(AnnToken::SpecificName(name.clone()));
                    index += 2;
                    continue;
                }
            }
            _ => {}
        }
        result.push(tokens[index].clone());
        index += 1;
    }
    result
}

/// Rewrites `Optional[X]` → `X | None`, `Union[X, Y]` → `X | Y` (including
/// `typing.` qualified spellings), matching `_unparse_pep_604_annotation`.
fn expand_aliases(tokens: &[AnnToken]) -> Vec<AnnToken> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        let is_optional = matches!(&tokens[i], AnnToken::Name(name) if name == "Optional" || name == "typing.Optional");
        let is_union = matches!(&tokens[i], AnnToken::Name(name) if name == "Union" || name == "typing.Union");
        if (is_optional || is_union)
            && matches!(tokens.get(i + 1), Some(AnnToken::Punct(p)) if p == "[")
        {
            if let Some(end) = matching_bracket(tokens, i + 1) {
                let mut inner = tokens[i + 2..end].to_vec();
                if is_union {
                    inner = replace_top_level_commas(&inner);
                } else {
                    while matches!(inner.last(), Some(AnnToken::Space)) {
                        inner.pop();
                    }
                    inner.extend([
                        AnnToken::Space,
                        AnnToken::Punct("|".to_owned()),
                        AnnToken::Space,
                        AnnToken::Name("None".to_owned()),
                    ]);
                }
                // Any surrounding whitespace in `inner` is already normalized.
                out.extend(inner);
                i = end + 1;
                continue;
            }
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

fn matching_bracket(tokens: &[AnnToken], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            AnnToken::Punct(p) if p == "[" => depth += 1,
            AnnToken::Punct(p) if p == "]" => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn replace_top_level_commas(tokens: &[AnnToken]) -> Vec<AnnToken> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut skip_space = false;
    for token in tokens {
        if skip_space && matches!(token, AnnToken::Space) {
            continue;
        }
        skip_space = false;
        match token {
            AnnToken::Punct(p) if p == "[" || p == "(" => depth += 1,
            AnnToken::Punct(p) if p == "]" || p == ")" => depth = depth.saturating_sub(1),
            AnnToken::Punct(p) if p == "," && depth == 0 => {
                out.extend([AnnToken::Space, AnnToken::Punct("|".to_owned()), AnnToken::Space]);
                skip_space = true;
                continue;
            }
            _ => {}
        }
        out.push(token.clone());
    }
    out
}

// ----- argument list tokenizer ----------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParamToken {
    PositionalOnlySeparator,
    KeywordOnlySeparator,
    Param {
        name: String,
        annotation: String,
        default: String,
        var: u8, // 0 = normal, 1 = *args, 2 = **kwargs
    },
}

fn split_top_level(text: &str, delimiter: char) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            current.push(ch);
            if ch == '\\' && i + 1 < chars.len() {
                i += 1;
                current.push(chars[i]);
            } else if ch == q {
                quote = None;
            }
        } else {
            match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    current.push(ch);
                }
                '[' | '(' | '{' => {
                    depth += 1;
                    current.push(ch);
                }
                ']' | ')' | '}' => {
                    depth -= 1;
                    current.push(ch);
                }
                c if c == delimiter && depth == 0 => {
                    parts.push(current.trim().to_owned());
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
        i += 1;
    }
    parts.push(current.trim().to_owned());
    parts
}

fn split_once_top(text: &str, delimiter: char) -> Option<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, ch) in chars.iter().enumerate() {
        if let Some(q) = quote {
            if *ch == '\\' {
                continue;
            }
            if *ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(*ch),
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth -= 1,
            c if *c == delimiter && depth == 0 => {
                let left: String = chars[..i].iter().collect();
                let right: String = chars[i + 1..].iter().collect();
                return Some((left.trim().to_owned(), right.trim().to_owned()));
            }
            _ => {}
        }
    }
    None
}

fn tokenize_arglist(arglist: &str) -> Vec<ParamToken> {
    if arglist.trim().is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for part in split_top_level(arglist, ',') {
        if part.is_empty() {
            continue;
        }
        if part == "/" {
            result.push(ParamToken::PositionalOnlySeparator);
            continue;
        }
        if part == "*" {
            result.push(ParamToken::KeywordOnlySeparator);
            continue;
        }
        let (var, body) = if let Some(rest) = part.strip_prefix("**") {
            (2u8, rest.trim())
        } else if let Some(rest) = part.strip_prefix('*') {
            (1u8, rest.trim())
        } else {
            (0u8, part.as_str())
        };
        let (head, default) = split_once_top(body, '=').unwrap_or((body.to_owned(), String::new()));
        let (name, annotation) = split_once_top(&head, ':').unwrap_or((head.clone(), String::new()));
        result.push(ParamToken::Param {
            name: name.trim().to_owned(),
            annotation: annotation.trim().to_owned(),
            default: default.trim().to_owned(),
            var,
        });
    }
    result
}

// ----- node construction ----------------------------------------------------

struct Builder<'a> {
    doc: &'a mut Document,
    span: Span,
}

impl<'a> Builder<'a> {
    fn add(&mut self, kind: NodeKind, parent: NodeId) -> DomainResult<NodeId> {
        let mut node = Node::new(kind);
        node.span = Some(self.span);
        self.doc.add_node(node, Some(parent)).map_err(|error| DomainError::InvalidData {
            domain: "py".to_owned(),
            detail: error.to_string(),
        })
    }

    fn add_text(&mut self, text: impl Into<String>, parent: NodeId) -> DomainResult<NodeId> {
        let mut node = Node::text(text);
        node.span = Some(self.span);
        self.doc.add_node(node, Some(parent)).map_err(|error| DomainError::InvalidData {
            domain: "py".to_owned(),
            detail: error.to_string(),
        })
    }

    fn classes(&mut self, id: NodeId, classes: &[&str]) -> DomainResult<()> {
        let node = self.doc.get_mut(id).map_err(|error| DomainError::InvalidData {
            domain: "py".to_owned(),
            detail: error.to_string(),
        })?;
        node.attrs.set_list("classes", classes.iter().map(|class| (*class).to_owned()).collect());
        Ok(())
    }

    fn sig(&mut self, kind: NodeKind, text: &str, css: &str, parent: NodeId) -> DomainResult<NodeId> {
        let id = self.add(kind, parent)?;
        self.classes(id, &[css])?;
        if !text.is_empty() {
            self.add_text(text, id)?;
        }
        Ok(id)
    }
}

fn sig_class(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::DescSigSpace => "w",
        NodeKind::DescSigName => "n",
        NodeKind::DescSigOperator => "o",
        NodeKind::DescSigPunctuation => "p",
        NodeKind::DescSigKeyword => "k",
        NodeKind::DescSigKeywordType => "kt",
        NodeKind::DescSigLiteralNumber => "m",
        NodeKind::DescSigLiteralString => "s",
        NodeKind::DescSigLiteralChar => "sc",
        _ => "n",
    }
}

/// Emits annotation nodes from a token stream, registering type xrefs.
fn emit_annotation(
    builder: &mut Builder<'_>,
    tokens: &[AnnToken],
    parent: NodeId,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<()> {
    for token in tokens {
        match token {
            AnnToken::Space => {
                builder.sig(NodeKind::DescSigSpace, " ", "w", parent)?;
            }
            AnnToken::Punct(punct) => {
                let kind = match punct.as_str() {
                    "..." => NodeKind::DescSigPunctuation,
                    _ => NodeKind::DescSigPunctuation,
                };
                builder.sig(kind, punct, "p", parent)?;
            }
            AnnToken::Operator(op) => {
                builder.sig(NodeKind::DescSigOperator, op, "o", parent)?;
            }
            AnnToken::Keyword(word) => {
                builder.sig(NodeKind::DescSigKeyword, word, "k", parent)?;
            }
            AnnToken::Number(number) => {
                builder.sig(NodeKind::DescSigLiteralNumber, number, "m", parent)?;
            }
            AnnToken::Str(text) => {
                builder.sig(NodeKind::DescSigLiteralString, text, "s", parent)?;
            }
            AnnToken::Name(name)
            | AnnToken::ShortName(name)
            | AnnToken::SpecificName(name) => {
                let suppress = matches!(token, AnnToken::ShortName(_));
                let specific = matches!(token, AnnToken::SpecificName(_));
                let prefixed = if suppress {
                    format!("~{name}")
                } else if specific {
                    format!(".{name}")
                } else {
                    name.clone()
                };
                let (reftype, target, title, refspecific) = parse_reftarget(&prefixed, suppress);
                let xref = builder.add(NodeKind::PendingXref, parent)?;
                {
                    let node = builder.doc.get_mut(xref)?;
                    let mut view = node.pending_xref_mut().ok_or_else(|| DomainError::InvalidData {
                        domain: "py".to_owned(),
                        detail: "pending_xref view".to_owned(),
                    })?;
                    view.set_refdomain(Some("py".to_owned()));
                    view.set_reftype(Some(reftype.to_owned()));
                    view.set_reftarget(Some(target));
                    view.set_refspecific(Some(refspecific));
                    view.set_refexplicit(Some(false));
                    view.set_classes(vec![
                        "xref".to_owned(),
                        "py".to_owned(),
                        format!("py-{reftype}"),
                    ]);
                }
                {
                    let node = builder.doc.get_mut(xref)?;
                    if let Some(module) = &context.module {
                        node.attrs.set_str("py:module", module);
                    }
                    if let Some(class) = &context.class {
                        node.attrs.set_str("py:class", class);
                    }
                }
                if options.use_unqualified_type_names {
                    // Upstream `type_to_xref` emits two conditions: the short
                    // name for a resolved reference and the full title for the
                    // fallback (`*`) branch.
                    let short = title.rsplit('.').next().unwrap_or(&title).to_owned();
                    let resolved = builder.add(NodeKind::PendingXrefCondition, xref)?;
                    if let Ok(node) = builder.doc.get_mut(resolved) {
                        node.attrs.set_str("condition", "resolved");
                    }
                    builder.add_text(&short, resolved)?;
                    let fallback = builder.add(NodeKind::PendingXrefCondition, xref)?;
                    if let Ok(node) = builder.doc.get_mut(fallback) {
                        node.attrs.set_str("condition", "*");
                    }
                    builder.add_text(&title, fallback)?;
                    if let Ok(node) = builder.doc.get_mut(xref) {
                        node.attrs.set_node_ids("resolve_conditions", vec![resolved, fallback]);
                    }
                } else {
                    builder.add_text(&title, xref)?;
                }
            }
        }
    }
    Ok(())
}

fn prefix_keywords(builder: &mut Builder<'_>, signode: NodeId, words: &[&str]) -> DomainResult<()> {
    if words.is_empty() {
        return Ok(());
    }
    let annotation = builder.add(NodeKind::DescAnnotation, signode)?;
    for word in words {
        builder.sig(NodeKind::DescSigKeyword, word, sig_class(NodeKind::DescSigKeyword), annotation)?;
        builder.sig(NodeKind::DescSigSpace, " ", "w", annotation)?;
    }
    Ok(())
}

fn build_type_params(
    builder: &mut Builder<'_>,
    signode: NodeId,
    text: &str,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<()> {
    let list = builder.add(NodeKind::DescTypeParameterList, signode)?;
    for part in split_top_level(text, ',') {
        if part.is_empty() {
            continue;
        }
        let node = builder.add(NodeKind::DescTypeParameter, list)?;
        let (head, default) = split_once_top(&part, '=').unwrap_or((part.clone(), String::new()));
        let (name_part, bound) = split_once_top(&head, ':').unwrap_or((head.clone(), String::new()));
        let (var, name) = if let Some(rest) = name_part.strip_prefix("**") {
            ("**", rest.trim())
        } else if let Some(rest) = name_part.strip_prefix('*') {
            ("*", rest.trim())
        } else {
            ("", name_part.as_str())
        };
        if !var.is_empty() {
            builder.sig(NodeKind::DescSigOperator, var, "o", node)?;
        }
        builder.sig(NodeKind::DescSigName, name, "n", node)?;
        if !bound.is_empty() {
            builder.sig(NodeKind::DescSigPunctuation, ":", "p", node)?;
            let wrapper = builder.sig(NodeKind::DescSigName, "", "n", node)?;
            emit_annotation(builder, &parse_annotation(&bound), wrapper, context, options)?;
        }
        if !default.is_empty() {
            builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
            builder.sig(NodeKind::DescSigOperator, "=", "o", node)?;
            builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
            let inline = builder.add(NodeKind::Inline, node)?;
            builder.classes(inline, &["default_value"])?;
            builder.add_text(default, inline)?;
        }
    }
    Ok(())
}

fn build_parameterlist(
    builder: &mut Builder<'_>,
    signode: NodeId,
    arglist: &str,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<()> {
    let list = builder.add(NodeKind::DescParameterlist, signode)?;
    {
        let node = builder.doc.get_mut(list)?;
        node.attrs.set_bool("multi_line_parameter_list", false);
        node.attrs.set_bool("multi_line_trailing_comma", true);
    }
    for token in tokenize_arglist(arglist) {
        match token {
            ParamToken::PositionalOnlySeparator => {
                let param = builder.add(NodeKind::DescParameter, list)?;
                let op = builder.sig(NodeKind::DescSigOperator, "/", "o", param)?;
                builder.classes(op, &["positional-only-separator"])?;
            }
            ParamToken::KeywordOnlySeparator => {
                let param = builder.add(NodeKind::DescParameter, list)?;
                let op = builder.sig(NodeKind::DescSigOperator, "*", "o", param)?;
                builder.classes(op, &["keyword-only-separator"])?;
            }
            ParamToken::Param { name, annotation, default, var } => {
                let param = builder.add(NodeKind::DescParameter, list)?;
                if var > 0 {
                    builder.sig(NodeKind::DescSigOperator, if var == 2 { "**" } else { "*" }, "o", param)?;
                }
                builder.sig(NodeKind::DescSigName, &name, "n", param)?;
                if !annotation.is_empty() {
                    builder.sig(NodeKind::DescSigPunctuation, ":", "p", param)?;
                    builder.sig(NodeKind::DescSigSpace, " ", "w", param)?;
                    let wrapper = builder.sig(NodeKind::DescSigName, "", "n", param)?;
                    emit_annotation(builder, &parse_annotation(&annotation), wrapper, context, options)?;
                }
                if !default.is_empty() {
                    if annotation.is_empty() {
                        builder.sig(NodeKind::DescSigOperator, "=", "o", param)?;
                    } else {
                        builder.sig(NodeKind::DescSigSpace, " ", "w", param)?;
                        builder.sig(NodeKind::DescSigOperator, "=", "o", param)?;
                        builder.sig(NodeKind::DescSigSpace, " ", "w", param)?;
                    }
                    let inline = builder.add(NodeKind::Inline, param)?;
                    builder.classes(inline, &["default_value"])?;
                    builder.add_text(default, inline)?;
                }
            }
        }
    }
    Ok(())
}

fn needs_arglist(objtype: &str) -> bool {
    matches!(objtype, "function" | "method" | "classmethod" | "staticmethod")
}

/// Builds the full `desc_signature` subtree under a skeleton `desc` node.
pub fn build_desc_signature(
    doc: &mut Document,
    desc_id: NodeId,
    signature: &str,
    objtype: &str,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<SignatureInfo> {
    let parsed = parse_sig_re(signature).ok_or_else(|| DomainError::InvalidData {
        domain: "py".to_owned(),
        detail: format!("could not parse signature {signature:?}"),
    })?;
    let span = doc.get(desc_id).ok().and_then(|node| node.span).unwrap_or(Span::new(0, 0));
    let placeholder = doc.get(desc_id).ok().and_then(|node| {
        node.children.iter().copied().find(|&child| {
            doc.get(child).is_ok_and(|child| child.kind == NodeKind::DescSignature)
        })
    });
    let mut builder = Builder { doc, span };

    let modname = options.module.clone().or_else(|| context.module.clone());
    let classname_in = context.class.clone();
    let (classname, fullname, displayed_prefix) = if let Some(class) = &classname_in {
        if parsed.prefix.is_empty() || parsed.prefix.starts_with(&format!("{class}.")) || parsed.prefix == format!("{class}.") {
            let prefix = if parsed.prefix.is_empty() {
                String::new()
            } else {
                parsed.prefix[class.len()..].trim_start_matches('.').to_owned()
            };
            (class.clone(), format!("{class}.{}", parsed.name), prefix)
        } else if !parsed.prefix.is_empty() {
            (class.clone(), format!("{class}.{}{}", parsed.prefix, parsed.name), parsed.prefix.clone())
        } else {
            (class.clone(), format!("{class}.{}", parsed.name), String::new())
        }
    } else if !parsed.prefix.is_empty() {
        let class = parsed.prefix.trim_end_matches('.').to_owned();
        (class, format!("{}{}", parsed.prefix, parsed.name), parsed.prefix.clone())
    } else {
        (String::new(), parsed.name.clone(), String::new())
    };

    let anchor = py_make_id(&match &modname {
        Some(module) if !module.is_empty() => format!("{module}.{fullname}"),
        _ => fullname.clone(),
    });

    let signode = match placeholder {
        Some(id) => id,
        None => builder.add(NodeKind::DescSignature, desc_id)?,
    };
    {
        let node = builder.doc.get_mut(signode)?;
        node.children.clear();
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "py".to_owned()]);
        node.attrs.set_str("xml:space", "preserve");
        if !options.no_index {
            node.attrs.set_list("ids", vec![anchor.clone()]);
        }
        node.attrs.set_str("fullname", &fullname);
        node.attrs.set_str("class", &classname);
        node.attrs.set_str("module", modname.as_deref().unwrap_or(""));
    }

    // Decorator directives prepend an `@` to the signature.
    if matches!(objtype, "decorator" | "decoratormethod") {
        let add = builder.add(NodeKind::DescAddname, signode)?;
        builder.add_text("@", add)?;
    }

    // Signature keyword prefixes. Order follows the upstream directive classes.
    let mut keywords: Vec<&str> = Vec::new();
    match objtype {
        "class" => keywords.push("class"),
        "exception" => keywords.push("exception"),
        "type" => keywords.push("type"),
        "property" => {
            if options.abstract_ || options.abstractmethod {
                keywords.push("abstract");
            }
            if options.classmethod {
                keywords.push("class");
            }
            keywords.push("property");
        }
        _ => {}
    }
    if options.final_ {
        keywords.insert(0, "final");
    }
    if options.abstract_ && !matches!(objtype, "property") {
        keywords.insert(keywords.len().min(1), "abstract");
    }
    if objtype == "method" {
        if options.abstractmethod {
            keywords.push("abstractmethod");
        }
        if options.async_ {
            keywords.push("async");
        }
        if options.classmethod {
            keywords.push("classmethod");
        }
        if options.staticmethod {
            keywords.push("static");
        }
    } else if objtype == "function" && options.async_ {
        keywords.push("async");
    }
    prefix_keywords(&mut builder, signode, &keywords)?;

    // Qualifying addname.
    let add_module = classname_in.is_none();
    if !displayed_prefix.is_empty() {
        let addname = builder.sig(NodeKind::DescAddname, &displayed_prefix, "n", signode)?;
        builder.classes(addname, &["sig-prename", "descclassname"])?;
    } else if let Some(module) = &modname {
        if add_module && options.add_module_names {
            let text = format!("{module}.");
            let addname = builder.sig(NodeKind::DescAddname, &text, "n", signode)?;
            builder.classes(addname, &["sig-prename", "descclassname"])?;
        }
    }

    // Object name.
    let name_node = builder.sig(NodeKind::DescName, &parsed.name, "n", signode)?;
    builder.classes(name_node, &["sig-name", "descname"])?;

    if !parsed.type_params.is_empty() {
        build_type_params(&mut builder, signode, &parsed.type_params, context, options)?;
    }
    if !parsed.arglist.is_empty() || needs_arglist(objtype) {
        build_parameterlist(&mut builder, signode, &parsed.arglist, context, options)?;
    }
    if !parsed.retann.is_empty() {
        let returns = builder.add(NodeKind::DescReturns, signode)?;
        emit_annotation(&mut builder, &parse_annotation(&parsed.retann), returns, context, options)?;
    }
    if objtype == "type" {
        if let Some(canonical) = &options.canonical {
            let node = builder.add(NodeKind::DescAnnotation, signode)?;
            builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
            builder.sig(NodeKind::DescSigPunctuation, "=", "p", node)?;
            builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
            emit_annotation(&mut builder, &parse_annotation(canonical), node, context, options)?;
        }
    }
    if let Some(annotation) = &options.annotation {
        let node = builder.add(NodeKind::DescAnnotation, signode)?;
        builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
        builder.add_text(annotation, node)?;
    }
    for (value, separator) in [(&options.r#type, ':'), (&options.value, '=')] {
        let Some(value) = value else { continue };
        let node = builder.add(NodeKind::DescAnnotation, signode)?;
        // Upstream `PyVariable`/`PyAttribute`: the `:type:` annotation starts
        // directly with ':', while the `:value:` annotation starts with a space.
        if separator == '=' {
            builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
        }
        builder.sig(NodeKind::DescSigPunctuation, &separator.to_string(), "p", node)?;
        builder.sig(NodeKind::DescSigSpace, " ", "w", node)?;
        if separator == ':' {
            emit_annotation(&mut builder, &parse_annotation(value), node, context, options)?;
        } else {
            builder.add_text(value, node)?;
        }
    }

    // Provenance used by `PythonDomain::process_doc`.
    {
        let node = builder.doc.get_mut(desc_id)?;
        node.attrs.set_str("py:fullname", &fullname);
        node.attrs.set_str("py:class", &classname);
        node.attrs.set_str("py:module", modname.as_deref().unwrap_or(""));
        node.attrs.set_str("py:anchor", &anchor);
        if let Some(canonical) = &options.canonical {
            node.attrs.set_str("py:canonical", canonical);
        }
        node.attrs.set_str("domain", "py");
        node.attrs.set_str("desctype", objtype);
        node.attrs.set_list("classes", vec!["py".to_owned(), objtype.to_owned()]);
        for (key, value) in [
            ("no-index", options.no_index),
            ("noindex", options.no_index),
            ("no-index-entry", options.no_index_entry),
            ("noindexentry", options.no_index_entry),
            ("no-contents-entry", options.no_contents_entry),
            ("nocontentsentry", options.no_contents_entry),
            ("no-typesetting", options.no_typesetting),
        ] {
            node.attrs.set_bool(key, value);
        }
    }

    let _ = sig_class(NodeKind::DescName);
    Ok(SignatureInfo { fullname, classname, module: modname, anchor })
}

// ----- typed field lists ----------------------------------------------------

fn field_name_text(doc: &Document, field: NodeId) -> String {
    let Ok(node) = doc.get(field) else {
        return String::new();
    };
    for &child in &node.children {
        if let Ok(name) = doc.get(child) {
            if name.kind == NodeKind::FieldName {
                return name.astext(doc).trim().to_owned();
            }
        }
    }
    String::new()
}

fn field_body(doc: &Document, field: NodeId) -> Option<NodeId> {
    let node = doc.get(field).ok()?;
    node.children.iter().copied().find(|&child| {
        doc.get(child).is_ok_and(|child| child.kind == NodeKind::FieldBody)
    })
}

/// Upstream keeps an existing inline role reference (e.g. `:type:` containing
/// `:doc:\`index\``) as the typed field's content instead of reparsing its text.
fn contains_xref(doc: &Document, root: NodeId) -> bool {
    let Ok(node) = doc.get(root) else {
        return false;
    };
    if node.kind == NodeKind::PendingXref || node.kind == NodeKind::Reference {
        return true;
    }
    node.children.iter().copied().any(|child| contains_xref(doc, child))
}

/// Rewrites `:param:` / `:type:` / `:rtype:` / `:raises:` bodies with xref
/// annotations (upstream's `PyTypedField`, `PyGroupedField`, `PyField`).
pub fn rewrite_content_fields(
    doc: &mut Document,
    content: NodeId,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<()> {
    let fieldlists: Vec<NodeId> = {
        let Ok(node) = doc.get(content) else { return Ok(()) };
        node.children
            .iter()
            .copied()
            .filter(|&child| doc.get(child).is_ok_and(|child| child.kind == NodeKind::FieldList))
            .collect()
    };
    for fieldlist in fieldlists {
        rewrite_fieldlist(doc, fieldlist, context, options)?;
    }
    Ok(())
}

fn rewrite_fieldlist(
    doc: &mut Document,
    fieldlist: NodeId,
    context: &SigContext,
    options: &PyOptions,
) -> DomainResult<()> {
    let fields: Vec<NodeId> = {
        let Ok(node) = doc.get(fieldlist) else { return Ok(()) };
        node.children.to_vec()
    };
    let mut param_names: Vec<(String, String)> = Vec::new(); // (name, description)
    let mut type_map: BTreeMap<String, String> = BTreeMap::new();
    let mut other_fields: Vec<(String, String, NodeId)> = Vec::new(); // (name, arg, field)

    for field in fields {
        let Ok(node) = doc.get(field) else { continue };
        if node.kind != NodeKind::Field {
            continue;
        }
        let label = field_name_text(doc, field);
        let (head, arg) = match label.split_once(' ') {
            Some((head, arg)) => (head.to_lowercase(), arg.trim().to_owned()),
            None => (label.to_lowercase(), String::new()),
        };
        match head.as_str() {
            "param" | "parameter" | "arg" | "argument" | "keyword" | "kwarg" | "kwparam" => {
                let description = field_body(doc, field)
                    .and_then(|body| doc.get(body).ok().map(|node| node.astext(doc).trim().to_owned()))
                    .unwrap_or_default();
                param_names.push((arg, description));
                let _ = doc.remove_node(field);
            }
            "type" | "paramtype" | "vartype" => {
                let annotation = field_body(doc, field)
                    .and_then(|body| doc.get(body).ok().map(|node| node.astext(doc).trim().to_owned()))
                    .unwrap_or_default();
                if arg.is_empty() {
                    // A bare `:type:` is rendered as its own "Type" field.
                    if let Some(name_id) = doc.get(field).ok().and_then(|f| f.children.first().copied()) {
                        if let Ok(name) = doc.get_mut(name_id) {
                            name.children.clear();
                        }
                        let _ = doc.add_node(Node::text("Type"), Some(name_id));
                    }
                    type_map.insert(String::new(), annotation.clone());
                    other_fields.push(("Type".to_owned(), String::new(), field));
                } else {
                    type_map.insert(arg, annotation);
                    let _ = doc.remove_node(field);
                }
            }
            "var" | "ivar" | "cvar" => {
                param_names.push((arg, field_body(doc, field)
                    .and_then(|body| doc.get(body).ok().map(|node| node.astext(doc).trim().to_owned()))
                    .unwrap_or_default()));
                let _ = doc.remove_node(field);
            }
            _ => other_fields.push((label, arg, field)),
        }
    }

    // Rebuild the field list in upstream order: Parameters, Variables, Raises,
    // Returns, Return type, then everything else preserved verbatim.
    let mut rebuilt: Vec<NodeId> = Vec::new();
    let span = doc.get(fieldlist).ok().and_then(|node| node.span).unwrap_or(Span::new(0, 0));
    let mut builder = Builder { doc, span };

    if !param_names.is_empty() {
        let field = builder.add(NodeKind::Field, fieldlist)?;
        let name = builder.add(NodeKind::FieldName, field)?;
        builder.add_text("Parameters", name)?;
        let body = builder.add(NodeKind::FieldBody, field)?;
        let bullets = builder.add(NodeKind::BulletList, body)?;
        for (param, description) in &param_names {
            let item = builder.add(NodeKind::ListItem, bullets)?;
            let para = builder.add(NodeKind::Paragraph, item)?;
            let strong = builder.add(NodeKind::LiteralStrong, para)?;
            builder.classes(strong, &[])?;
            builder.add_text(param, strong)?;
            if let Some(annotation) = type_map.get(param) {
                builder.add_text(" (", para)?;
                let emphasis = builder.add(NodeKind::LiteralEmphasis, para)?;
                emit_annotation(&mut builder, &parse_annotation(annotation), emphasis, context, options)?;
                builder.add_text(")", para)?;
            }
            if !description.is_empty() {
                builder.add_text(" – ", para)?;
                builder.add_text(description, para)?;
            }
        }
        rebuilt.push(field);
    }

    for (label, arg, field) in other_fields {
        let lowered = label.to_lowercase();
        if lowered == "type" {
            if field_body(builder.doc, field).is_some_and(|body| contains_xref(builder.doc, body)) {
                rebuilt.push(field);
                continue;
            }
            if let Some(body) = field_body(builder.doc, field) {
                let annotation = type_map.get("").cloned().unwrap_or_default();
                if let Ok(node) = builder.doc.get_mut(body) {
                    node.children.clear();
                }
                let paragraph = builder.add(NodeKind::Paragraph, body)?;
                emit_annotation(&mut builder, &parse_annotation(&annotation), paragraph, context, options)?;
            }
        } else if lowered == "rtype" {
            if field_body(builder.doc, field).is_some_and(|body| contains_xref(builder.doc, body)) {
                rebuilt.push(field);
                continue;
            }
            if let Some(body) = field_body(builder.doc, field) {
                let annotation = builder.doc.get(body).ok().map(|node| node.astext(builder.doc).trim().to_owned()).unwrap_or_default();
                if let Ok(node) = builder.doc.get_mut(body) {
                    node.children.clear();
                }
                if let Some(name_id) = builder.doc.get(field).ok().and_then(|f| f.children.first().copied()) {
                    if let Ok(name) = builder.doc.get_mut(name_id) {
                        name.children.clear();
                    }
                    let _ = builder.doc.add_node(Node::text("Return type"), Some(name_id));
                }
                let paragraph = builder.add(NodeKind::Paragraph, body)?;
                emit_annotation(&mut builder, &parse_annotation(&annotation), paragraph, context, options)?;
            }
        } else if lowered == "raises" || lowered == "raise" || lowered == "exception" || lowered == "except" {
            if field_body(builder.doc, field).is_some_and(|body| contains_xref(builder.doc, body)) {
                rebuilt.push(field);
                continue;
            }
            if let Some(body) = field_body(builder.doc, field) {
                let text = builder.doc.get(body).ok().map(|node| node.astext(builder.doc).trim().to_owned()).unwrap_or(arg);
                if let Ok(node) = builder.doc.get_mut(body) {
                    node.children.clear();
                }
                let paragraph = builder.add(NodeKind::Paragraph, body)?;
                emit_annotation(&mut builder, &parse_annotation(&text), paragraph, context, options)?;
            }
        }
        rebuilt.push(field);
    }

    let list = builder.doc.get_mut(fieldlist)?;
    list.children = rebuilt;
    Ok(())
}

// ----- whole-document preparation -------------------------------------------

/// Renders one upstream `index` entry tuple as a string (the frozen AST has no
/// heterogeneous tuple attribute, so the pseudo-XML surface is a JSON-ish list).
fn index_entry(objtype: &str, info: &SignatureInfo, options: &PyOptions) -> String {
    if options.no_index_entry {
        return "[]".to_owned();
    }
    let module = info.module.as_deref().unwrap_or("");
    let key = if module.is_empty() {
        info.fullname.clone()
    } else {
        format!("{module}.{}", info.fullname)
    };
    let label = match objtype {
        "function" => {
            if module.is_empty() {
                format!("built-in function; {}()", info.fullname)
            } else {
                format!("{}() (in module {module})", info.fullname)
            }
        }
        "class" => {
            if module.is_empty() {
                format!("{} (built-in class)", info.fullname)
            } else {
                format!("{} (class in {module})", info.fullname)
            }
        }
        "method" | "classmethod" | "staticmethod" => {
            let (class, method) = info.fullname.rsplit_once('.').unwrap_or(("", &info.fullname));
            format!("{method}() ({class} method)")
        }
        "module" => format!("module; {}", info.fullname),
        _ => format!("{} (in module {module})", info.fullname),
    };
    format!("['single', '{label}', '{key}', '', None]")
}

fn is_skeleton(doc: &Document, id: NodeId) -> Option<String> {
    let node = doc.get(id).ok()?;
    if node.kind != NodeKind::Desc {
        return None;
    }
    let objtype = node.get_str("objtype").ok().flatten()?.to_owned();
    node.get_str("py:signature").ok().flatten()?;
    Some(objtype)
}

fn read_options(node: &Node) -> PyOptions {
    PyOptions::from_node(node)
}

/// Expands every skeleton `py:` description in `document`, builds its
/// signature nodes, rewrites typed fields and annotates all pending `py`
/// cross-references with their read-time module/class context.
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    let root = document.root;
    let mut context = SigContext::default();
    prepare_children(document, root, &mut context)
}

fn prepare_children(
    document: &mut Document,
    parent: NodeId,
    context: &mut SigContext,
) -> DomainResult<()> {
    let children = document.children(parent).map(<[NodeId]>::to_vec).unwrap_or_default();
    for child in children {
        if document.get(child).is_err() {
            // The node was removed (e.g. a `currentmodule` marker) earlier.
            continue;
        }
        // `py:module` / `py:currentmodule` emit target markers instead of desc
        // nodes, matching upstream's `PyModule` / `PyCurrentModule` output.
        let marker_kind = document.get(child).ok().and_then(|node| {
            if node.kind != NodeKind::Target || node.get_str("py:signature").ok().flatten().is_none() {
                return None;
            }
            node.get_str("objtype").ok().flatten().map(str::to_owned)
        });
        if let Some(marker) = marker_kind {
            let name = document.get(child)
                .ok()
                .and_then(|node| node.get_str("py:signature").ok().flatten())
                .unwrap_or("")
                .to_owned();
            if marker == "currentmodule" {
                context.module = if name == "None" || name.is_empty() { None } else { Some(name) };
                let _ = document.remove_node(child);
                continue;
            }
            if marker == "module" {
                let anchor = py_make_id(&format!("module-{name}"));
                if let Ok(node) = document.get_mut(child) {
                    node.attrs.set_str("py:fullname", &name);
                    node.attrs.set_str("py:module", &name);
                    node.attrs.set_str("py:anchor", &anchor);
                    node.attrs.set_str("domain", "py");
                    node.attrs.set_list("ids", vec![anchor.clone()]);
                }
                context.module = Some(name);
                annotate_xref(document, child, context)?;
                continue;
            }
        }
        let Some(objtype) = is_skeleton(document, child) else {
            annotate_xref(document, child, context)?;
            // Recurse into ordinary containers.
            let _ = prepare_children(document, child, context);
            continue;
        };
        let (signature, options, synopsis, platform, deprecated) = {
            let node = document.get(child)?;
            (
                node.get_str("py:signature").ok().flatten().unwrap_or("").to_owned(),
                read_options(node),
                node.get_str("py:synopsis").ok().flatten().unwrap_or("").to_owned(),
                node.get_str("py:platform").ok().flatten().unwrap_or("").to_owned(),
                node.attrs.get_bool("py:deprecated").ok().flatten().unwrap_or(false),
            )
        };

        if objtype == "currentmodule" {
            context.module = if signature == "None" || signature.is_empty() {
                None
            } else {
                Some(signature.trim().to_owned())
            };
            let _ = document.remove_node(child);
            continue;
        }

        let outer = context.clone();
        let signature_info = build_desc_signature(document, child, &signature, &objtype, &outer, &options)?;
        // Populate the sibling `index` node (upstream `add_target_and_index`).
        let index_id = document.get(child).ok().and_then(|node| node.attrs.get_node_id("py:index_id").ok().flatten());
        if let Some(index_id) = index_id {
            let entry = index_entry(&objtype, &signature_info, &options);
            if let Ok(node) = document.get_mut(index_id) {
                node.attrs.set_str("entries", &entry);
            }
        }
        if objtype == "module" {
            context.module = signature_info.module.clone().filter(|module| !module.is_empty());
        }
        let inner = SigContext {
            module: options.module.clone().or_else(|| context.module.clone()),
            class: if matches!(objtype.as_str(), "class" | "exception") && !signature_info.fullname.is_empty() {
                Some(signature_info.fullname.clone())
            } else if !signature_info.classname.is_empty() && !outer.class.is_some() {
                Some(signature_info.classname.clone())
            } else {
                outer.class.clone()
            },
        };

        // Annotate the signature's own xrefs with the outer class context
        // (upstream sets py:class in `before_content`, i.e. after the signature).
        let signature_children: Vec<NodeId> = document.get(child).ok().and_then(|node| {
            node.children.iter().copied().find(|&id| document.get(id).is_ok_and(|n| n.kind == NodeKind::DescSignature))
        }).and_then(|id| document.children(id).map(<[NodeId]>::to_vec).ok()).unwrap_or_default();
        for id in signature_children {
            if document.get(id).is_ok_and(|node| node.kind == NodeKind::PendingXref) {
                let node = document.get_mut(id)?;
                if let Some(module) = &inner.module {
                    node.attrs.set_str("py:module", module);
                }
                if let Some(class) = &outer.class {
                    node.attrs.set_str("py:class", class);
                } else {
                    node.attrs.remove("py:class");
                }
            }
        }

        // Rewrite typed field bodies using the inner (content) context.
        let content_id = document.get(child).ok().and_then(|node| {
            node.children.iter().copied().find(|&id| document.get(id).is_ok_and(|n| n.kind == NodeKind::DescContent))
        });
        if let Some(content) = content_id {
            rewrite_content_fields(document, content, &inner, &options)?;
        }

        // Store module-index metadata on the desc node for `process_doc`.
        if objtype == "module" {
            if let Ok(node) = document.get_mut(child) {
                node.attrs.set_str("py:synopsis", &synopsis);
                node.attrs.set_str("py:platform", &platform);
                node.attrs.set_bool("py:deprecated", deprecated);
            }
        }

        let nested = document.get(child).map(|node| node.children.clone()).unwrap_or_default();
        for grandchild in nested {
            if document.get(grandchild).is_ok_and(|node| node.kind == NodeKind::DescSignature) {
                let saved = context.clone();
                *context = SigContext { module: inner.module.clone(), class: outer.class.clone() };
                let _ = prepare_children(document, grandchild, context);
                *context = saved;
            } else {
                let saved = context.clone();
                *context = inner.clone();
                let _ = prepare_children(document, grandchild, context);
                *context = saved;
            }
        }
        *context = if matches!(objtype.as_str(), "class" | "exception") {
            outer
        } else {
            SigContext { module: context.module.clone(), class: outer.class }
        };
    }
    Ok(())
}

fn annotate_xref(document: &mut Document, id: NodeId, context: &SigContext) -> DomainResult<()> {
    let is_py_xref = document.get(id).is_ok_and(|node| {
        node.kind == NodeKind::PendingXref
            && node.pending_xref().and_then(|xref| xref.refdomain()).is_some_and(|domain| domain == "py")
    });
    if !is_py_xref {
        return Ok(());
    }
    let node = document.get_mut(id)?;
    if let Some(module) = &context.module {
        node.attrs.set_str("py:module", module);
    }
    if let Some(class) = &context.class {
        node.attrs.set_str("py:class", class);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sig_re_forms() {
        let parsed = parse_sig_re("foo(a, b=1) -> int").expect("simple signature");
        assert_eq!(parsed.name, "foo");
        assert_eq!(parsed.arglist, "a, b=1");
        assert_eq!(parsed.retann, "int");

        let parsed = parse_sig_re("mod.Class.method[T](self, x: list[str], /, *, y=2) -> X | Y").expect("complex");
        assert_eq!(parsed.prefix, "mod.Class.");
        assert_eq!(parsed.name, "method");
        assert_eq!(parsed.type_params, "T");
        assert!(parsed.arglist.contains('/'));
        assert_eq!(parsed.retann, "X | Y");

        assert!(parse_sig_re("").is_none());
        assert!(parse_sig_re("foo(").is_none());
        assert!(parse_sig_re("foo() trailing").is_none());
    }

    #[test]
    fn reftarget_modifiers() {
        assert_eq!(parse_reftarget("~a.b.C", false), ("class", "a.b.C".to_owned(), "C".to_owned(), false));
        assert_eq!(parse_reftarget(".C", false), ("class", "C".to_owned(), "C".to_owned(), true));
        assert_eq!(parse_reftarget("None", false), ("obj", "None".to_owned(), "None".to_owned(), false));
        assert_eq!(parse_reftarget("typing.Any", false), ("obj", "typing.Any".to_owned(), "Any".to_owned(), false));
    }

    #[test]
    fn annotation_optional_and_union() {
        let tokens = parse_annotation("Optional[int]");
        let rendered: String = tokens.iter().map(|token| match token {
            AnnToken::Space => " ".to_owned(),
            AnnToken::Punct(text) | AnnToken::Number(text) | AnnToken::Keyword(text) | AnnToken::Str(text) | AnnToken::Operator(text) | AnnToken::Name(text) | AnnToken::ShortName(text) | AnnToken::SpecificName(text) => text.clone(),
        }).collect();
        assert_eq!(rendered, "int | None");
        let tokens = parse_annotation("Union[int, str]");
        let rendered: String = tokens.iter().map(|token| match token {
            AnnToken::Space => " ".to_owned(),
            AnnToken::Punct(text) | AnnToken::Number(text) | AnnToken::Keyword(text) | AnnToken::Str(text) | AnnToken::Operator(text) | AnnToken::Name(text) | AnnToken::ShortName(text) | AnnToken::SpecificName(text) => text.clone(),
        }).collect();
        assert_eq!(rendered, "int | str");
    }

    #[test]
    fn arglist_separators_and_vars() {
        let tokens = tokenize_arglist("a, /, b: int = 1, *args, c, **kwargs");
        assert_eq!(tokens.len(), 6);
        assert_eq!(tokens[1], ParamToken::PositionalOnlySeparator);
        assert!(matches!(tokens[3], ParamToken::Param { var: 1, .. }));
        assert!(matches!(tokens[5], ParamToken::Param { var: 2, .. }));
        let keyword_only = tokenize_arglist("a, *, b");
        assert_eq!(keyword_only[1], ParamToken::KeywordOnlySeparator);
    }
}
