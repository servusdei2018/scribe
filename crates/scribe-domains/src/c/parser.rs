//! Hand-written C declarator parser (Phase 13).
//!
//! Mirrors the subset of `sphinx/domains/c/_parser.py` / `_ast.py` needed by the
//! declaration and xref forms in the upstream fixture suites: primitive and
//! named types, cv-qualifiers, pointers, arrays, function parameter lists,
//! anonymous fields (`@data`) and dotted names (`Sphinx.version`).  Malformed
//! declarations degrade to a best-effort parse and never panic.

use crate::sigbuild::SigToken;

/// Token kind produced by the linear tokenizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    /// Identifier (possibly starting with `@`).
    Word,
    /// Numeric literal.
    Number,
    /// String / character literal.
    Literal,
    /// Multi-character or single punctuation.
    Punct,
}

/// One tokenizer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tok {
    /// Kind.
    pub kind: TokKind,
    /// Source text.
    pub text: String,
}

/// Linear, allocation-light tokenizer for a C declaration.
pub fn tokenize(src: &str) -> Vec<Tok> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() || (ch == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_' | b'x' | b'X')) {
                i += 1;
            }
            out.push(Tok { kind: TokKind::Number, text: src[start..i].to_owned() });
            continue;
        }
        if ch == b'\'' || ch == b'"' {
            let start = i;
            let quote = ch;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(Tok { kind: TokKind::Literal, text: src[start..i].to_owned() });
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == b'_' || ch == b'@' || ch >= 0x80 {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'@') || bytes[i] >= 0x80) {
                i += 1;
            }
            out.push(Tok { kind: TokKind::Word, text: src[start..i].to_owned() });
            continue;
        }
        // Punctuation (longest match first).
        let rest = &src[i..];
        let punct = ["...", "->", "::", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "++", "--", "()", ""]
            .iter()
            .find(|candidate| !candidate.is_empty() && rest.starts_with(**candidate))
            .copied();
        if let Some(punct) = punct {
            i += punct.len();
            out.push(Tok { kind: TokKind::Punct, text: punct.to_owned() });
        } else {
            // Punctuation is always ASCII here (non-ASCII bytes are words).
            let text = src[i..i + 1].to_owned();
            i += 1;
            out.push(Tok { kind: TokKind::Punct, text });
        }
    }
    out
}

/// One segment of a nested declarator name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameSegment {
    /// Segment text (may be an anonymous field like `@data`).
    pub text: String,
    /// Separator emitted *after* this segment inside `desc_addname`.
    pub sep_after: String,
}

/// A parsed declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decl {
    /// Leading type-specifier tokens (including leading pointers/qualifiers).
    pub prefix: Vec<SigToken>,
    /// Nested name segments; the last one is the declared name.
    pub segments: Vec<NameSegment>,
    /// Function/macro parameter groups (each a token sequence).
    pub params: Vec<Vec<SigToken>>,
    /// Tokens emitted after the name (array extents, trailing pointers, ...).
    pub suffix: Vec<SigToken>,
    /// Dotted full name of the declared object.
    pub name: String,
    /// True when a parameter list was present.
    pub is_function: bool,
}

impl Decl {
    /// Final (unqualified) name.
    pub fn short_name(&self) -> &str {
        self.segments.last().map_or(self.name.as_str(), |segment| segment.text.as_str())
    }
}

/// Primitive C type keywords (rendered as `desc_sig_keyword_type`).
///
/// These mirror upstream's `sphinx.domains.c._ids._keywords` (C language
/// keywords) plus the `c_extra_keywords` (`_macro_keywords`) that Sphinx treats
/// as type keywords. Standard-library *typedefs* such as `size_t` are **not**
/// keywords: upstream parses them as named types and emits a `c:identifier`
/// pending xref (see `ASTIdentifier.describe_signature(mode='markType')`), so
/// they must not appear here.
pub const PRIMITIVE_TYPES: &[&str] = &[
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "_Bool",
    "bool",
];

/// Declaration specifiers rendered as `desc_sig_keyword`.
pub const DECL_KEYWORDS: &[&str] = &[
    "const", "volatile", "restrict", "static", "extern", "register", "auto", "inline", "struct",
    "union", "enum", "typedef", "public", "protected", "private", "virtual", "explicit",
    "constexpr", "friend", "mutable",
];

