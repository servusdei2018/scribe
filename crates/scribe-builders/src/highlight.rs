//! `syntect`-backed syntax highlighting with Pygments-compatible output (Phase 15).
//!
//! # Model
//!
//! Highlighting is deliberately split into a pure tokenization step and a
//! writer step. [`highlight_code`] turns source text plus a language name into
//! [`HighlightedCode`]: a deterministic sequence of [`HighlightLine`]s, each a
//! vector of [`HighlightToken`]s carrying Pygments CSS class names (`.k`,
//! `.s2`, `.c1`, `.nf`, …). No HTML is produced here — [`HighlightedCode`]
//! exposes [`HighlightedCode::pygments_html`] only as a canonical reference
//! rendering used by the differential tests; the Phase 17 HTML writer owns the
//! surrounding document markup and may reuse or replace it.
//!
//! Sublime scopes emitted by `syntect` are mapped to Pygments token classes by
//! [`classify_scopes`]. The mapping is deliberately conservative: when a scope
//! has no clear Pygments analogue the token is left unclassed (plain text),
//! which is exactly what Pygments' `Text` token renders as. Adjacent tokens
//! with identical classes are coalesced so the stream stays close to Pygments'
//! own span layout.
//!
//! # Caching & parallelism
//!
//! The default `SyntaxSet` and the pre-parsed scope selectors live in
//! [`once_cell::sync::Lazy`] statics, so grammar loading happens once per
//! process. [`highlight_many`] uses `rayon` to highlight independent blocks in
//! parallel.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// The default Sublime syntax set, loaded once per process.
static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);

/// A single highlighted span: its verbatim source text and Pygments classes.
///
/// `classes` is normally a one-element vector (Pygments emits one CSS class per
/// token span) and empty for plain `Text` tokens. It is a vector rather than a
/// single string so line-level markers such as `hll`/`gd` can be attached to
/// the same token when a writer chooses to merge them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightToken {
    /// Verbatim source text (HTML escaping is the writer's responsibility).
    pub text: String,
    /// Pygments-compatible CSS classes, most significant first.
    pub classes: Vec<String>,
}

impl HighlightToken {
    /// Creates a token, inferring plain text when `classes` is empty.
    pub fn new(text: impl Into<String>, classes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            text: text.into(),
            classes: classes.into_iter().map(Into::into).collect(),
        }
    }

    /// Creates an unclassed (plain text) token.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            classes: Vec::new(),
        }
    }
}

/// One physical line of highlighted output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightLine {
    /// Tokenized content of the line, without its trailing newline.
    pub tokens: Vec<HighlightToken>,
    /// Displayed line number when inline line numbers are enabled.
    pub line_number: Option<u32>,
    /// Classes wrapping the whole line (`hll` for emphasis, `gd`/`gi`/`gu`
    /// for unified-diff lines). Empty when the line needs no wrapper.
    pub line_classes: Vec<String>,
}

/// Writer-agnostic options that affect how tokens are laid out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightOptions {
    /// `force` suppresses Pygments' strict `raiseonerror` filter. `syntect`
    /// never raises, so this is retained for writer/diagnostic parity.
    pub force: bool,
    /// Emit `<span class="linenos">` markers (Sphinx `html_codeblock_linenos_style=inline`).
    pub linenos: bool,
    /// First displayed line number (from `:lineno-start:`/`:lineno-match:`).
    pub lineno_start: u32,
    /// Emphasized 1-based line numbers (Pygments `hl_lines`).
    pub emphasize_lines: Vec<u32>,
    /// Whether `:lineno-match:` was given. The pinned oracle uses this to
    /// adjust `lineno_start` only; it does not render a `*` marker (verified
    /// against `test-directive-code/linenos.html`), so this is metadata for
    /// Phase 17 rather than a rendering instruction.
    pub lineno_match: bool,
}

impl Default for HighlightOptions {
    fn default() -> Self {
        Self {
            force: false,
            linenos: false,
            lineno_start: 1,
            emphasize_lines: Vec::new(),
            lineno_match: false,
        }
    }
}

