//! Hand-written C++ declaration parser (Phase 13).
//!
//! Covers the common subset of `sphinx/domains/cpp/_parser.py`: `::`-qualified
//! names, templates, references (`&` / `&&`), cv-qualifiers, builtin and named
//! types, function parameter lists, operators, constructors/destructors and
//! trailing `= alias` declarations.  Malformed input degrades gracefully.

use crate::c::parser::{tokenize, Tok, TokKind};
use crate::cpp::ids::{MangleDecl, MangleParam};
use crate::sigbuild::SigToken;

/// One segment of a C++ qualified name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameSegment {
    /// Segment text (may include an operator spelling).
    pub text: String,
    /// Separator emitted after this segment (`::` or `.`).
    pub sep_after: String,
}

/// A parsed C++ declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CppDecl {
    /// Leading tokens (template header, cv-qualifiers, return type, ...).
    pub prefix: Vec<SigToken>,
    /// Qualified name segments; last is the declared name.
    pub segments: Vec<NameSegment>,
    /// Function parameter lists.
    pub params: Vec<Vec<SigToken>>,
    /// Tokens after the name.
    pub suffix: Vec<SigToken>,
    /// `::`-joined full name.
    pub name: String,
    /// Whether a function parameter list was present.
    pub is_function: bool,
    /// Template arguments on the declared name (`f<int>`).
    pub template_args: Vec<String>,
    /// Names introduced by a leading `template<...>` header.
    pub template_params: Vec<String>,
    /// Token groups of each template parameter (for the signature line).
    pub template_param_tokens: Vec<Vec<SigToken>>,
    /// Named function parameters (`void f(int a)` → `["a"]`).
    pub function_params: Vec<String>,
    /// Alias target tokens (`type X = Y`).
    pub alias: Option<Vec<SigToken>>,
}

impl CppDecl {
    /// Final (unqualified) name.
    pub fn short_name(&self) -> &str {
        self.segments.last().map_or(self.name.as_str(), |segment| segment.text.as_str())
    }
}

/// Builtin C++ type keywords.
pub const BUILTIN_TYPES: &[&str] = &[
    "void", "bool", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
    "wchar_t", "char16_t", "char32_t", "auto", "size_t", "std::size_t",
];

/// Declaration keywords rendered as `desc_sig_keyword`.
pub const KEYWORDS: &[&str] = &[
    "const", "volatile", "constexpr", "static", "inline", "virtual", "explicit", "friend",
    "public", "protected", "private", "typename", "class", "struct", "union", "enum", "typedef",
    "template", "mutable",
];

fn normalized(src: &str) -> &str {
    let mut src = src.trim();
    while src.ends_with(';') {
        src = src[..src.len() - 1].trim_end();
    }
    src
}

/// Parses a C++ declaration for the given object type.
pub fn parse_cpp_declaration(signature: &str, objtype: &str, domain: &str) -> CppDecl {
    let toks = tokenize(normalized(signature));
    if toks.is_empty() {
        return CppDecl {
            prefix: Vec::new(), segments: Vec::new(), params: Vec::new(), suffix: Vec::new(),
            name: String::new(), is_function: false, template_args: Vec::new(),
            template_params: Vec::new(), template_param_tokens: Vec::new(),
            function_params: Vec::new(), alias: None,
        };
    }
    parse_tokens(&toks, objtype, domain)
}