fn is_word(toks: &[Tok], index: usize, words: &[&str]) -> bool {
    toks.get(index).is_some_and(|token| token.kind == TokKind::Word && words.contains(&token.text.as_str()))
}

/// Parses one declaration for the given object type.
///
/// The parser is deliberately tolerant: unrecognized punctuation becomes a
/// punctuation token inside the appropriate token stream rather than an error.
pub fn parse_declaration(signature: &str, objtype: &str, domain: &str) -> Decl {
    let mut src = signature.trim();
    // Trailing semicolon (C++ declarations) and inline default value.
    while src.ends_with(';') {
        src = src[..src.len() - 1].trim_end();
    }
    let toks = tokenize(src);
    if toks.is_empty() {
        return Decl {
            prefix: Vec::new(), segments: Vec::new(), params: Vec::new(), suffix: Vec::new(),
            name: String::new(), is_function: false,
        };
    }
    parse_tokens(&toks, objtype, domain)
}

/// Parses an xref target (`parse_xref_object` in upstream).  Keeps pointers,
/// arrays, template arguments and `::`/`.`/`->` qualified names verbatim.
pub fn parse_xref(signature: &str, domain: &str) -> String {
    let toks = tokenize(signature.trim());
    let mut out = String::new();
    for (index, token) in toks.iter().enumerate() {
        match token.text.as_str() {
            "(" | "[" | "<" if index > 0 && !out.ends_with(' ') => out.push(' '),
            "," | ")" | "]" | ">" | "::" | "." | "->" | "*" | "&" => {}
            _ if !out.is_empty() && needs_space(&out, &token.text) => out.push(' '),
            _ => {}
        }
        out.push_str(&token.text);
    }
    let _ = domain;
    out.trim().to_owned()
}

fn needs_space(prev: &str, next: &str) -> bool {
    let prev_word = prev.chars().last().is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '@');
    let next_word = next.chars().next().is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '@');
    prev_word && next_word
}