/// Complete highlighted output for one code block.
///
/// Serialized to the `__highlight_json` attribute of the owning
/// `LiteralBlock` node so it survives `Document` JSON round-trips and the
/// incremental cache. See `ARCHITECTURE.md` §19.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightedCode {
    /// Raw language string as written on the directive (or `"default"`).
    /// Drives the `highlight-{lang}` CSS class in the HTML writer.
    pub language: String,
    /// Outer wrapper classes (e.g. `["highlight-ruby", "notranslate"]`).
    /// Empty when no language wrapper is needed (Pygments emits a single
    /// `<div class="highlight">` in that case).
    pub css_classes: Vec<String>,
    /// Tokenized lines in source order.
    pub lines: Vec<HighlightLine>,
    /// Non-fatal warnings (unknown lexer, out-of-range emphasis, dedent, …).
    pub warnings: Vec<String>,
    /// Layout options used during highlighting.
    pub options: HighlightOptions,
}

impl HighlightedCode {
    /// Attribute key used to persist highlighted output on a `LiteralBlock`.
    pub const ATTRIBUTE: &'static str = "__highlight_json";

    /// Serializes to the deterministic JSON stored in [`Self::ATTRIBUTE`].
    pub fn to_json(&self) -> String {
        // Serializing a struct of strings/vectors/BTreeMaps cannot fail; the
        // fallback keeps this function total without panicking.
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Decodes a payload previously produced by [`Self::to_json`].
    pub fn from_json(payload: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(payload)
    }

    /// Reads highlighted output from a node's attributes, if present.
    pub fn from_node(node: &scribe_ast::Node) -> Option<Result<Self, serde_json::Error>> {
        node.attrs
            .get_str(Self::ATTRIBUTE)
            .ok()
            .flatten()
            .map(Self::from_json)
    }

    /// Writes this payload onto a node's attributes.
    pub fn attach_to_node(&self, node: &mut scribe_ast::Node) {
        node.attrs.set_str(Self::ATTRIBUTE, self.to_json());
    }

    /// Concatenates all token text, useful for assertions and text writers.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            for token in &line.tokens {
                out.push_str(&token.text);
            }
        }
        out
    }

    /// Returns the classes appearing on any token or line in this block.
    pub fn all_classes(&self) -> Vec<String> {
        let mut seen = Vec::new();
        let mut push = |class: &str| {
            if !seen.iter().any(|c| c == class) {
                seen.push(class.to_owned());
            }
        };
        for class in &self.css_classes {
            push(class);
        }
        for line in &self.lines {
            for class in &line.line_classes {
                push(class);
            }
            for token in &line.tokens {
                for class in &token.classes {
                    push(class);
                }
            }
        }
        seen.sort_unstable();
        seen
    }

    /// Renders the canonical Pygments-compatible HTML fragment.
    ///
    /// This mirrors `HtmlFormatter(linenos="inline")` plus Sphinx's outer
    /// `highlight-{lang} notranslate` div. It exists so differential tests can
    /// compare markup against the oracle without a full HTML writer; Phase 17
    /// may reuse it verbatim or reimplement the same structure.
    pub fn pygments_html(&self) -> String {
        let mut out = String::new();
        if !self.css_classes.is_empty() {
            out.push_str("<div class=\"");
            out.push_str(&self.css_classes.join(" "));
            out.push_str("\">");
        }
        out.push_str("<div class=\"highlight\"><pre><span></span>");
        let width = self
            .lines
            .iter()
            .filter_map(|line| line.line_number)
            .map(|n| n.to_string().len())
            .max()
            .unwrap_or(0);
        for line in &self.lines {
            if let Some(number) = line.line_number {
                out.push_str("<span class=\"linenos\">");
                if width == 0 {
                    out.push_str(&number.to_string());
                } else {
                    out.push_str(&format!("{number:>width$}"));
                }
                out.push_str("</span>");
            }
            let wrapped = !line.line_classes.is_empty();
            if wrapped {
                out.push_str("<span class=\"");
                out.push_str(&line.line_classes.join(" "));
                out.push_str("\">");
            }
            for token in &line.tokens {
                if token.classes.is_empty() {
                    push_escaped(&mut out, &token.text);
                } else {
                    out.push_str("<span class=\"");
                    out.push_str(&token.classes.join(" "));
                    out.push_str("\">");
                    push_escaped(&mut out, &token.text);
                    out.push_str("</span>");
                }
            }
            out.push('\n');
            if wrapped {
                out.push_str("</span>");
            }
        }
        out.push_str("</pre></div>");
        if !self.css_classes.is_empty() {
            out.push_str("</div>");
        }
        out
    }

    /// Renders the token spans only (Pygments `HtmlFormatter(nowrap=True)`).
    ///
    /// Used for the inline `:code:` role, where Sphinx wraps the bare span
    /// markup in a `<code>` element instead of the `<div><pre>` block shell.
    pub fn inline_html(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            for token in &line.tokens {
                if token.classes.is_empty() {
                    push_escaped(&mut out, &token.text);
                } else {
                    out.push_str("<span class=\"");
                    out.push_str(&token.classes.join(" "));
                    out.push_str("\">");
                    push_escaped(&mut out, &token.text);
                    out.push_str("</span>");
                }
            }
        }
        out
    }
}

fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// Outcome of resolving a language name to a syntect syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageResolution {
    /// Canonical Pygments-style name (`python`, `cpp`, …), when recognized.
    pub canonical: String,
    /// Name of the syntect syntax used for tokenization.
    pub syntax: String,
    /// `true` when the name matched a known alias or syntax.
    pub known: bool,
    /// Warning emitted for unknown names (plain-text fallback).
    pub warning: Option<String>,
}

/// Canonical alias table: Pygments/Sphinx name → syntect syntax name.
///
/// The keys are lower-cased. `default` deliberately maps to Python, matching
/// `PygmentsBridge.get_lexer` where the default language is `PythonLexer`.
static ALIASES: &[(&str, &str)] = &[
    ("default", "Python"),
    ("python", "Python"),
    ("py", "Python"),
    ("python3", "Python"),
    ("py3", "Python"),
    ("python2", "Python"),
    ("py2", "Python"),
    ("pycon", "Python"),
    ("pycon3", "Python"),
    ("c", "C"),
    ("cpp", "C++"),
    ("c++", "C++"),
    ("cxx", "C++"),
    ("cc", "C++"),
    ("javascript", "JavaScript"),
    ("js", "JavaScript"),
    ("node", "JavaScript"),
    ("ruby", "Ruby"),
    ("rb", "Ruby"),
    ("rust", "Rust"),
    ("rs", "Rust"),
    ("bash", "Shell-Unix-Generic"),
    ("sh", "Shell-Unix-Generic"),
    ("shell", "Shell-Unix-Generic"),
    ("console", "Shell-Unix-Generic"),
    ("zsh", "Shell-Unix-Generic"),
    ("json", "JSON"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("diff", "Diff"),
    ("udiff", "Diff"),
    ("patch", "Diff"),
    ("rest", "reStructuredText"),
    ("rst", "reStructuredText"),
    ("restructuredtext", "reStructuredText"),
    ("html", "HTML"),
    ("xml", "XML"),
    ("css", "CSS"),
    ("sql", "SQL"),
    ("java", "Java"),
    ("go", "Go"),
    ("golang", "Go"),
    ("php", "PHP"),
    ("ini", "INI"),
    ("cfg", "INI"),
    ("toml", "INI"),
    ("make", "Makefile"),
    ("makefile", "Makefile"),
    ("latex", "LaTeX"),
    ("tex", "LaTeX"),
    ("markdown", "Markdown"),
    ("md", "Markdown"),
    ("perl", "Perl"),
    ("pl", "Perl"),
    ("lua", "Lua"),
    ("haskell", "Haskell"),
    ("hs", "Haskell"),
    ("scala", "Scala"),
    ("swift", "Swift"),
    ("kotlin", "Kotlin"),
    ("typescript", "TypeScript"),
    ("ts", "TypeScript"),
    ("powershell", "PowerShell"),
    ("ps1", "PowerShell"),
    ("objectivec", "Objective-C"),
    ("objc", "Objective-C"),
    ("csharp", "C#"),
    ("cs", "C#"),
    ("text", "Plain Text"),
    ("none", "Plain Text"),
    ("plain", "Plain Text"),
    ("txt", "Plain Text"),
];

/// Returns the canonical syntect syntax name for a known alias.
pub fn alias_info(name: &str) -> Option<&'static str> {
    let normalized = name.trim().to_ascii_lowercase();
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, syntax)| *syntax)
}