fn parse_tokens(toks: &[Tok], objtype: &str, domain: &str) -> CppDecl {
    // Split off a trailing `= alias` at depth zero.
    let mut depth = 0i32;
    let mut alias_at = None;
    for (index, token) in toks.iter().enumerate() {
        match token.text.as_str() {
            "(" | "[" | "{" | "<" => depth += 1,
            ")" | "]" | "}" | ">" | ">>" => depth -= 1,
            "=" if depth == 0 => alias_at = Some(index),
            _ => {}
        }
    }
    let (head, alias) = match alias_at {
        Some(index) => (&toks[..index], Some(type_tokens(&toks[index + 1..], domain))),
        None => (toks, None),
    };

    // An alias with nothing before it (e.g. `= aa`) leaves no declaration head.
    // Return a well-formed empty declaration instead of indexing `head[0]` below
    // (which panicked before the Phase 29 fuzz campaign exposed it).
    if head.is_empty() {
        return CppDecl {
            prefix: Vec::new(), segments: Vec::new(), params: Vec::new(), suffix: Vec::new(),
            name: String::new(), is_function: false, template_args: Vec::new(),
            template_params: Vec::new(), template_param_tokens: Vec::new(),
            function_params: Vec::new(), alias,
        };
    }

    // Strip a leading `template < ... >` header and remember the introduced
    // parameter names. Upstream registers each as a `templateParam` object so
    // roles inside the declaration can refer to it by bare name.
    let (head, template_header, template_param_groups, template_params) = strip_template_header(head);
    let template_param_tokens = template_param_groups
        .iter()
        .map(|group| type_tokens(group, domain))
        .collect::<Vec<_>>();

    // Find the last top-level word that can be the declared name.
    let mut nesting = 0i32;
    let mut candidate = None;
    let mut operator_at = None;
    for (index, token) in head.iter().enumerate() {
        // A single `:` starts an enum's underlying-type/base clause; the
        // declared name is the last identifier before it (`enum E : int`).
        if token.text == ":" && head.get(index + 1).is_none_or(|next| next.text != ":") {
            break;
        }
        match token.text.as_str() {
            "(" | "[" | "{" => nesting += 1,
            ")" | "]" | "}" => nesting -= 1,
            "<" => nesting += 1,
            ">" | ">>" => nesting -= 1,
            _ => {}
        }
        if nesting != 0 || token.kind != TokKind::Word {
            continue;
        }
        if token.text == "operator" {
            operator_at = Some(index);
        }
        if BUILTIN_TYPES.contains(&token.text.as_str()) || KEYWORDS.contains(&token.text.as_str()) {
            continue;
        }
        candidate = Some(index);
    }
    // An operator name always begins at `operator`, even when the following
    // spelling is a literal (`operator""_lit`) or a builtin type
    // (`operator int` conversion operator).
    let name_index = operator_at.or(candidate)
        .unwrap_or_else(|| head.iter().position(|token| token.kind == TokKind::Word).unwrap_or(0));

    // Extend backwards across `::` chains.
    let mut start = name_index;
    while start >= 2 {
        let sep = &head[start - 1];
        if sep.kind != TokKind::Punct || !matches!(sep.text.as_str(), "::" | "." | "->") {
            break;
        }
        if head[start - 2].kind != TokKind::Word {
            break;
        }
        start -= 2;
    }
    let mut end = name_index + 1;
    // Operator names (`operator+`, `operator()`, `operator[]`, `operator""_x`,
    // `operator int`). The name swallows its operator spelling so lookup by
    // the rendered signature succeeds.
    if head[name_index].text == "operator" {
        if let Some(next) = head.get(end) {
            if matches!(next.text.as_str(), "(" | "[")
                && head.get(end + 1).is_some_and(|t| matches!(t.text.as_str(), ")" | "]"))
            {
                end += 2;
            } else if next.kind == TokKind::Punct {
                end += 1;
            } else if next.kind == TokKind::Literal {
                // User-defined literal: `operator""` plus its suffix.
                end += 1;
                if head.get(end).is_some_and(|token| token.kind == TokKind::Word) {
                    end += 1;
                }
            } else if next.kind == TokKind::Word {
                // Conversion operator: `operator` plus the target type.
                end += 1;
            }
        }
    }
    // Constructor / destructor names are already full words (`~A`).
    // Template arguments on the name (`f<int>`).
    let mut template_args = Vec::new();
    if head.get(end).is_some_and(|t| t.text == "<") {
        let close = matching_close(head, end, "<", ">");
        template_args = if close > end + 1 { split_top(&head[end + 1..close]) } else { Vec::new() }
            .into_iter()
            .map(|tokens| tokens.iter().map(|token| token.text.as_str()).collect::<String>())
            .filter(|arg| !arg.is_empty())
            .collect();
        end = close + 1;
    }

    // Name segments.
    let mut word_indices = Vec::new();
    let mut index = start;
    while index < end.min(name_index + 1) {
        if head[index].kind == TokKind::Word {
            word_indices.push(index);
        }
        index += 1;
    }
    let mut segments = Vec::new();
    for (position, &word_index) in word_indices.iter().enumerate() {
        let sep_after = if position + 1 < word_indices.len() {
            head[word_indices[position + 1] - 1].text.clone()
        } else {
            String::new()
        };
        if head[word_index].text == "operator" {
            let mut text = String::from("operator");
            for token in &head[word_index + 1..end] {
                text.push_str(&token.text);
            }
            segments.push(NameSegment { text, sep_after });
            break;
        }
        segments.push(NameSegment { text: head[word_index].text.clone(), sep_after });
    }
    if segments.is_empty() {
        segments.push(NameSegment { text: String::new(), sep_after: String::new() });
    }

    // Prefix tokens before the name chain. Upstream strips visibility
    // specifiers from class-like declarations and renders the class keyword.
    let leading: Vec<Tok> = if matches!(objtype, "class" | "struct" | "union") {
        head[..start].iter()
            .filter(|token| !matches!(token.text.as_str(), "public" | "protected" | "private"))
            .cloned()
            .collect()
    } else {
        head[..start].to_vec()
    };
    let mut prefix = type_tokens(&leading, domain);
    let _ = template_header; // rendered as its own signature line by the writer
    let keyword = match objtype {
        "class" | "struct" => Some("class"),
        "union" => Some("union"),
        "enum" => Some("enum"),
        "enum-class" => Some("enum-class"),
        "enum-struct" => Some("enum-struct"),
        "enumerator" => Some("enumerator"),
        "concept" => Some("concept"),
        "type" if prefix.is_empty() => Some("type"),
        "type" => Some("typedef"),
        _ => None,
    };
    if let Some(keyword) = keyword {
        let mut with_keyword = Vec::new();
        if keyword == "enum-class" {
            with_keyword.push(SigToken::Keyword("enum".to_owned()));
            with_keyword.push(SigToken::Space);
            with_keyword.push(SigToken::Keyword("class".to_owned()));
        } else if keyword == "enum-struct" {
            with_keyword.push(SigToken::Keyword("enum".to_owned()));
            with_keyword.push(SigToken::Space);
            with_keyword.push(SigToken::Keyword("struct".to_owned()));
        } else {
            with_keyword.push(SigToken::Keyword(keyword.to_owned()));
        }
        with_keyword.push(SigToken::Space);
        with_keyword.append(&mut prefix);
        prefix = with_keyword;
    }

    // Suffix tokens / parameters.
    let rest = &head[end..];
    let mut params = Vec::new();
    let mut function_params = Vec::new();
    let mut suffix_tokens = rest;
    if rest.first().is_some_and(|token| token.text == "(") {
        let close = matching_close(rest, 0, "(", ")");
        if close > 0 {
            let groups = split_top(&rest[1..close]);
            function_params = groups.iter().filter_map(|group| parameter_name(group)).collect();
            params = groups
                .into_iter()
                .map(|param| param_tokens(&param, domain))
                .collect();
        }
        suffix_tokens = rest.get(close + 1..).unwrap_or(&[]);
    }
    let suffix = type_tokens(suffix_tokens, domain);
    let is_function = !rest.is_empty() && rest[0].text == "(";
    let name = segments.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>().join("::");

    CppDecl {
        prefix, segments, params, suffix, name, is_function, template_args,
        template_params, template_param_tokens, function_params, alias,
    }
}