fn parse_tokens(toks: &[Tok], objtype: &str, domain: &str) -> Decl {
    // Find the declarator name: the last top-level word that is not a type
    // keyword or a leading declaration qualifier.
    let mut depth = 0i32;
    let mut candidate: Option<usize> = None;
    for (index, token) in toks.iter().enumerate() {
        match token.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            "<" => depth += 1,
            ">" | ">>" => depth -= 1,
            _ => {}
        }
        if depth != 0 || token.kind != TokKind::Word {
            continue;
        }
        if is_word(toks, index, PRIMITIVE_TYPES) || is_word(toks, index, DECL_KEYWORDS) {
            continue;
        }
        // `operator` is part of a longer name (operator+, operator(), ...).
        candidate = Some(index);
    }
    let name_index = candidate.unwrap_or_else(|| {
        // Function pointer declarators `(*cb)(...)`: the name sits inside
        // parentheses right after a pointer marker.
        (1..toks.len())
            .find(|&index| toks[index].kind == TokKind::Word && toks[index - 1].text == "*")
            .or_else(|| toks.iter().position(|token| token.kind == TokKind::Word))
            .unwrap_or(0)
    });

    // Extend the name backwards over `.` / `->` / `::` chains.
    let mut start = name_index;
    loop {
        if start < 2 {
            break;
        }
        let sep = &toks[start - 1];
        if sep.kind != TokKind::Punct || !matches!(sep.text.as_str(), "." | "->" | "::") {
            break;
        }
        let prev = &toks[start - 2];
        if prev.kind != TokKind::Word {
            break;
        }
        start -= 2;
    }
    let mut end = name_index + 1;
    // `operator` names: consume the following operator token(s).
    if toks[name_index].text == "operator" {
        if let Some(next) = toks.get(end) {
            if next.kind == TokKind::Punct && next.text != "(" {
                end += 1;
            } else if next.text == "(" && toks.get(end + 1).is_some_and(|t| t.text == ")") {
                end += 2;
            }
        }
        if toks.get(end).is_some_and(|t| t.kind == TokKind::Literal) {
            end += 1; // literal operator `operator""_x`
        }
    }

    // Build name segments, each with the separator that precedes it (stored on
    // the previous segment as `sep_after`).
    let mut segments = Vec::new();
    let mut segment_indices = Vec::new();
    let mut index = start;
    while index < end {
        if toks[index].kind == TokKind::Word {
            segment_indices.push(index);
        }
        index += 1;
    }
    for (position, &word_index) in segment_indices.iter().enumerate() {
        let sep_after = if position + 1 < segment_indices.len() {
            let next = segment_indices[position + 1];
            toks[next - 1].text.clone()
        } else {
            String::new()
        };
        if toks[word_index].text == "operator" && end > word_index + 1 {
            // Combine operator spelling into one segment.
            let mut text = "operator".to_owned();
            for token in &toks[word_index + 1..end] {
                text.push_str(&token.text);
            }
            segments.push(NameSegment { text, sep_after });
            break;
        }
        segments.push(NameSegment { text: toks[word_index].text.clone(), sep_after });
    }
    if segments.is_empty() {
        segments.push(NameSegment { text: String::new(), sep_after: String::new() });
    }

    // Prefix tokens (everything before the name chain).
    let prefix_toks = &toks[..start];
    let mut prefix = type_tokens(prefix_toks, domain);

    // Suffix tokens: everything after the name, split into parameter groups and
    // trailing punctuation.
    let rest = &toks[end..];
    let (params, suffix) = parse_suffix(rest, domain);
    let is_function = !params.is_empty() && rest.first().is_some_and(|token| token.text == "(");
    if objtype == "macro" && !is_function {
        // A macro with a parameter list was already captured by `parse_suffix`;
        // an object-like macro has none.
    }

    let mut name = String::new();
    for segment in &segments {
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&segment.text);
    }

    if objtype == "type" && !prefix.is_empty() {
        // `int SphinxVersionNum` is a typedef with an explicit base type.
        let mut with_kw = vec![SigToken::Keyword("typedef".to_owned()), SigToken::Space];
        with_kw.append(&mut prefix);
        prefix = with_kw;
    } else if objtype == "type" {
        prefix.push(SigToken::Keyword("type".to_owned()));
        prefix.push(SigToken::Space);
    } else if prefix.is_empty() && objtype == "struct" {
        prefix.push(SigToken::Keyword("struct".to_owned()));
        prefix.push(SigToken::Space);
    } else if prefix.is_empty() && objtype == "union" {
        prefix.push(SigToken::Keyword("union".to_owned()));
        prefix.push(SigToken::Space);
    } else if prefix.is_empty() && objtype == "enum" {
        prefix.push(SigToken::Keyword("enum".to_owned()));
        prefix.push(SigToken::Space);
    } else if prefix.is_empty() && objtype == "enumerator" {
        prefix.push(SigToken::Keyword("enumerator".to_owned()));
        prefix.push(SigToken::Space);
    }

    Decl { prefix, segments, params, suffix, name, is_function }
}

/// Converts declaration-specifier tokens into signature tokens.
fn type_tokens(toks: &[Tok], domain: &str) -> Vec<SigToken> {
    let mut out = Vec::new();
    for (index, token) in toks.iter().enumerate() {
        match token.kind {
            TokKind::Number => out.push(SigToken::Number(token.text.clone())),
            TokKind::Literal => out.push(SigToken::Literal(token.text.clone())),
            TokKind::Word if PRIMITIVE_TYPES.contains(&token.text.as_str()) => {
                out.push(SigToken::KeywordType(token.text.clone()));
            }
            TokKind::Word if DECL_KEYWORDS.contains(&token.text.as_str()) => {
                out.push(SigToken::Keyword(token.text.clone()));
            }
            TokKind::Word => {
                // A named type; upstream renders it via
                // `ASTIdentifier.describe_signature(mode='markType')` as a
                // `pending_xref(reftype='identifier')` wrapping a
                // `desc_sig_name`. Standard-library typedefs (`size_t`, ...)
                // therefore produce `c:identifier` missing-reference warnings.
                let prev_is_sep = index > 0 && matches!(toks[index - 1].text.as_str(), "::" | "." | "->");
                if prev_is_sep {
                    out.push(SigToken::Punctuation(toks[index - 1].text.clone()));
                    out.push(SigToken::reference(domain, token.text.clone(), "identifier", "c-identifier"));
                } else {
                    out.push(SigToken::reference(domain, token.text.clone(), "identifier", "c-identifier"));
                }
            }
            TokKind::Punct => match token.text.as_str() {
                // Name separators are emitted by the following word token so a
                // qualified type renders as `Ref :: Ref`, never `Ref :: :: Ref`.
                "::" | "." | "->" => {}
                "*" | "&" | "(" | ")" | "[" | "]" | "," | "..." => out.push(SigToken::Punctuation(token.text.clone())),
                "+" | "-" | "==" | "!=" | "<<" | ">>" => out.push(SigToken::Operator(token.text.clone())),
                _ => out.push(SigToken::Punctuation(token.text.clone())),
            },
        }
        // A space after each word/pointer-token (except before punctuation).
        if index + 1 < toks.len() {
            let next = &toks[index + 1];
            let needs_space = matches!(token.kind, TokKind::Word)
                && (next.kind == TokKind::Word || next.text == "*" || next.text == "&");
            if needs_space {
                out.push(SigToken::Space);
            }
        }
    }
    out
}