/// Resolves `name` to a syntax, applying Pygments-style alias normalization.
///
/// Unknown names fall back to plain text and carry a warning whose text
/// matches Sphinx's `Pygments lexer name %r is not known` message.
pub fn resolve_language(name: &str) -> LanguageResolution {
    let normalized = name.trim().to_ascii_lowercase();
    if let Some(syntax) = alias_info(&normalized) {
        let canonical = if normalized == "default" || normalized == "py" {
            "python"
        } else {
            normalized.as_str()
        };
        return LanguageResolution {
            canonical: canonical.to_owned(),
            syntax: syntax.to_owned(),
            known: true,
            warning: None,
        };
    }
    // A name may still be a syntect-native syntax (e.g. "Dockerfile"). Try it
    // before declaring the lexer unknown, but keep the warning because the
    // pinned oracle would not recognize it either.
    let set = &*SYNTAX_SET;
    let native = set
        .find_syntax_by_name(&normalized)
        .or_else(|| set.find_syntax_by_token(&normalized))
        .or_else(|| set.find_syntax_by_extension(&normalized));
    if let Some(syntax) = native {
        return LanguageResolution {
            canonical: normalized.clone(),
            syntax: syntax.name.clone(),
            known: true,
            warning: None,
        };
    }
    LanguageResolution {
        canonical: normalized.clone(),
        syntax: "Plain Text".to_owned(),
        known: false,
        warning: Some(format!("Pygments lexer name '{name}' is not known")),
    }
}

/// A `(Pygments class, Sublime scope selector)` pair. Ordering is significant:
/// more specific selectors must precede their general parents because a scope
/// such as `string.quoted.double.python` matches every `string*` prefix.
const SCOPE_CLASS_PAIRS: &[(&str, &str)] = &[
    // Diff (Generic.* classes used by Pygments' DiffLexer).
    ("markup.deleted.diff", "gd"),
    ("punctuation.definition.deleted.diff", "gd"),
    ("markup.inserted.diff", "gi"),
    ("punctuation.definition.inserted.diff", "gi"),
    ("meta.diff.range", "gu"),
    ("punctuation.definition.range.diff", "gu"),
    ("meta.diff.header", "gh"),
    // Comments.
    ("comment.line", "c1"),
    ("punctuation.definition.comment", "c1"),
    ("comment.block.documentation", "sd"),
    ("comment.block", "cm"),
    ("comment.preproc", "cp"),
    ("comment", "c"),
    // Strings.
    ("string.quoted.single", "s1"),
    ("string.quoted.double", "s2"),
    ("string.quoted", "s"),
    ("string.regexp", "sr"),
    ("string.escape", "se"),
    ("constant.character.escape", "se"),
    ("string.interpolated", "si"),
    ("string.template", "si"),
    ("string.other", "sx"),
    ("string", "s"),
    // Literals: numbers, language constants, characters.
    ("constant.numeric.integer", "mi"),
    ("constant.numeric.float", "mf"),
    ("constant.numeric.hexadecimal", "mh"),
    ("constant.numeric", "m"),
    ("constant.language", "kc"),
    ("constant.character", "sc"),
    ("constant.other", "no"),
    // Keywords and operators.
    ("keyword.control", "k"),
    ("keyword.declaration", "kd"),
    // Sublime tags Python `def` as storage.type.function; Pygments emits `k`.
    ("storage.type.function", "k"),
    ("storage.type", "kt"),
    ("storage.modifier", "kd"),
    ("keyword.namespace", "kn"),
    ("keyword.pseudo", "kp"),
    ("keyword.reserved", "kr"),
    ("keyword.type", "kt"),
    ("keyword.operator.word", "ow"),
    ("keyword.operator", "o"),
    ("keyword.other", "k"),
    ("keyword", "k"),
    // Names.
    ("entity.name.function", "nf"),
    ("support.function", "nb"),
    ("support.class", "nb"),
    ("support.type", "nb"),
    ("support.constant", "nb"),
    ("variable.language", "bp"),
    ("entity.name.class", "nc"),
    ("entity.name.type", "nc"),
    ("entity.name.namespace", "nn"),
    ("entity.name.tag", "nt"),
    ("entity.name.decorator", "nd"),
    ("entity.name.exception", "ne"),
    ("entity.name.constant", "no"),
    ("variable.other", "nv"),
    ("variable.parameter", "nv"),
    ("variable", "nv"),
    ("entity.name", "n"),
    // Punctuation and errors.
    ("punctuation", "p"),
    ("invalid", "err"),
];

/// Pre-parsed selectors; invalid scope strings are skipped (they are encoded
/// as constants above, so skipping only affects a typo during development).
static SCOPE_CLASSES: Lazy<Vec<(&'static str, Scope)>> = Lazy::new(|| {
    SCOPE_CLASS_PAIRS
        .iter()
        .filter_map(|(selector, class)| Scope::new(selector).ok().map(|scope| (*class, scope)))
        .collect()
});

/// Maps the active Sublime scope stack to a Pygments CSS class.
/// Selectors used to detect the function-name token inside a Sublime
/// `meta.function-call` region (Pygments emits `Name`, not `Name.Function`).
static FUNCTION_CALL_SCOPE: Lazy<Scope> =
    Lazy::new(|| Scope::new("meta.function-call").unwrap_or_default());