/// Strips a leading `template < ... >` header, returning the remaining tokens
/// and the names of the introduced template parameters.
type TemplateHeader<'a> = (&'a [Tok], &'a [Tok], Vec<Vec<Tok>>, Vec<String>);

fn strip_template_header(toks: &[Tok]) -> TemplateHeader<'_> {
    if toks.first().is_none_or(|token| token.text != "template") || toks.get(1).is_none_or(|token| token.text != "<") {
        return (toks, &[], Vec::new(), Vec::new());
    }
    let mut depth = 0i32;
    let mut close = None;
    for (index, token) in toks.iter().enumerate().skip(1) {
        if token.text == "<" {
            depth += 1;
        } else if token.text == ">" {
            depth -= 1;
            if depth == 0 {
                close = Some(index);
                break;
            }
        }
    }
    let Some(close) = close else { return (toks, &[], Vec::new(), Vec::new()) };
    let groups = split_top(&toks[2..close]);
    let names = groups.iter().filter_map(|group| parameter_name(group)).collect();
    (&toks[close + 1..], &toks[..=close], groups, names)
}

/// Finds the declared name of a template/function parameter group: the last
/// identifier that is not a type keyword or a builtin type name.
fn parameter_name(toks: &[Tok]) -> Option<String> {
    toks.iter()
        .rev()
        .find(|token| {
            token.kind == TokKind::Word
                && !BUILTIN_TYPES.contains(&token.text.as_str())
                && !matches!(
                    token.text.as_str(),
                    "typename" | "class" | "struct" | "template" | "const" | "volatile"
                        | "int" | "auto" | "unsigned" | "signed" | "long" | "short"
                )
        })
        .map(|token| token.text.clone())
}