/// Operators rendered as `desc_sig_operator` by upstream's C expression
/// renderer (`ASTOperator` / `ASTOpUnary`).
const EXPRESSION_OPERATORS: &[&str] = &[
    "->", "::", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=", "/=",
];

/// Parses the payload of a `c:expr` / `c:texpr` role into signature tokens.
///
/// Upstream calls `describe_signature(mode='markType')` on the parsed
/// expression, so every identifier becomes an `identifier` pending xref while
/// type keywords render as `desc_sig_keyword_type` and operators as
/// `desc_sig_operator`.  Whitespace between tokens becomes a `desc_sig_space`.
pub fn parse_expression(text: &str) -> Vec<SigToken> {
    let toks = tokenize(text);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut member_access = false;
    for tok in &toks {
        let is_member = member_access;
        member_access = false;
        while cursor < text.len() && text.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !out.is_empty() && cursor > 0
            && text.as_bytes().get(cursor.saturating_sub(1)).is_some_and(u8::is_ascii_whitespace)
        {
            out.push(SigToken::Space);
        }
        if text[cursor.min(text.len())..].starts_with(&tok.text) {
            cursor += tok.text.len();
        }
        let token = match tok.kind {
            TokKind::Word if PRIMITIVE_TYPES.contains(&tok.text.as_str()) => {
                SigToken::KeywordType(tok.text.clone())
            }
            TokKind::Word if DECL_KEYWORDS.contains(&tok.text.as_str()) => {
                SigToken::Keyword(tok.text.clone())
            }
            // Upstream `ASTPostfixMemberOfPointer` renders the member name in
            // `noneIsName` mode: a plain `desc_sig_name`, never an xref.
            TokKind::Word if is_member => SigToken::Name(tok.text.clone()),
            TokKind::Word => SigToken::reference("c", tok.text.clone(), "identifier", "c-identifier"),
            TokKind::Punct if EXPRESSION_OPERATORS.contains(&tok.text.as_str()) => {
                member_access = tok.text == "->";
                SigToken::Operator(tok.text.clone())
            }
            TokKind::Punct => {
                member_access = tok.text == ".";
                SigToken::Punctuation(tok.text.clone())
            }
            TokKind::Number => SigToken::Number(tok.text.clone()),
            TokKind::Literal => SigToken::Literal(tok.text.clone()),
        };
        out.push(token);
    }
    out
}

/// Converts a parameter's token list, rendering its final identifier as a plain
/// name (Sphinx marks parameters `n`, not `n` + type xref).
pub fn param_tokens(toks: &[Tok], domain: &str) -> Vec<SigToken> {
    let mut tokens = type_tokens(toks, domain);
    if let Some(last) = tokens.iter_mut().rev().find(|token| !matches!(token, SigToken::Space)) {
        if let SigToken::Ref { text, .. } = last {
            *last = SigToken::Name(text.clone());
        }
    }
    tokens
}