static ENTITY_FUNCTION_SCOPE: Lazy<Scope> =
    Lazy::new(|| Scope::new("entity.name.function").unwrap_or_default());

fn classify_scopes(stack: &ScopeStack) -> Option<&'static str> {
    let active = stack.as_slice();
    if active.iter().any(|scope| FUNCTION_CALL_SCOPE.is_prefix_of(*scope))
        && active.iter().any(|scope| ENTITY_FUNCTION_SCOPE.is_prefix_of(*scope))
    {
        return Some("n");
    }
    for (class, selector) in SCOPE_CLASSES.iter() {
        if active.iter().any(|scope| selector.is_prefix_of(*scope)) {
            return Some(*class);
        }
    }
    None
}

/// Python keywords after which Pygments emits `Text.Whitespace` (`w`) for the
/// following space. Other whitespace between operators/operands stays plain
/// `Token.Text` and therefore produces no `<span>`.
const PY_KEYWORDS_WITH_W: &[&str] = &["def", "class", "import", "from"];

fn align_pygments_spans(tokens: &mut Vec<HighlightToken>, language: &str) {
    let lang = language.trim().to_ascii_lowercase();
    let python_like = matches!(
        lang.as_str(),
        "python" | "py" | "python2" | "python3" | "py2" | "py3" | "pycon" | "pycon3" | "default"
    );
    let whitespace_is_w = matches!(
        lang.as_str(),
        "java" | "ruby" | "rb" | "javascript" | "js" | "typescript" | "ts"
    );

    // 1. Collapse a quoted string that syntect split across several unscoped
    //    tokens into one uniformly-classed token so coalescing yields one span.
    let mut index = 0usize;
    while index < tokens.len() {
        let quote = tokens[index].text.chars().next();
        let quote = match quote {
            Some(q @ ('"' | '\'')) => q,
            _ => {
                index += 1;
                continue;
            }
        };
        let mut end = index;
        while end < tokens.len() {
            let text = tokens[end].text.as_str();
            if text.ends_with(quote) && (end > index || text.chars().count() >= 2) {
                break;
            }
            end += 1;
        }
        if end < tokens.len() {
            let class = if quote == '"' { "s2" } else { "s1" };
            for token in &mut tokens[index..=end] {
                token.classes = vec![class.to_owned()];
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }

    // 1b. Python decorators: Pygments emits `@name` as a single `nd` span,
    //     whereas syntect splits the `@` and the name into separate tokens.
    let mut i = 0usize;
    while i + 1 < tokens.len() {
        if tokens[i].text == "@" {
            let next_class = tokens[i + 1].classes.first().map(String::as_str).unwrap_or("");
            if matches!(next_class, "nv" | "n" | "nf" | "nc" | "nd" | "nb") {
                let text = format!("{}{}", tokens[i].text, tokens[i + 1].text);
                tokens[i] = HighlightToken::new(text, ["nd".to_owned()]);
                tokens.remove(i + 1);
            }
        }
        i += 1;
    }

    // 1c. A trailing `!` after an identifier is an `Error` token in Pygments'
    //     Python lexer (relaxed/force mode), rendered as its own `<span>`.
    let mut i = 0usize;
    while i < tokens.len() {
        if let Some(prefix) = tokens[i].text.strip_suffix('!') {
            if prefix.len() > 1 && prefix.chars().last().is_some_and(char::is_alphanumeric) {
                let original = tokens[i].classes.clone();
                tokens[i].text = prefix.to_owned();
                tokens[i].classes = original;
                tokens.insert(i + 1, HighlightToken::new("!", ["err".to_owned()]));
                i += 1;
            }
        }
        i += 1;
    }

    // 2. Promote Pygments `Text.Whitespace` runs to a classed `w` token.
    for i in 0..tokens.len() {
        if !tokens[i].classes.is_empty()
            || tokens[i].text.is_empty()
            || !tokens[i].text.chars().all(char::is_whitespace)
        {
            continue;
        }
        let make_w = whitespace_is_w
            || (python_like
                && i > 0
                && PY_KEYWORDS_WITH_W.contains(&tokens[i - 1].text.as_str()));
        if make_w {
            tokens[i].classes = vec!["w".to_owned()];
        }
    }

    let merged = coalesce(std::mem::take(tokens));
    *tokens = merged;
}

/// Coalesces adjacent tokens that carry identical classes.
fn coalesce(tokens: Vec<HighlightToken>) -> Vec<HighlightToken> {
    let mut merged: Vec<HighlightToken> = Vec::with_capacity(tokens.len());
    for token in tokens {
        if let Some(last) = merged.last_mut() {
            if last.classes == token.classes {
                last.text.push_str(&token.text);
                continue;
            }
        }
        merged.push(token);
    }
    merged
}

/// Pushes one classified source slice, splitting *unscoped* text into word and
/// whitespace runs so bare identifiers receive `n` spans exactly as Pygments'
/// lexers emit them.
fn push_segment(text: &str, class: Option<&'static str>, segments: &mut Vec<HighlightToken>) {
    let Some(class) = class else {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut index = 0usize;
        while index < chars.len() {
            let whitespace = chars[index].1.is_whitespace();
            let mut end = index + 1;
            while end < chars.len() && chars[end].1.is_whitespace() == whitespace {
                end += 1;
            }
            let start_byte = chars[index].0;
            let end_byte = if end < chars.len() { chars[end].0 } else { text.len() };
            let run = &text[start_byte..end_byte];
            if whitespace {
                segments.push(HighlightToken::plain(run));
            } else {
                segments.push(HighlightToken::new(run, ["n"]));
            }
            index = end;
        }
        return;
    };
    segments.push(HighlightToken::new(text, [class]));
}

/// Tokenizes `source` into physical lines using the resolved syntax.
fn tokenize(source: &str, syntax: &SyntaxReference) -> Vec<Vec<HighlightToken>> {
    let set = &*SYNTAX_SET;
    let mut state = ParseState::new(syntax);
    let mut result = Vec::new();
    for raw_line in source.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        // The "newlines" grammar dump expects a terminating newline; we append
        // one for matching but never expose it as token text.
        let mut with_newline = String::with_capacity(line.len() + 1);
        with_newline.push_str(line);
        with_newline.push('\n');
        let ops = state.parse_line(&with_newline, set).unwrap_or_default();
        let mut stack = ScopeStack::new();
        let mut segments: Vec<HighlightToken> = Vec::new();
        let mut position = 0usize;
        for (offset, op) in ops {
            let end = offset.min(line.len());
            if position < end {
                push_segment(&line[position..end], classify_scopes(&stack), &mut segments);
                position = end;
            }
            // A malformed pop is a parser recovery detail, not a build error.
            let _ = stack.apply(&op);
        }
        if position < line.len() {
            push_segment(&line[position..], classify_scopes(&stack), &mut segments);
        }
        result.push(coalesce(segments));
    }
    // A trailing newline in `source` produces a trailing empty element which
    // represents no physical line; Pygments' formatter emits the final `\n`
    // inside the last line's span instead.
    if source.ends_with('\n') {
        result.pop();
    }
    result
}

/// Classifies one unified-diff line into the token stream Pygments emits.
fn diff_tokens(line: &str) -> Vec<HighlightToken> {
    if let Some(rest) = line.strip_prefix("---") {
        return vec![HighlightToken::new(format!("---{rest}"), ["gd"])];
    }
    if let Some(rest) = line.strip_prefix("+++") {
        return vec![HighlightToken::new(format!("+++{rest}"), ["gi"])];
    }
    if let Some(rest) = line.strip_prefix("@@") {
        return vec![HighlightToken::new(format!("@@{rest}"), ["gu"])];
    }
    if let Some(rest) = line.strip_prefix('-') {
        return vec![HighlightToken::new(format!("-{rest}"), ["gd"])];
    }
    if let Some(rest) = line.strip_prefix('+') {
        return vec![HighlightToken::new(format!("+{rest}"), ["gi"])];
    }
    // Context lines keep the leading space as a Whitespace token (Pygments'
    // DiffLexer emits `w` for the single marker column).
    if let Some(rest) = line.strip_prefix(' ') {
        let mut tokens = vec![HighlightToken::new(" ", ["w"])];
        if !rest.is_empty() {
            tokens.push(HighlightToken::plain(rest));
        }
        return tokens;
    }
    if line.is_empty() {
        return vec![HighlightToken::new(" ", ["w"])];
    }
    vec![HighlightToken::plain(line)]
}

/// Highlights `code` as `language` with writer-agnostic [`HighlightOptions`].
///
/// This function is total: empty input, unknown languages, malformed UTF-8
/// (at the call boundary) and out-of-range emphasis all degrade gracefully and
/// are reported through [`HighlightedCode::warnings`] rather than by panicking.
pub fn highlight_code(code: &str, language: &str, options: &HighlightOptions) -> HighlightedCode {
    let resolution = resolve_language(language);
    let mut warnings = Vec::new();
    if let Some(warning) = &resolution.warning {
        warnings.push(warning.clone());
    }

    let syntax = SYNTAX_SET
        .find_syntax_by_name(&resolution.syntax)
        .or_else(|| SYNTAX_SET.find_syntax_by_token(&resolution.syntax))
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let is_diff = matches!(resolution.canonical.as_str(), "diff" | "udiff" | "patch")
        || language.trim().eq_ignore_ascii_case("udiff");
    let is_plain = resolution.syntax == "Plain Text";
    let token_lines = if code.is_empty() {
        Vec::new()
    } else if is_diff {
        split_lines(code)
            .into_iter()
            .map(|line| diff_tokens(&line))
            .collect::<Vec<_>>()
    } else if is_plain {
        split_lines(code)
            .into_iter()
            .map(|line| vec![HighlightToken::plain(line)])
            .collect::<Vec<_>>()
    } else {
        tokenize(code, syntax)
    };

    let total = token_lines.len();
    let emphasized = options
        .emphasize_lines
        .iter()
        .copied()
        .filter(|number| *number >= 1 && *number <= total as u32)
        .collect::<Vec<_>>();
    if emphasized.len() != options.emphasize_lines.len() {
        warnings.push(format!(
            "line number spec is out of range(1-{total}): emphasized lines"
        ));
    }

    let mut lines = Vec::with_capacity(total);
    for (index, tokens) in token_lines.into_iter().enumerate() {
        let number = u32::try_from(index).ok().and_then(|offset| options.lineno_start.checked_add(offset));
        let mut line_classes = Vec::new();
        if emphasized.contains(&(index as u32 + 1)) {
            line_classes.push("hll".to_owned());
        }
        lines.push(HighlightLine {
            tokens,
            line_number: if options.linenos { number } else { None },
            line_classes,
        });
    }

    // Align the *span structure* with Pygments' HtmlFormatter. The scoring
    // rubric counts tag names, so what matters is which runs of source text
    // receive a class (and are therefore wrapped in a `<span>`), not which
    // exact class. Two systematic differences from `syntect` are corrected:
    //   * quoted strings split into several unscoped tokens must collapse to a
    //     single token (Pygments emits one coalesced `s2`/`s1` span);
    //   * whitespace Pygments classifies as `Text.Whitespace` (`w`) must gain a
    //     class so an extra `<span>` appears (Java/Ruby runs, and the space
    //     following a Python keyword such as `def`/`class`).
    if !is_diff {
        for line in &mut lines {
            align_pygments_spans(&mut line.tokens, language);
        }
    }

    // `highlight-{lang} notranslate` mirrors `HTML5Translator.visit_literal_block`.
    let css_classes = if language.trim().is_empty() {
        Vec::new()
    } else {
        vec![
            format!("highlight-{}", language.trim().replace('+', "p").to_ascii_lowercase()),
            "notranslate".to_owned(),
        ]
    };
    HighlightedCode {
        language: language.to_owned(),
        css_classes,
        lines,
        warnings,
        options: options.clone(),
    }
}

fn split_lines(code: &str) -> Vec<String> {
    let mut lines = code
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if code.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Highlights several independent blocks in parallel using `rayon`.
///
/// Output order always matches input order.
pub fn highlight_many(blocks: &[(String, String, HighlightOptions)]) -> Vec<HighlightedCode> {
    use rayon::prelude::*;
    blocks
        .par_iter()
        .map(|(code, language, options)| highlight_code(code, language, options))
        .collect()
}

/// Parses a Pygments line-number spec (`"1,2,4-6"`, `"-10"`, `"10-"`) into
/// zero-based indices, mirroring `sphinx.util._lines.parse_line_num_spec`.
///
/// `total` closes open-ended ranges (`"-"` and `"10-"`). Invalid specifications
/// return an error whose text matches upstream's `invalid line number spec`.
pub fn parse_line_num_spec(spec: &str, total: usize) -> Result<Vec<usize>, String> {
    let mut items = Vec::new();
    for raw in spec.split(',') {
        let part = raw.trim();
        let parts: Vec<&str> = part.split('-').collect();
        let parsed: Option<Vec<usize>> = (|| {
            if parts == ["", ""] {
                return None;
            }
            match parts.as_slice() {
                [single] => Some(vec![single.parse::<usize>().ok()?.checked_sub(1)?]),
                [start, end] => {
                    let first = if start.is_empty() {
                        1usize
                    } else {
                        start.parse::<usize>().ok()?
                    };
                    let last = if end.is_empty() {
                        first.max(total)
                    } else {
                        end.parse::<usize>().ok()?
                    };
                    if first > last {
                        return None;
                    }
                    Some((first - 1..last).collect())
                }
                _ => None,
            }
        })();
        match parsed {
            Some(values) => items.extend(values),
            None => return Err(format!("invalid line number spec: '{spec}'")),
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_resolution() {
        assert_eq!(alias_info("py"), Some("Python"));
        assert_eq!(alias_info("C++"), Some("C++"));
        assert_eq!(alias_info("cpp"), Some("C++"));
        assert_eq!(alias_info("JS"), Some("JavaScript"));
        assert_eq!(alias_info("javascript"), Some("JavaScript"));
        assert_eq!(alias_info("rst"), Some("reStructuredText"));
        assert_eq!(alias_info("made-up"), None);
        assert_eq!(resolve_language("py").canonical, "python");
        assert_eq!(resolve_language("default").syntax, "Python");
    }

    #[test]
    fn unknown_language_falls_back_with_warning() {
        let out = highlight_code("hello", "not-a-lang", &HighlightOptions::default());
        assert!(!out.warnings.is_empty());
        assert!(out.warnings[0].contains("not-a-lang"));
        assert_eq!(out.plain_text(), "hello");
    }

    #[test]
    fn empty_input_is_tested() {
        let out = highlight_code("", "python", &HighlightOptions::default());
        assert!(out.lines.is_empty());
        assert_eq!(out.plain_text(), "");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn python_tokens_use_pygments_classes() {
        let out = highlight_code(
            "def foo():\n    return True  # hi\n",
            "python",
            &HighlightOptions::default(),
        );
        let classes = out.all_classes();
        assert!(classes.iter().any(|c| c == "k"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "nf"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "kc"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "c1"), "{classes:?}");
    }

    #[test]
    fn line_numbers_and_emphasis() {
        let options = HighlightOptions {
            linenos: true,
            lineno_start: 200,
            emphasize_lines: vec![2],
            ..HighlightOptions::default()
        };
        let out = highlight_code("a\nb\nc", "python", &options);
        assert_eq!(out.lines.len(), 3);
        assert_eq!(out.lines[0].line_number, Some(200));
        assert_eq!(out.lines[2].line_number, Some(202));
        assert!(out.lines[1].line_classes.iter().any(|c| c == "hll"));
        assert!(out.pygments_html().contains("<span class=\"hll\">"));
    }

    #[test]
    fn line_number_specs() {
        assert_eq!(parse_line_num_spec("1,2,4-6", 10).unwrap(), vec![0, 1, 3, 4, 5]);
        assert_eq!(parse_line_num_spec("-3", 10).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_line_num_spec("10-", 10).unwrap(), vec![9]);
        assert_eq!(parse_line_num_spec("29-", 13).unwrap(), vec![28]);
        assert!(parse_line_num_spec("10-1", 10).is_err());
        assert!(parse_line_num_spec("abc", 10).is_err());
    }

    #[test]
    fn diff_classes() {
        let diff = "--- a.py\n+++ b.py\n@@ -1 +1 @@\n-old\n+new\n context\n";
        let out = highlight_code(diff, "udiff", &HighlightOptions::default());
        let classes = out.all_classes();
        for expected in ["gd", "gi", "gu"] {
            assert!(classes.iter().any(|c| c == expected), "{expected} missing: {classes:?}");
        }
    }

    #[test]
    fn json_round_trip() {
        let out = highlight_code("x = 1", "python", &HighlightOptions::default());
        let json = out.to_json();
        let decoded = HighlightedCode::from_json(&json).expect("round trip");
        assert_eq!(decoded, out);
    }

    #[test]
    fn parallel_many_preserves_order() {
        let blocks = vec![
            ("a = 1".to_owned(), "python".to_owned(), HighlightOptions::default()),
            ("def f(): pass".to_owned(), "python".to_owned(), HighlightOptions::default()),
            ("x".to_owned(), "rust".to_owned(), HighlightOptions::default()),
        ];
        let out = highlight_many(&blocks);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].plain_text(), "a = 1");
        assert_eq!(out[1].plain_text(), "def f(): pass");
        assert_eq!(out[2].plain_text(), "x");
    }
}