/// Converts tokens to signature tokens.
pub fn type_tokens(toks: &[Tok], domain: &str) -> Vec<SigToken> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < toks.len() {
        let token = &toks[index];
        match token.kind {
            TokKind::Number => out.push(SigToken::Number(token.text.clone())),
            TokKind::Literal => out.push(SigToken::Literal(token.text.clone())),
            TokKind::Word if BUILTIN_TYPES.contains(&token.text.as_str()) => {
                out.push(SigToken::KeywordType(token.text.clone()));
            }
            TokKind::Word if KEYWORDS.contains(&token.text.as_str()) => {
                out.push(SigToken::Keyword(token.text.clone()));
            }
            TokKind::Word => {
                if index > 0 && matches!(toks[index - 1].text.as_str(), "::" | "." | "->") {
                    // Merge `std::vector` into one xref target so the whole
                    // qualified name can be suppressed as a std library type.
                    if let Some(SigToken::Ref { text, target, .. }) = out.last_mut() {
                        text.push_str("::");
                        text.push_str(&token.text);
                        target.clone_from(text);
                    } else {
                        out.push(SigToken::Punctuation(toks[index - 1].text.clone()));
                        out.push(SigToken::reference(domain, token.text.clone(), "identifier", "cpp-identifier"));
                    }
                } else {
                    out.push(SigToken::reference(domain, token.text.clone(), "identifier", "cpp-identifier"));
                }
            }
            TokKind::Punct => match token.text.as_str() {
                "::" | "." | "->" => {}
                "&" | "&&" | "*" | "(" | ")" | "[" | "]" | "," | "..." | "template" => {
                    out.push(SigToken::Punctuation(token.text.clone()));
                }
                "<" | ">" => out.push(SigToken::Punctuation(token.text.clone())),
                "~" | "+" | "-" | "==" | "!=" | "<<" | ">>" | "++" | "--" => {
                    out.push(SigToken::Operator(token.text.clone()));
                }
                _ => out.push(SigToken::Punctuation(token.text.clone())),
            },
        }
        // Whitespace between word-ish tokens.
        let next = toks.get(index + 1);
        if let Some(next) = next {
            // No space between a pointer/reference marker and the following
            // name (`char *name`, `T& value` → upstream `T`, `&`, `value`).
            let pointer_marker = matches!(token.text.as_str(), "*" | "&" | "&&");
            let word = token.kind == TokKind::Word || matches!(token.text.as_str(), ">" | ">>");
            let next_word = next.kind == TokKind::Word || matches!(next.text.as_str(), ">" | "*" | "&" | "&&");
            if !pointer_marker && word && next_word {
                out.push(SigToken::Space);
            }
        }
        index += 1;
    }
    out
}