/// Returns the parameter names of a parsed declaration (for `functionParam`
/// objects and field xrefs).
pub fn parameter_names(decl: &Decl) -> Vec<String> {
    decl.params
        .iter()
        .filter_map(|param| {
            param.iter().rev().find_map(|token| match token {
                SigToken::Name(text) | SigToken::Ref { text, .. } => {
                    let name = text.trim_matches(['*', '&']);
                    (!name.is_empty()).then(|| name.to_owned())
                }
                _ => None,
            })
        })
        .collect()
}

/// Splits the tokens after the declarator name into parameter groups and a
/// trailing token stream.
fn parse_suffix(toks: &[Tok], domain: &str) -> (Vec<Vec<SigToken>>, Vec<SigToken>) {
    let mut params = Vec::new();
    let mut suffix = Vec::new();
    let mut index = 0usize;
    if toks.first().is_some_and(|token| token.text == "(") {
        let end = matching_close(toks, 0, "(", ")");
        if end > 0 {
            let inner = &toks[1..end];
            params = split_params(inner).into_iter().map(|param| param_tokens(&param, domain)).collect();
        }
        index = (end + 1).min(toks.len());
    }
    suffix.extend(type_tokens(&toks[index.min(toks.len())..], domain));
    (params, suffix)
}

/// Returns the index of the closing delimiter matching `open` at `start`.
fn matching_close(toks: &[Tok], start: usize, open: &str, close: &str) -> usize {
    let mut depth = 0i32;
    for (index, token) in toks.iter().enumerate().skip(start) {
        if token.text == open {
            depth += 1;
        } else if token.text == close {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    toks.len().saturating_sub(1)
}

/// Splits comma-separated parameter tokens at nesting depth zero.
fn split_params(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0i32;
    for token in toks {
        match token.text.as_str() {
            "(" | "[" | "<" => depth += 1,
            ")" | "]" | ">" => depth -= 1,
            "," if depth == 0 => {
                result.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(token.clone());
    }
    if !current.is_empty() || !result.is_empty() {
        result.push(current);
    }
    result.into_iter().filter(|param| !param.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_qualified_and_anonymous_names() {
        let toks = tokenize("const char *name");
        assert_eq!(toks.iter().map(|token| token.text.as_str()).collect::<Vec<_>>(), ["const", "char", "*", "name"]);
        let toks = tokenize("A.@data.a");
        assert_eq!(toks.iter().map(|token| token.text.as_str()).collect::<Vec<_>>(), ["A", ".", "@data", ".", "a"]);
        let toks = tokenize("void (*cb)(int)");
        assert_eq!(toks.iter().map(|token| token.text.as_str()).collect::<Vec<_>>(), ["void", "(", "*", "cb", ")", "(", "int", ")"]);
    }

    #[test]
    fn parses_simple_function() {
        let decl = parse_declaration("int hello(const char *name)", "function", "c");
        assert_eq!(decl.name, "hello");
        assert!(decl.is_function);
        assert_eq!(decl.params.len(), 1);
        assert_eq!(decl.segments.len(), 1);
        assert_eq!(decl.segments[0].text, "hello");
    }

    #[test]
    fn parses_nested_member() {
        let decl = parse_declaration("float Sphinx.version", "member", "c");
        assert_eq!(decl.name, "Sphinx.version");
        assert_eq!(decl.segments.len(), 2);
        assert_eq!(decl.segments[0].text, "Sphinx");
        assert_eq!(decl.segments[0].sep_after, ".");
        assert_eq!(decl.segments[1].text, "version");
    }

    #[test]
    fn parses_named_type_prefix() {
        let decl = parse_declaration("MyStruct hello2(char *name)", "function", "c");
        assert_eq!(decl.name, "hello2");
        assert!(matches!(decl.prefix.first(), Some(SigToken::Ref { .. })));
    }

    #[test]
    fn parses_macro_with_parameters() {
        let decl = parse_declaration("SPHINX(arg1, arg2)", "macro", "c");
        assert_eq!(decl.name, "SPHINX");
        assert_eq!(decl.params.len(), 2);
    }

    #[test]
    fn anonymous_fields_are_preserved() {
        let decl = parse_declaration("@data", "union", "c");
        assert_eq!(decl.name, "@data");
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for src in ["", "(", "(((", "*", "int )", ":::", "void (),,", "a < < < b"] {
            let decl = parse_declaration(src, "function", "c");
            let _ = decl.name;
        }
    }
}