fn param_tokens(toks: &[Tok], domain: &str) -> Vec<SigToken> {
    let mut tokens = type_tokens(toks, domain);
    if let Some(last) = tokens.iter_mut().rev().find(|token| !matches!(token, SigToken::Space)) {
        if let SigToken::Ref { text, .. } = last {
            *last = SigToken::Name(text.clone());
        }
    }
    tokens
}

fn split_top(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0i32;
    for token in toks {
        match token.text.as_str() {
            "(" | "[" | "{" | "<" => depth += 1,
            ")" | "]" | "}" | ">" | ">>" => depth -= 1,
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

/// Builds the input for Itanium-style mangling from a parsed declaration.
pub fn mangle_input(decl: &CppDecl) -> MangleDecl {
    let names = decl.segments.iter().map(|segment| segment.text.clone()).collect();
    let params = decl
        .params
        .iter()
        .map(|param| {
            let mut ty = String::new();
            let mut pointers = 0usize;
            let mut reference = 0u8;
            let mut is_const = false;
            for token in param {
                match token {
                    SigToken::KeywordType(text) | SigToken::Name(text) => {
                        if text == "const" {
                            is_const = true;
                        } else {
                            ty.push_str(text);
                        }
                    }
                    SigToken::Ref { text, .. } => ty.push_str(text),
                    SigToken::Punctuation(text) if text == "*" => pointers += 1,
                    SigToken::Punctuation(text) if text == "&" => reference = 1,
                    SigToken::Punctuation(text) if text == "&&" => reference = 2,
                    SigToken::Keyword(text) if text == "const" => is_const = true,
                    _ => {}
                }
            }
            MangleParam { ty, pointers, reference, is_const }
        })
        .collect();
    MangleDecl { names, params, template_args: decl.template_args.clone() }
}

/// Parses a C++ xref target into the declared name it refers to.
///
/// Upstream's `parse_xref_object` accepts both a bare/shorthand name and a
/// full declaration (`int g()`, `int operator+(int, int)`); only the declared
/// name participates in symbol lookup.
pub fn parse_cpp_xref(signature: &str) -> String {
    let trimmed = signature.trim();
    // A bare qualified identifier is already normalized.
    if !trimmed.contains('(') && !trimmed.contains('<') && !trimmed.contains("operator")
        && !trimmed.chars().any(|ch| ch.is_whitespace())
    {
        return trimmed.trim_start_matches("::").to_owned();
    }
    let decl = parse_cpp_declaration(signature, "function", "cpp");
    if decl.name.is_empty() {
        return trimmed.trim_start_matches("::").to_owned();
    }
    decl.name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qualified_member() {
        let decl = parse_cpp_declaration("float Sphinx::version", "member", "cpp");
        assert_eq!(decl.name, "Sphinx::version");
        assert_eq!(decl.segments.len(), 2);
        assert_eq!(decl.segments[0].sep_after, "::");
    }

    #[test]
    fn parses_template_type() {
        let decl = parse_cpp_declaration("std::vector<int> List", "type", "cpp");
        assert_eq!(decl.name, "List");
        assert!(decl.prefix.iter().any(|token| matches!(token, SigToken::Ref { .. })));
    }

    #[test]
    fn parses_operator_function() {
        let decl = parse_cpp_declaration("int operator+(int, int)", "function", "cpp");
        assert_eq!(decl.name, "operator+");
        assert_eq!(decl.params.len(), 2);
        assert!(decl.is_function);
    }

    #[test]
    fn parses_alias() {
        let decl = parse_cpp_declaration("Alias = int;", "type", "cpp");
        assert_eq!(decl.name, "Alias");
        assert!(decl.alias.is_some());
    }

    #[test]
    fn malformed_input_never_panics() {
        for src in ["", "<", "::", "template<", "void f(", "operator", "a >>> b"] {
            let decl = parse_cpp_declaration(src, "function", "cpp");
            let _ = mangle_input(&decl);
        }
    }
}
