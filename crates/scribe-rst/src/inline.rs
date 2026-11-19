//! Full inline-markup recognition (Phase 05).
//!
//! This module ports the rules of Docutils' `parsers/rst/states.py::Inliner`
//! (Docutils 0.23) and Sphinx's `roles.py` / `util/docutils.py::ReferenceRole`
//! into an allocation-light, byte-span-accurate scanner. It deliberately does
//! **not** recurse into the content of emphasis, strong or literal nodes:
//! upstream does not either (`inline_obj()` stores the inner text verbatim).
//!
//! # Span model
//!
//! [`InlineSource`] owns the text to scan plus a parallel table mapping every
//! byte position in that text to an absolute byte offset in the original
//! source. Multi-line paragraphs join their lines with `\n`; the offset table
//! makes those joins transparent, so every emitted [`InlineNode`] carries an
//! absolute [`Span`] into the original (possibly prolog/epilog-augmented)
//! source. Escaped characters (`\*`) emit only the escaped character but keep
//! a span covering the original backslash sequence.
//!
//! # Purity
//!
//! [`parse_inline`] is a pure function over `&InlineSource`; it never touches a
//! [`Document`](scribe_ast::Document) and never panics. Materialisation into
//! arena nodes happens in [`materialize_inline`], which is the only place that
//! can fail (and only on an invalid parent id). This split makes the recogniser
//! directly fuzzable with `proptest`.

use std::collections::HashMap;

use scribe_ast::{AttributeValue, Document, Node, NodeId, NodeKind, Span};

use crate::InlineContext;

/// Source-anchored text handed to the inline recogniser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSource {
    /// The text to scan (possibly several source lines joined with `\n`).
    pub text: String,
    /// `offsets[i]` is the absolute source byte offset of `text`'s byte `i`.
    /// Its length is exactly `text.len() + 1` so end-exclusive slices work.
    pub offsets: Vec<u32>,
}

impl InlineSource {
    /// Builds a source with an explicit offset table.
    ///
    /// `offsets.len()` must equal `text.len() + 1`; callers should prefer the
    /// checked constructors below, which build a correct table themselves.
    pub fn new(text: String, offsets: Vec<u32>) -> Self {
        debug_assert_eq!(offsets.len(), text.len() + 1);
        Self { text, offsets }
    }

    /// A single contiguous slice of source starting at `abs_start`.
    pub fn single(text: &str, abs_start: u32) -> Self {
        let mut offsets = Vec::with_capacity(text.len() + 1);
        for i in 0..=text.len() {
            offsets.push(abs_start.saturating_add(i as u32));
        }
        Self {
            text: text.to_owned(),
            offsets,
        }
    }

    /// Joins several source lines with `\n`, preserving each line's absolute
    /// byte offset. Each tuple is `(line_text, absolute_start_of_line_text)`.
    pub fn from_lines(parts: &[(String, u32)]) -> Self {
        let total: usize = parts.iter().map(|(t, _)| t.len()).sum::<usize>()
            + parts.len().saturating_sub(1);
        let mut text = String::with_capacity(total);
        let mut offsets = Vec::with_capacity(total + 1);
        for (idx, (line, abs_start)) in parts.iter().enumerate() {
            if idx > 0 {
                // The '\n' we insert corresponds to the source EOL immediately
                // after the previous line's content.
                let prev_end = parts[idx - 1].1.saturating_add(parts[idx - 1].0.len() as u32);
                text.push('\n');
                offsets.push(prev_end);
            }
            for i in 0..line.len() {
                offsets.push(abs_start.saturating_add(i as u32));
            }
            text.push_str(line);
        }
        // The one-past-the-end entry is the end of the final line.
        if let Some((last, abs_start)) = parts.last() {
            offsets.push(abs_start.saturating_add(last.len() as u32));
        } else {
            offsets.push(0);
        }
        debug_assert_eq!(offsets.len(), text.len() + 1, "offset table length mismatch");
        Self { text, offsets }
    }

    /// Returns a sub-view covering `text[start..end]`, preserving absolute
    /// source offsets. `start`/`end` are byte indices into `text`.
    pub fn slice(&self, start: usize, end: usize) -> Self {
        let n = self.offsets.len().saturating_sub(1);
        let mut s = start.min(n);
        let mut e = end.min(n).max(s);
        // Never slice through a UTF-8 code point, even if a caller passes a
        // byte estimate derived from a trimmed/rejoined string.
        while s > 0 && !self.text.is_char_boundary(s) {
            s -= 1;
        }
        while e < self.text.len() && !self.text.is_char_boundary(e) {
            e += 1;
        }
        Self {
            text: self.text[s..e].to_owned(),
            offsets: self.offsets[s..=e].to_vec(),
        }
    }

    /// Absolute byte span of `text[start..end]` (byte indices).
    ///
    /// The offset table is *not* guaranteed monotonic: `from_lines` maps each
    /// line's bytes at its original `abs_start`, and a tab-expanded line can be
    /// longer than its source slice, so a later line may map to a smaller
    /// absolute offset than the preceding one. Taking the min/max over the
    /// covered table range (instead of just the two endpoints) keeps every span
    /// well-formed and makes containment structural — for token ranges
    /// `[a, b) ⊆ [c, d)` we always get `abs_span(a,b) ⊆ abs_span(c,d)`. For a
    /// monotonic table this is identical to using the two endpoint offsets, so
    /// all normal (non-tab-expanded) spans are unchanged.
    pub fn abs_span(&self, start: usize, end: usize) -> Span {
        let len = self.offsets.len().saturating_sub(1);
        let s = start.min(len);
        let e = end.min(len).max(s);
        let mut lo = self.offsets[s];
        let mut hi = lo;
        for &off in &self.offsets[s..=e] {
            if off < lo {
                lo = off;
            }
            if off > hi {
                hi = off;
            }
        }
        Span::new(lo, hi)
    }
}

/// A detached inline node produced by the recogniser.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineNode {
    pub kind: NodeKind,
    /// Content of a `Text` leaf; `None` for elements.
    pub text: Option<String>,
    /// Element attributes (sorted when materialised into the arena).
    pub attrs: Vec<(String, AttributeValue)>,
    pub children: Vec<InlineNode>,
    /// Absolute source byte span.
    pub span: Span,
    /// 1-based source line (inherited from the enclosing text).
    pub line: u32,
}

impl InlineNode {
    fn text_node(text: String, span: Span, line: u32) -> Self {
        Self {
            kind: NodeKind::Text,
            text: Some(text),
            attrs: Vec::new(),
            children: Vec::new(),
            span,
            line,
        }
    }

    fn element(kind: NodeKind, span: Span, line: u32) -> Self {
        Self {
            kind,
            text: None,
            attrs: Vec::new(),
            children: Vec::new(),
            span,
            line,
        }
    }

    fn with_attr(mut self, key: &str, value: impl Into<AttributeValue>) -> Self {
        self.attrs.push((key.to_owned(), value.into()));
        self
    }

    fn with_child(mut self, child: InlineNode) -> Self {
        self.children.push(child);
        self
    }

    fn with_text_child(self, text: String, span: Span, line: u32) -> Self {
        self.with_child(InlineNode::text_node(text, span, line))
    }
}

/// A recoverable inline diagnostic (e.g. an unmatched start-string).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineWarning {
    pub message: String,
    pub span: Span,
}

/// Result of a pure inline parse.
#[derive(Clone, Debug, Default)]
pub struct InlineParseResult {
    pub nodes: Vec<InlineNode>,
    pub warnings: Vec<InlineWarning>,
}

/// Extra contract information for a `pending_xref` produced by a role.
///
/// Stored in a side table on `ParseOutput` so the pseudo-XML surface stays
/// clean; consumed by Phases 06/11 when domains resolve cross references.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct XrefMeta {
    /// Full role name as written, e.g. `py:func` or `ref`.
    pub role: String,
    /// Raw payload between the backticks.
    pub raw_payload: String,
    /// Parsed (and modifier-adjusted) target.
    pub target: String,
    /// Absolute span of the target inside the payload.
    pub target_span: Span,
    /// Absolute span of the explicit/implicit title.
    pub title_span: Span,
    /// Whether the payload used `Title <target>`.
    pub explicit_title: bool,
    /// Whether a `~` / `.` modifier was present.
    pub specific: bool,
}

/// Options for inline parsing.
#[derive(Clone, Debug, Default)]
pub struct InlineOptions {
    /// Document name, used for `refdoc` on `pending_xref` nodes.
    pub source_name: Option<String>,
    /// Dynamic external-link roles registered by `sphinx.ext.extlinks`.
    pub extlinks: crate::extlinks::ExtLinkRoles,
    /// Role selected by a `.. default-role::` declaration (prolog or body).
    pub default_role: Option<String>,
    /// Custom roles declared by `.. role:: name(base)` in document order.
    /// Maps the local role name to its base role name. The base role supplies
    /// the emitted node kind, while the custom name supplies the `classes`
    /// attribute (matching Docutils' `roles.CustomRole`).
    pub custom_roles: HashMap<String, String>,
}

// ----- tokenizer ------------------------------------------------------------

#[derive(Clone, Debug)]
struct Tok {
    ch: char,
    /// Byte index in `InlineSource::text` where this token starts.
    start: usize,
    /// Byte index in `InlineSource::text` just past this token.
    end: usize,
    /// `true` when the character was backslash-escaped in the source.
    escaped: bool,
}

fn tokenize(src: &InlineSource) -> Vec<Tok> {
    let text = src.text.as_bytes();
    let mut tokens = Vec::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        if text[i] == b'\\' {
            if i + 1 < text.len() {
                // `escape2null` consumes one Unicode character after the
                // backslash and turns the pair into <NUL><char>.
                let next = src.text[i + 1..].chars().next().unwrap_or('\0');
                let next_len = next.len_utf8();
                tokens.push(Tok {
                    ch: next,
                    start: i,
                    end: i + 1 + next_len,
                    escaped: true,
                });
                i += 1 + next_len;
            } else {
                // Trailing backslash: <NUL> with no following character.
                tokens.push(Tok {
                    ch: '\0',
                    start: i,
                    end: i + 1,
                    escaped: true,
                });
                i += 1;
            }
        } else {
            let ch = src.text[i..].chars().next().unwrap_or('\0');
            tokens.push(Tok {
                ch,
                start: i,
                end: i + ch.len_utf8(),
                escaped: false,
            });
            i += ch.len_utf8();
        }
    }
    tokens
}

/// Emission mode for text slices: escape backslashes are dropped (normal text)
/// or restored (inline literals and role raw sources).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unescape {
    Drop,
    Restore,
}

fn text_of(tokens: &[Tok], a: usize, b: usize, mode: Unescape) -> String {
    // Adversarial/adornment-heavy inputs can make a recogniser hand us an
    // upper bound past the token slice. Clamp rather than panic; this is a
    // no-op for every in-bounds call.
    let a = a.min(tokens.len());
    let b = b.min(tokens.len());
    if a >= b {
        return String::new();
    }
    let mut out = String::new();
    for tok in &tokens[a..b] {
        if tok.ch == '\0' {
            if mode == Unescape::Restore {
                out.push('\\');
            }
            continue;
        }
        if tok.escaped && mode == Unescape::Restore {
            out.push('\\');
        }
        out.push(tok.ch);
    }
    out
}

fn span_of(src: &InlineSource, tokens: &[Tok], a: usize, b: usize) -> Span {
    // Clamp both ends: several recognisers compute `b` as `i + delimiter_len`
    // or `close + offset`, which can exceed `tokens.len()` for a payload whose
    // closing delimiter was consumed by an earlier rule. Previously that
    // panicked under proptest; clamping keeps every emitted span inside the
    // source while leaving all in-bounds calls unchanged.
    let a = a.min(tokens.len());
    let b = b.min(tokens.len());
    if a >= b {
        // Empty / synthesised content (e.g. the text child of an ``x`` payload
        // whose closing delimiter starts at `body`, so `body == end`). Anchor
        // the zero-width span at the *construct's* position, never at the
        // global byte 0: a child whose base span starts past 0 would otherwise
        // get `Span { 0, 0 }`, which lies outside its parent and is out of
        // order. `tokens[a]` is inside `[a, b)`'s enclosing construct, so this
        // offset is always a valid position within the parent's span.
        let at = if a < tokens.len() {
            tokens[a].start
        } else {
            tokens.last().map_or(0, |t| t.end)
        };
        return src.abs_span(at, at);
    }
    src.abs_span(tokens[a].start, tokens[b - 1].end)
}

// ----- boundary rules (Docutils punctuation_chars) --------------------------

const OPENERS: &str = "\"'(<[{\u{0f3a}\u{0f3c}\u{169b}\u{2045}\u{207d}\u{208d}\u{2329}\u{2768}\u{276a}\u{276c}\u{276e}\u{2770}\u{2772}\u{2774}\u{27c5}\u{27e6}\u{27e8}\u{27ea}\u{27ec}\u{27ee}\u{2983}\u{2985}\u{2987}\u{2989}\u{298b}\u{298d}\u{298f}\u{2991}\u{2993}\u{2995}\u{2997}\u{29d8}\u{29da}\u{29fc}\u{2e22}\u{2e24}\u{2e26}\u{2e28}\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}\u{3014}\u{3016}\u{3018}\u{301a}\u{301d}\u{fd3e}\u{fe17}\u{fe35}\u{fe37}\u{fe39}\u{fe3b}\u{fe3d}\u{fe3f}\u{fe41}\u{fe43}\u{fe47}\u{fe59}\u{fe5b}\u{fe5d}\u{ff08}\u{ff3b}\u{ff5b}\u{ff5f}\u{ff62}\u{ab}\u{2018}\u{201c}\u{2039}\u{2e02}\u{2e04}\u{2e09}\u{2e0c}\u{2e1c}\u{2e20}\u{201a}\u{201e}\u{bb}\u{2019}\u{201d}\u{203a}\u{2e03}\u{2e05}\u{2e0a}\u{2e0d}\u{2e1d}\u{2e21}\u{201b}\u{201f}";

const CLOSERS: &str = "\"')>\\]}\u{0f3b}\u{0f3d}\u{169c}\u{2046}\u{207e}\u{208e}\u{232a}\u{2769}\u{276b}\u{276d}\u{276f}\u{2771}\u{2773}\u{2775}\u{27c6}\u{27e7}\u{27e9}\u{27eb}\u{27ed}\u{27ef}\u{2984}\u{2986}\u{2988}\u{298a}\u{298c}\u{298e}\u{2990}\u{2992}\u{2994}\u{2996}\u{2998}\u{29d9}\u{29db}\u{29fd}\u{2e23}\u{2e25}\u{2e27}\u{2e29}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\u{3015}\u{3017}\u{3019}\u{301b}\u{301e}\u{301f}\u{fd3f}\u{fe18}\u{fe36}\u{fe38}\u{fe3a}\u{fe3c}\u{fe3e}\u{fe40}\u{fe42}\u{fe44}\u{fe48}\u{fe5a}\u{fe5c}\u{fe5e}\u{ff09}\u{ff3d}\u{ff5d}\u{ff60}\u{ff63}\u{bb}\u{2019}\u{201d}\u{203a}\u{2e03}\u{2e05}\u{2e0a}\u{2e0d}\u{2e1d}\u{2e21}\u{201b}\u{201f}\u{ab}\u{2018}\u{201c}\u{2039}\u{2e02}\u{2e04}\u{2e09}\u{2e0c}\u{2e1c}\u{2e20}\u{201a}\u{201e}";

const DELIMITERS: &str = "\\-/:\u{058a}\u{a1}\u{b7}\u{bf}\u{037e}\u{0387}\u{055a}\u{055b}\u{055c}\u{055d}\u{055e}\u{055f}\u{0589}\u{05be}\u{05c0}\u{05c3}\u{05c6}\u{05f3}\u{05f4}\u{0609}\u{060a}\u{060c}\u{060d}\u{061b}\u{061e}\u{061f}\u{066a}\u{066b}\u{066c}\u{066d}\u{06d4}\u{2010}\u{2011}\u{2012}\u{2013}\u{2014}\u{2015}\u{2016}\u{2017}\u{2020}\u{2021}\u{2022}\u{2023}\u{2024}\u{2025}\u{2026}\u{2027}\u{2030}\u{2031}\u{2032}\u{2033}\u{2034}\u{2035}\u{2036}\u{2037}\u{2038}\u{203b}\u{203c}\u{203d}\u{203e}\u{2041}\u{2042}\u{2043}\u{2047}\u{2048}\u{2049}\u{2050}\u{2051}\u{2052}\u{2053}\u{2055}\u{2056}\u{2057}\u{2058}\u{2059}\u{205a}\u{205b}\u{205c}\u{205d}\u{205e}\u{3001}\u{3002}\u{3003}\u{301c}\u{3030}\u{303d}\u{30a0}\u{30fb}\u{fe10}\u{fe11}\u{fe12}\u{fe13}\u{fe14}\u{fe15}\u{fe16}\u{fe19}\u{fe30}\u{fe31}\u{fe32}\u{fe45}\u{fe46}\u{fe49}\u{fe4a}\u{fe4b}\u{fe4c}\u{fe50}\u{fe51}\u{fe52}\u{fe54}\u{fe55}\u{fe56}\u{fe57}\u{fe58}\u{fe59}\u{fe5a}\u{fe5b}\u{fe5c}\u{fe5d}\u{fe5e}\u{fe5f}\u{fe60}\u{fe61}\u{fe63}\u{fe68}\u{fe6a}\u{fe6b}\u{ff01}\u{ff02}\u{ff03}\u{ff05}\u{ff06}\u{ff07}\u{ff0a}\u{ff0c}\u{ff0d}\u{ff0e}\u{ff0f}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff20}\u{ff3c}\u{ff61}\u{ff64}\u{ff65}";

fn is_opener(c: char) -> bool {
    OPENERS.contains(c)
}
fn is_closer(c: char) -> bool {
    CLOSERS.contains(c)
}
fn is_delimiter(c: char) -> bool {
    DELIMITERS.contains(c) || matches!(c, '\u{2010}'..='\u{2017}')
}

/// `(^|(?<=\s|[openers delimiters]))` — a start-string may follow whitespace,
/// an opening bracket/quote or a delimiter.
fn start_prefix_ok(tokens: &[Tok], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = tokens[i - 1].ch;
    prev.is_whitespace() || is_opener(prev) || is_delimiter(prev)
}

/// `($|(?=\s|[\ \.,;!? delimiters closers]))` — an end-string must be followed
/// by whitespace, a closing delimiter, punctuation or a closer.
fn end_suffix_ok(tokens: &[Tok], j: usize) -> bool {
    if j >= tokens.len() {
        return true;
    }
    let next = tokens[j].ch;
    next.is_whitespace()
        || matches!(next, '\\' | '.' | ',' | ';' | '!' | '?')
        || is_delimiter(next)
        || is_closer(next)
}

/// Non-whitespace before an end-string (`(?<![\s\x00])`). An escaped character
/// counts as non-whitespace here (upstream's `\x00` is not `\s`).
fn non_ws_before(tokens: &[Tok], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    !tokens[i - 1].ch.is_whitespace() && tokens[i - 1].ch != '\0'
}

/// `non_whitespace_before` (`(?<!\s)`) used by inline literals.
fn non_ws_before_plain(tokens: &[Tok], i: usize) -> bool {
    i > 0 && !tokens[i - 1].ch.is_whitespace()
}

/// Docutils `quoted_start`: skip a start-string enclosed in matching quotes.
fn quoted_start(tokens: &[Tok], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let Some(post) = tokens.get(i + 1) else {
        return true;
    };
    let pre = tokens[i - 1].ch;
    let post = post.ch;
    if let Some(pos) = OPENERS.find(pre) {
        let close = CLOSERS.chars().nth(OPENERS[..pos].chars().count()).unwrap_or('\0');
        if post == close || matches!((pre, post), ('\u{ab}', '\u{ab}') | ('\u{2019}', '\u{2019}') | ('\u{201d}', '\u{201d}') | ('\u{203a}', '\u{203a}')) {
            return true;
        }
    }
    false
}

// ----- simplename / URI helpers ---------------------------------------------

/// Length (in tokens) of a Docutils `simplename` starting at `i`, or `None`.
/// Pattern: `(?:(?!_)\w)+(?:[-._+:](?:(?!_)\w)+)*`.
fn simplename_len(tokens: &[Tok], i: usize) -> Option<usize> {
    let mut p = i;
    let mut word = 0usize;
    while p < tokens.len() {
        let c = tokens[p].ch;
        if c == '_' || !is_word(c) || tokens[p].escaped {
            break;
        }
        word += 1;
        p += 1;
    }
    if word == 0 {
        return None;
    }
    loop {
        if p >= tokens.len()
            || !matches!(tokens[p].ch, '-' | '.' | '_' | '+' | ':')
            || tokens[p].escaped
        {
            break;
        }
        let sep = p;
        p += 1;
        let mut w = 0usize;
        while p < tokens.len() {
            let c = tokens[p].ch;
            if c == '_' || !is_word(c) || tokens[p].escaped {
                break;
            }
            w += 1;
            p += 1;
        }
        if w == 0 {
            p = sep;
            break;
        }
    }
    Some(p - i)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Recognised URI schemes (from `docutils.utils.urischemes.schemes`).
const URI_SCHEMES: &[&str] = &[
    "about", "acap", "addbook", "afp", "afs", "aim", "callto", "castanet", "chttp", "cid",
    "crid", "data", "dav", "dict", "dns", "eid", "fax", "feed", "file", "finger", "freenet",
    "ftp", "go", "gopher", "gsm-sms", "h323", "h324", "hdl", "hnews", "http", "https", "hydra",
    "iioploc", "ilu", "im", "imap", "info", "ior", "ipp", "irc", "iris.beep", "iseek", "jar",
    "javascript", "jdbc", "ldap", "lifn", "livescript", "lrq", "mailbox", "mailserver", "mailto",
    "md5", "mid", "mocha", "modem", "mtqp", "mupdate", "news", "nfs", "nntp", "opaquelocktoken",
    "phone", "pop", "pop3", "pres", "printer", "prospero", "rdar", "res", "rtsp", "rvp",
    "rwhois", "rx", "sdp", "service", "shttp", "sip", "sips", "smb", "snews", "snmp", "soap.beep",
    "soap.beeps", "ssh", "t120", "tag", "tcp", "tel", "telephone", "telnet", "tftp", "tip",
    "tn3270", "tv", "urn", "uuid", "vemmi", "videotex", "view-source", "wais", "whodp", "whois++",
    "x-man-page", "xmlrpc.beep", "xmlrpc.beeps", "z39.50r", "z39.50s",
];

fn is_uric(c: char) -> bool {
    // Docutils splits `?query` / `#fragment` out of `uric`; we accept them as
    // internal characters and rely on `uri_end`/trailing-punctuation trimming
    // to avoid swallowing final punctuation.
    matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')' | '[' | ']' | ';' | '/' | ':' | '@' | '&' | '=' | '+' | '$' | ',' | '%' | '?' | '#')
        || c.is_ascii_alphanumeric()
}
fn is_urilast(c: char) -> bool {
    matches!(c, '_' | '~' | '*' | '/' | '=' | '+' | 'a'..='z' | 'A'..='Z' | '0'..='9')
}
fn is_emailc(c: char) -> bool {
    matches!(c, '-' | '_' | '!' | '~' | '*' | '\'' | '{' | '}' | '|' | '/' | '#' | '?' | '^' | '`' | '&' | '=' | '+' | '$' | '%')
        || c.is_ascii_alphanumeric()
}

// ----- implicit inline (standalone URIs / emails) ---------------------------

fn parse_implicit(
    tokens: &[Tok],
    base: usize,
    src: &InlineSource,
    line: u32,
    out: &mut Vec<InlineNode>,
) {
    let mut i = 0usize;
    let mut text_start = 0usize;
    while i < tokens.len() {
        if let Some(UriMatch { consumed, refuri }) = match_uri(tokens, i) {
            if text_start < i {
                out.push(InlineNode::text_node(
                    text_of(tokens, text_start, i, Unescape::Drop),
                    span_of(src, tokens, text_start, i),
                    line,
                ));
            }
            let span = span_of(src, tokens, i, i + consumed);
            let raw = text_of(tokens, i, i + consumed, Unescape::Drop);
            let mut reference = InlineNode::element(NodeKind::Reference, span, line)
                .with_attr("refuri", refuri);
            reference = reference.with_text_child(raw, span, line);
            out.push(reference);
            i += consumed;
            text_start = i;
        } else {
            i += 1;
        }
    }
    if text_start < tokens.len() {
        out.push(InlineNode::text_node(
            text_of(tokens, text_start, tokens.len(), Unescape::Drop),
            span_of(src, tokens, text_start, tokens.len()),
            line,
        ));
    }
    let _ = base;
}

struct UriMatch {
    consumed: usize,
    refuri: String,
}

/// Matches a standalone URI or email starting at token `i`.
fn match_uri(tokens: &[Tok], i: usize) -> Option<UriMatch> {
    if !start_prefix_ok(tokens, i) || tokens[i].escaped {
        return None;
    }
    // Try scheme + ':' first.
    let mut p = i;
    if tokens[p].ch.is_ascii_alphabetic() {
        let mut scheme = String::new();
        while p < tokens.len()
            && (tokens[p].ch.is_ascii_alphanumeric()
                || matches!(tokens[p].ch, '.' | '+' | '-'))
            && !tokens[p].escaped
        {
            scheme.push(tokens[p].ch);
            p += 1;
        }
        if p < tokens.len() && tokens[p].ch == ':' && !tokens[p].escaped {
            if !scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                return None;
            }
            if !URI_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
                return None;
            }
            p += 1; // consume ':'
            return match_uri_body(tokens, i, p, false);
        }
    }
    // No scheme: try an email address.
    match_email(tokens, i)
}

fn match_uri_body(tokens: &[Tok], start: usize, mut p: usize, _email: bool) -> Option<UriMatch> {
    let body_start = p;
    while p < tokens.len() {
        let t = &tokens[p];
        if t.ch == '>' && t.escaped {
            p += 1;
            continue;
        }
        if (is_uric(t.ch) || t.ch == '\0') && !t.escaped {
            p += 1;
            continue;
        }
        if t.escaped {
            // <NUL> in URI: allowed as part of uric.
            p += 1;
            continue;
        }
        break;
    }
    if p == body_start {
        return None;
    }
    // Trim trailing punctuation that cannot end a URI (unless before '>').
    while p > body_start {
        let c = tokens[p - 1].ch;
        if is_urilast(c) {
            break;
        }
        if p < tokens.len() && tokens[p].ch == '>' {
            break;
        }
        p -= 1;
    }
    if p == body_start {
        return None;
    }
    if !end_suffix_ok(tokens, p) {
        return None;
    }
    let text = text_of(tokens, start, p, Unescape::Drop);
    Some(UriMatch {
        consumed: p - start,
        refuri: text,
    })
}

fn match_email(tokens: &[Tok], i: usize) -> Option<UriMatch> {
    // `emailc+ (?:\.emailc+)*` permits dots between non-empty components.
    let mut p = i;
    while p < tokens.len()
        && (is_emailc(tokens[p].ch) || tokens[p].ch == '.' || tokens[p].escaped)
    {
        p += 1;
    }
    while p > i && tokens[p - 1].ch == '.' {
        p -= 1;
    }
    let at = p;
    if at == i || at >= tokens.len() || tokens[at].ch != '@' || tokens[at].escaped {
        return None;
    }
    p = at + 1;
    let host_start = p;
    while p < tokens.len()
        && (is_emailc(tokens[p].ch) || tokens[p].ch == '.' || tokens[p].escaped)
    {
        p += 1;
    }
    while p > host_start && tokens[p - 1].ch == '.' {
        p -= 1;
    }
    if p == host_start {
        return None;
    }
    while p > host_start && !is_urilast(tokens[p - 1].ch) {
        p -= 1;
    }
    if p == host_start || !end_suffix_ok(tokens, p) {
        return None;
    }
    let local = text_of(tokens, i, at, Unescape::Drop);
    let host = text_of(tokens, at + 1, p, Unescape::Drop);
    Some(UriMatch {
        consumed: p - i,
        refuri: format!("mailto:{local}@{host}"),
    })
}

// ----- role classification --------------------------------------------------

use crate::role_registry::{classify_builtin, RoleClass};

/// Classifies a role through the Phase 06 registry, with a fallback for
/// fully-qualified `domain:role` spellings (`:std:ref:`, `:py:func:`, ...).
fn classify_role(role: &str) -> Option<RoleClass> {
    if let Some(class) = classify_builtin(role) {
        return Some(class);
    }
    let (domain, name) = role.split_once(':')?;
    if role.contains("::")
        || !matches!(domain, "std" | "py" | "rst" | "c" | "cpp" | "js" | "math")
    {
        return None;
    }
    let RoleClass::Xref { reftype, inner, warn, .. } = classify_builtin(name)? else {
        return None;
    };
    Some(RoleClass::Xref {
        domain: domain.to_owned(),
        reftype,
        inner,
        warn,
    })
}

/// Parsed explicit-title payload (`Title <target>`).
struct ExplicitTitle {
    title: String,
    target: String,
}

fn split_explicit_title(payload: &str) -> Option<ExplicitTitle> {
    // Mirrors `ReferenceRole.explicit_title_re = ^(.+?)\s*(?<!\x00)<(.*?)>$`
    // with DOTALL. We find the last unescaped '<' with a matching final '>'.
    let bytes = payload.as_bytes();
    if bytes.last() != Some(&b'>') {
        return None;
    }
    let mut lt: Option<usize> = None;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'<' {
            lt = Some(i);
        }
        i += 1;
    }
    let lt = lt?;
    if lt == 0 {
        return None;
    }
    let title_raw = &payload[..lt];
    if title_raw.trim().is_empty() {
        return None;
    }
    let title = title_raw.trim_end();
    let target = &payload[lt + 1..payload.len() - 1];
    Some(ExplicitTitle {
        title: title.to_owned(),
        target: target.to_owned(),
    })
}

/// Applies the `~` / `.` modifiers from Sphinx's `PyXRefRole.process_link`.
fn apply_modifiers(title: &mut String, target: &mut String, specific: &mut bool) {
    let t = title.trim_start_matches('.');
    *title = t.to_owned();
    let tgt = target.trim_start_matches('~').to_owned();
    *target = tgt;
    if title.starts_with('~') {
        let rest = &title[1..];
        *title = rest.rsplit_once('.').map_or(rest, |(_, last)| last).to_owned();
    }
    if target.starts_with('.') {
        *target = target[1..].to_owned();
        *specific = true;
    }
}

// ----- the scanner ----------------------------------------------------------

/// Runs the inline recogniser over `source`.
pub fn parse_inline(source: &InlineSource, options: &InlineOptions) -> InlineParseResult {
    let tokens = tokenize(source);
    let mut result = InlineParseResult::default();
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut run_start = 0usize;

    while pos < tokens.len() {
        let mut buf = Vec::new();
        let consumed = try_construct(
            &tokens,
            pos,
            source,
            options,
            &mut result.warnings,
            &mut buf,
        );
        if let Some(next) = consumed {
            // Flush the unprocessed text *before* the construct first; the
            // recogniser appends construct nodes to `buf` independently.
            if run_start < pos {
                parse_implicit(&tokens[run_start..pos], run_start, source, 0, &mut out);
            }
            out.append(&mut buf);
            pos = next;
            run_start = pos;
        } else {
            pos += 1;
        }
    }
    if run_start < tokens.len() {
        parse_implicit(&tokens[run_start..], run_start, source, 0, &mut out);
    }
    // Fix up line numbers: inherit the enclosing text's line for every node.
    // The block parser passes this through `materialize_inline`; zero here is
    // replaced there. We keep a deterministic placeholder so tests are stable.
    result.nodes = out;
    result
}

/// Attempts to recognise a construct starting at token `i`. Returns the token
/// index just past the construct on success, appending emitted nodes to `out`.
#[allow(clippy::too_many_arguments)]
fn try_construct(
    tokens: &[Tok],
    i: usize,
    src: &InlineSource,
    options: &InlineOptions,
    warnings: &mut Vec<InlineWarning>,
    out: &mut Vec<InlineNode>,
) -> Option<usize> {
    // Backslash-escaped characters can never start markup.
    if tokens[i].escaped {
        return None;
    }
    let c = tokens[i].ch;

    // strong: **text**
    if c == '*' && tokens.get(i + 1).is_some_and(|t| t.ch == '*' && !t.escaped)
        && start_prefix_ok(tokens, i) && !quoted_start(tokens, i)
    {
        let body = i + 2;
        if body < tokens.len() && !tokens[body].ch.is_whitespace() {
            if let Some(end) = find_delimiter(tokens, body, "**", NwsMode::EscapeAware) {
                if non_ws_before(tokens, end) && end_suffix_ok(tokens, end + 2) {
                    let span = span_of(src, tokens, i, end + 2);
                    let text = text_of(tokens, body, end, Unescape::Drop);
                    let node = InlineNode::element(NodeKind::Strong, span, 0)
                        .with_text_child(text, span_of(src, tokens, body, end), 0);
                    out.push(node);
                    return Some(end + 2);
                }
            }
            // Unmatched start-string: emit a problematic node (upstream does
            // the same) and consume only the delimiter so scanning progresses.
            push_unmatched(warnings, src, tokens, i, i + 2, "strong");
            let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, i + 2), 0)
                .with_text_child("**".to_owned(), span_of(src, tokens, i, i + 2), 0);
            out.push(node);
            return Some(i + 2);
        }
    }

    // emphasis: *text* (but not **)
    if c == '*' && !tokens.get(i + 1).is_some_and(|t| t.ch == '*')
        && start_prefix_ok(tokens, i) && !quoted_start(tokens, i)
    {
        let body = i + 1;
        if body < tokens.len() && !tokens[body].ch.is_whitespace() {
            if let Some(end) = find_delimiter(tokens, body, "*", NwsMode::EscapeAware) {
                if non_ws_before(tokens, end) && end_suffix_ok(tokens, end + 1) {
                    let span = span_of(src, tokens, i, end + 1);
                    let text = text_of(tokens, body, end, Unescape::Drop);
                    let node = InlineNode::element(NodeKind::Emphasis, span, 0)
                        .with_text_child(text, span_of(src, tokens, body, end), 0);
                    out.push(node);
                    return Some(end + 1);
                }
            }
            push_unmatched(warnings, src, tokens, i, i + 1, "emphasis");
            let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, i + 1), 0)
                .with_text_child("*".to_owned(), span_of(src, tokens, i, i + 1), 0);
            out.push(node);
            return Some(i + 1);
        }
    }

    // inline literal: ``text``
    if c == '`' && tokens.get(i + 1).is_some_and(|t| t.ch == '`' && !t.escaped)
        && start_prefix_ok(tokens, i) && !quoted_start(tokens, i)
    {
        let body = i + 2;
        if body < tokens.len() && !tokens[body].ch.is_whitespace() {
            if let Some(end) = find_delimiter(tokens, body, "``", NwsMode::Plain) {
                if non_ws_before_plain(tokens, end) && end_suffix_ok(tokens, end + 2) {
                    let span = span_of(src, tokens, i, end + 2);
                    // Inline literals restore backslashes (`unescape(text, True)`).
                    let text = text_of(tokens, body, end, Unescape::Restore);
                    let node = InlineNode::element(NodeKind::Literal, span, 0)
                        .with_text_child(text, span_of(src, tokens, body, end), 0);
                    out.push(node);
                    return Some(end + 2);
                }
            }
            push_unmatched(warnings, src, tokens, i, i + 2, "literal");
            let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, i + 2), 0)
                .with_text_child("``".to_owned(), span_of(src, tokens, i, i + 2), 0);
            out.push(node);
            return Some(i + 2);
        }
    }

    // footnote / citation reference: [label]_
    if c == '[' && start_prefix_ok(tokens, i) && !quoted_start(tokens, i) {
        if let Some(next) = try_footnote(tokens, i, src, out) {
            return Some(next);
        }
    }

    // interpreted text / phrase reference, optionally with a role prefix.
    if c == '`' || (c == ':' && role_prefix_len(tokens, i).is_some()) {
        if let Some(next) = try_interpreted(tokens, i, src, options, warnings, out) {
            return Some(next);
        }
    }

    // inline internal target: _`name`
    if c == '_' && tokens.get(i + 1).is_some_and(|t| t.ch == '`' && !t.escaped)
        && start_prefix_ok(tokens, i)
    {
        if let Some(end) = find_closing_backtick(tokens, i + 2, &[]) {
            let inner = text_of(tokens, i + 2, end, Unescape::Drop);
            let name = crate::names::normalize_name(&inner);
            let span = span_of(src, tokens, i, end + 1);
            let mut node = InlineNode::element(NodeKind::Target, span, 0)
                .with_attr("names", vec![name]);
            node = node.with_text_child(inner, span_of(src, tokens, i + 2, end), 0);
            out.push(node);
            return Some(end + 1);
        }
    }

    // substitution reference: |name| (optionally |name|_ / |name|__)
    if c == '|' && !tokens.get(i + 1).is_some_and(|t| t.ch == '|')
        && start_prefix_ok(tokens, i) && !quoted_start(tokens, i)
    {
        let body = i + 1;
        if body < tokens.len() && !tokens[body].ch.is_whitespace() {
            if let Some(end) = find_delimiter(tokens, body, "|", NwsMode::EscapeAware) {
                if non_ws_before(tokens, end) && end_suffix_ok(tokens, end + 1) {
                    let inner_span = span_of(src, tokens, body, end);
                    let inner_text = text_of(tokens, body, end, Unescape::Drop);
                    let name = crate::names::normalize_name(&inner_text);
                    let sub = InlineNode::element(NodeKind::SubstitutionReference, inner_span, 0)
                        .with_attr("refname", name)
                        .with_text_child(inner_text, inner_span, 0);
                    let mut after = end + 1;
                    if after < tokens.len() && tokens[after].ch == '_' && !tokens[after].escaped {
                        after += 1;
                        if after < tokens.len() && tokens[after].ch == '_' && !tokens[after].escaped {
                            after += 1;
                        }
                        let ref_span = span_of(src, tokens, i, after);
                        let mut reference = InlineNode::element(NodeKind::Reference, ref_span, 0)
                            .with_attr("anonymous", true);
                        if !tokens[end + 1..after].iter().all(|t| t.ch == '_') {
                            // Unreachable: the slice only contains underscores.
                        }
                        // Named `|name|_`: refname is the substitution name.
                        let refname = crate::names::normalize_name(
                            &text_of(tokens, body, end, Unescape::Drop),
                        );
                        reference = reference.with_attr("refname", refname);
                        reference = reference.with_child(sub);
                        out.push(reference);
                        return Some(after);
                    }
                    out.push(sub);
                    return Some(end + 1);
                }
            }
            push_unmatched(warnings, src, tokens, i, i + 1, "substitution reference");
            let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, i + 1), 0)
                .with_text_child("|".to_owned(), span_of(src, tokens, i, i + 1), 0);
            out.push(node);
            return Some(i + 1);
        }
    }

    // simple reference name: name_ / name__
    if simplename_len(tokens, i).is_some() && start_prefix_ok(tokens, i) && !quoted_start(tokens, i) {
        if let Some(next) = try_simple_reference(tokens, i, src, out) {
            return Some(next);
        }
    }

    None
}

fn push_unmatched(
    warnings: &mut Vec<InlineWarning>,
    src: &InlineSource,
    tokens: &[Tok],
    a: usize,
    b: usize,
    what: &str,
) {
    warnings.push(InlineWarning {
        message: format!("Inline {what} start-string without end-string."),
        span: span_of(src, tokens, a, b),
    });
}

/// Finds the first occurrence of `delim` at or after `from` satisfying basic
/// boundary conditions. `require_nws` uses the escaped-aware lookbehind.
#[derive(Clone, Copy)]
enum NwsMode {
    EscapeAware,
    Plain,
}

/// Finds the first *valid* closing delimiter at or after `from`. A candidate
/// must satisfy both the lookbehind (non-whitespace immediately before) and
/// the lookahead (end-string suffix rule), mirroring Docutils' end patterns.
fn find_delimiter(
    tokens: &[Tok],
    from: usize,
    delim: &str,
    nws: NwsMode,
) -> Option<usize> {
    let chars: Vec<char> = delim.chars().collect();
    let n = chars.len();
    let mut p = from;
    while p + n <= tokens.len() {
        let mut ok = true;
        for (k, ch) in chars.iter().enumerate() {
            if tokens[p + k].ch != *ch || tokens[p + k].escaped {
                ok = false;
                break;
            }
        }
        if ok
            && match nws {
                NwsMode::EscapeAware => non_ws_before(tokens, p),
                NwsMode::Plain => non_ws_before_plain(tokens, p),
            }
            && end_suffix_ok(tokens, p + n)
        {
            return Some(p);
        }
        p += 1;
    }
    None
}

fn find_closing_backtick(tokens: &[Tok], from: usize, suffix: &[Tok]) -> Option<usize> {
    let mut p = from;
    while p < tokens.len() {
        if tokens[p].ch == '`' && !tokens[p].escaped && p > from {
            let after = p + 1 + suffix.len();
            if end_suffix_ok(tokens, after) {
                return Some(p);
            }
        }
        p += 1;
    }
    None
}

fn try_footnote(
    tokens: &[Tok],
    i: usize,
    src: &InlineSource,
    out: &mut Vec<InlineNode>,
) -> Option<usize> {
    let mut p = i + 1;
    let label_start = p;
    // [0-9]+ | #simplename? | * | citation simplename
    if p < tokens.len() && tokens[p].ch == '#' && !tokens[p].escaped {
        p += 1;
        if let Some(n) = simplename_len(tokens, p) {
            p += n;
        }
    } else if p < tokens.len() && tokens[p].ch == '*' && !tokens[p].escaped {
        p += 1;
    } else {
        p += simplename_len(tokens, p)?;
    }
    if p == label_start || p + 1 >= tokens.len() {
        return None;
    }
    if tokens[p].ch != ']' || tokens[p].escaped
        || tokens[p + 1].ch != '_' || tokens[p + 1].escaped
    {
        return None;
    }
    if !end_suffix_ok(tokens, p + 2) {
        return None;
    }
    let label = text_of(tokens, label_start, p, Unescape::Drop);
    let is_number = label.bytes().all(|b| b.is_ascii_digit());
    let is_auto = label.starts_with('#');
    let is_symbol = label == "*";
    let is_citation = !is_number && !is_auto && !is_symbol;
    let span = span_of(src, tokens, i, p + 2);
    let label_span = span_of(src, tokens, label_start, p);

    if is_citation {
        let name = crate::names::normalize_name(&label);
        let node = InlineNode::element(NodeKind::CitationReference, span, 0)
            .with_attr("refname", name)
            .with_text_child(label, label_span, 0);
        out.push(node);
    } else {
        let refname = if is_auto {
            label.trim_start_matches('#').to_owned()
        } else if is_symbol {
            String::new()
        } else {
            label.clone()
        };
        let mut node = InlineNode::element(NodeKind::FootnoteReference, span, 0);
        if is_auto {
            node = node.with_attr("auto", "1");
        }
        if is_symbol {
            node = node.with_attr("auto", "*");
        }
        if !refname.is_empty() {
            node = node.with_attr("refname", crate::names::normalize_name(&refname));
        }
        node = node.with_text_child(label, label_span, 0);
        out.push(node);
    }
    Some(p + 2)
}

fn try_simple_reference(
    tokens: &[Tok],
    i: usize,
    src: &InlineSource,
    out: &mut Vec<InlineNode>,
) -> Option<usize> {
    let n = simplename_len(tokens, i)?;
    let p = i + n;
    if p >= tokens.len() || tokens[p].ch != '_' || tokens[p].escaped {
        return None;
    }
    let mut after = p + 1;
    let anonymous = if after < tokens.len() && tokens[after].ch == '_' && !tokens[after].escaped {
        after += 1;
        true
    } else {
        false
    };
    if !end_suffix_ok(tokens, after) {
        return None;
    }
    let name = text_of(tokens, i, p, Unescape::Drop);
    let refname = crate::names::normalize_name(&name);
    let span = span_of(src, tokens, i, after);
    let mut node = InlineNode::element(NodeKind::Reference, span, 0)
        .with_attr("name", crate::names::whitespace_normalize_name(&name));
    if anonymous {
        node = node.with_attr("anonymous", true);
    } else {
        node = node.with_attr("refname", refname);
    }
    node = node.with_text_child(name, span_of(src, tokens, i, p), 0);
    out.push(node);
    Some(after)
}

/// Length in tokens of a `:role:` prefix starting at token `i`, including the
/// backtick that follows it. `simplename` permits isolated internal `:`
/// separators, so `:py:func:` is one role name (not `:py:` + `func`).
fn role_prefix_len(tokens: &[Tok], i: usize) -> Option<usize> {
    if i >= tokens.len() || tokens[i].ch != ':' || tokens[i].escaped {
        return None;
    }
    let mut p = i + 1;
    let mut word = false;
    let mut sep = false;
    while p < tokens.len() {
        let token = &tokens[p];
        if token.ch == ':' && !token.escaped {
            if p + 1 < tokens.len()
                && tokens[p + 1].ch == '`'
                && !tokens[p + 1].escaped
                && word
            {
                return Some(p + 1 - i);
            }
            if sep || p == i + 1 {
                return None; // doubled or leading separator
            }
            sep = true;
        } else if token.ch.is_alphanumeric() || (token.ch == '_' && !sep) {
            if token.escaped {
                return None;
            }
            word = true;
            sep = false;
        } else if matches!(token.ch, '-' | '+' | '.') && !token.escaped {
            if sep {
                return None;
            }
            sep = true;
        } else {
            return None;
        }
        p += 1;
    }
    None
}

fn try_interpreted(
    tokens: &[Tok],
    i: usize,
    src: &InlineSource,
    options: &InlineOptions,
    warnings: &mut Vec<InlineWarning>,
    out: &mut Vec<InlineNode>,
) -> Option<usize> {
    let (b, role) = if tokens[i].ch == '`' {
        if start_prefix_ok(tokens, i) && quoted_start(tokens, i) {
            return None;
        }
        (i, None)
    } else {
        let plen = role_prefix_len(tokens, i)?;
        let b = i + plen; // index of the backtick
        if !start_prefix_ok(tokens, i) {
            return None;
        }
        // `plen` spans `:name:` plus the backtick, so the role name is the
        // tokens between the leading ':' and the trailing ':' of the prefix.
        (b, Some(text_of(tokens, i + 1, b - 1, Unescape::Drop)))
    };
    let body = b + 1;
    if body >= tokens.len() || tokens[body].ch.is_whitespace() {
        return None;
    }

    // Find the closing backtick; after it there may be a `:suffix-role:` and
    // an optional `__?` reference marker.
    let mut p = body;
    let mut close = None;
    while p < tokens.len() {
        if tokens[p].ch == '`' && !tokens[p].escaped && p > body {
            let mut after = p + 1;
            let mut suffix_role: Option<String> = None;
            if after < tokens.len() && tokens[after].ch == ':' && !tokens[after].escaped {
                // A suffix role is `:simplename:` (no backtick follows it).
                let s = after + 1;
                let mut q = s;
                while q < tokens.len() && !tokens[q].escaped {
                    let c = tokens[q].ch;
                    if is_word(c) || matches!(c, '-' | '+' | '.') {
                        q += 1;
                    } else if c == ':' && q + 1 < tokens.len() && is_word(tokens[q + 1].ch) {
                        q += 1; // isolated internal separator in a simplename
                    } else {
                        break;
                    }
                }
                if q < tokens.len() && tokens[q].ch == ':' && q > s {
                    suffix_role = Some(text_of(tokens, s, q, Unescape::Drop));
                    after = q + 1;
                }
            }
            let mut refend = 0usize;
            if after < tokens.len() && tokens[after].ch == '_' && !tokens[after].escaped {
                refend += 1;
                if after + 1 < tokens.len() && tokens[after + 1].ch == '_' && !tokens[after + 1].escaped {
                    refend += 1;
                }
            }
            if end_suffix_ok(tokens, after + refend) {
                close = Some((p, suffix_role, refend, after + refend));
                break;
            }
        }
        p += 1;
    }
    let Some((close_tick, suffix_role, refend, end)) = close else {
        push_unmatched(warnings, src, tokens, b, b + 1, "interpreted text");
        let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, b, b + 1), 0)
            .with_text_child("`".to_owned(), span_of(src, tokens, b, b + 1), 0);
        out.push(node);
        return Some(b + 1);
    };

    if suffix_role.is_some() && role.is_some() {
        // Multiple roles: upstream emits a problematic node.
        push_unmatched(warnings, src, tokens, i, end, "interpreted text (multiple roles)");
        let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, end), 0)
            .with_text_child(text_of(tokens, i, end, Unescape::Drop), span_of(src, tokens, i, end), 0);
        out.push(node);
        return Some(end);
    }

    let payload = text_of(tokens, body, close_tick, Unescape::Restore);
    let payload_span = span_of(src, tokens, body, close_tick);
    let role_name = role
        .clone()
        .or_else(|| suffix_role.clone())
        .or_else(|| options.default_role.clone())
        .unwrap_or_else(|| "title-reference".to_owned());

    if refend > 0 {
        // Phrase reference: `text`_ / `text`__ (possibly with embedded target).
        if role.is_some() || suffix_role.is_some() {
            push_unmatched(warnings, src, tokens, i, end, "interpreted text (role + reference suffix)");
            let node = InlineNode::element(NodeKind::Problematic, span_of(src, tokens, i, end), 0)
                .with_text_child(text_of(tokens, i, end, Unescape::Drop), span_of(src, tokens, i, end), 0);
            out.push(node);
            return Some(end);
        }
        emit_phrase_reference(tokens, body, close_tick, refend, end, src, out);
        return Some(end);
    }

    emit_role(&role_name, &payload, payload_span, span_of(src, tokens, i, end), src, options, out);
    Some(end)
}

fn emit_phrase_reference(
    tokens: &[Tok],
    body: usize,
    close_tick: usize,
    refend: usize,
    end: usize,
    src: &InlineSource,
    out: &mut Vec<InlineNode>,
) {
    let raw = text_of(tokens, body, close_tick, Unescape::Drop);
    // Embedded `<...>` link: `Title <uri>`_ or `Title <alias_>`_.
    //
    // Locate the delimiters at the TOKEN level, never by byte-searching the
    // rendered `raw` string. `raw` is built with `Unescape::Drop`: a multi-byte
    // character contributes several bytes but exactly one token, and escaped
    // characters lose their backslash. Using `raw.rfind('<').len()` as a token
    // index (as this code once did) therefore overshoots for Unicode titles and
    // produced a child span past its parent. Tokens also let us honour escapes:
    // docutils' `escape2null` makes an escaped `<`/`>` ineligible as a delimiter.
    let gt = close_tick.saturating_sub(1);
    let angle_gt = gt >= body && tokens[gt].ch == '>' && !tokens[gt].escaped;
    let angle_lt = if angle_gt {
        (body..gt)
            .rev()
            .find(|&p| tokens[p].ch == '<' && !tokens[p].escaped)
    } else {
        None
    };
    let full_span = span_of(src, tokens, body, end);
    let reference_span = span_of(src, tokens, body, close_tick + 1 + refend);
    if let Some(lt) = angle_lt {
        let title_raw = text_of(tokens, body, lt, Unescape::Drop);
        let title = title_raw.trim_end();
        let target_raw = text_of(tokens, lt + 1, gt, Unescape::Drop);
        let target = target_raw.trim();
        // `title` is `raw[..lt].trim_end()`. Trim trailing whitespace *tokens*
        // so the child span ends exactly at the last title token; `title_end`
        // is a token index, computed from tokens only (never a byte length).
        let mut title_end = lt;
        while title_end > body && tokens[title_end - 1].ch.is_whitespace() {
            title_end -= 1;
        }
        let mut reference = InlineNode::element(NodeKind::Reference, reference_span, 0)
            .with_attr("name", crate::names::whitespace_normalize_name(title));
        let target_name = crate::names::normalize_name(target.trim_end_matches('_'));
        if refend == 2 || target.starts_with("http://") || target.starts_with("https://")
            || target.contains("://") || target.starts_with("mailto:")
        {
            let mut uri = target.to_owned();
            if !uri.contains(':') && is_emailish(uri.as_str()) {
                uri = format!("mailto:{uri}");
            }
            reference = reference.with_attr("refuri", uri);
        } else if target.ends_with('_') {
            reference = reference.with_attr("refname", target_name);
        } else {
            reference = reference.with_attr("refuri", target.to_owned());
        }
        reference = reference.with_child(InlineNode::text_node(
            title.to_owned(),
            span_of(src, tokens, body, title_end),
            0,
        ));
        out.push(reference);
        // The implicit target node that docutils appends beside the reference.
        let tgt_span = span_of(src, tokens, body, end);
        let normalized_title = crate::names::normalize_name(title);
        let mut target_node = InlineNode::element(NodeKind::Target, tgt_span, 0)
            .with_attr("ids", vec![crate::names::make_id(&normalized_title)])
            .with_attr("names", vec![normalized_title]);
        if target.contains("://") || target.starts_with("mailto:") {
            target_node = target_node.with_attr("refuri", target.to_owned());
        }
        out.push(target_node);
        let _ = full_span;
    } else {
        let name = crate::names::whitespace_normalize_name(&raw);
        let refname = crate::names::normalize_name(&raw);
        let mut reference = InlineNode::element(NodeKind::Reference, reference_span, 0)
            .with_attr("name", name);
        if refend == 2 {
            reference = reference.with_attr("anonymous", true);
        } else {
            reference = reference.with_attr("refname", refname);
        }
        reference = reference.with_child(InlineNode::text_node(
            raw,
            span_of(src, tokens, body, close_tick),
            0,
        ));
        out.push(reference);
    }
}

fn is_emailish(s: &str) -> bool {
    s.contains('@') && !s.contains(' ')
}

fn emit_role(
    role: &str,
    payload: &str,
    payload_span: Span,
    full_span: Span,
    _src: &InlineSource,
    options: &InlineOptions,
    out: &mut Vec<InlineNode>,
) {
    let out_start = out.len();

    // Resolve a custom role declared by `.. role:: name(base)` in document
    // order. Docutils' `roles.CustomRole` delegates node construction to the
    // base role but overrides the `classes` attribute with the custom name.
    let base = options.custom_roles.get(&role.to_ascii_lowercase());
    let custom_class = base.map(|_| role.to_owned());
    let effective_role = base.map_or(role, String::as_str);

    // A custom role with no explicit base uses Docutils' generic custom role:
    // a plain `inline` node classed with the custom role name.
    if base.is_some() && effective_role == "generic" {
        let mut node = InlineNode::element(NodeKind::Inline, full_span, 0)
            .with_text_child(payload.to_owned(), payload_span, 0);
        if let Some(name) = custom_class {
            node = node.with_attr("classes", vec![name]);
        }
        out.push(node);
        return;
    }

    // `sphinx.ext.extlinks` registers roles dynamically from configuration.
    // They take precedence over the static role table and produce a plain
    // external `Reference` (never a `pending_xref`).
    if let Some(spec) = options.extlinks.get(effective_role) {
        let explicit = split_explicit_title(payload);
        let target = explicit.as_ref().map_or(payload, |e| e.target.as_str());
        let (uri, implicit_title) = spec.resolve(target);
        let title = explicit.as_ref().map_or(implicit_title, |e| e.title.clone());
        let mut reference = InlineNode::element(NodeKind::Reference, full_span, 0)
            .with_attr("refuri", uri)
            .with_attr("internal", false)
            .with_attr("classes", vec![format!("extlink-{effective_role}")]);
        reference = reference.with_child(InlineNode::element(NodeKind::Strong, payload_span, 0)
            .with_child(InlineNode::text_node(title, payload_span, 0)));
        out.push(reference);
        apply_custom_class(out, out_start, custom_class);
        return;
    }

    let Some(class) = classify_role(effective_role) else {
        // Unknown role → problematic node, matching docutils.
        let mut node = InlineNode::element(NodeKind::Problematic, full_span, 0);
        node = node.with_child(InlineNode::text_node(payload.to_owned(), payload_span, 0));
        out.push(node);
        return;
    };

    match class {
        RoleClass::Direct { kind, classes, behavior, .. } => {
            emit_direct_role(effective_role, kind, classes, behavior, payload, payload_span, full_span, out);
        }
        RoleClass::External { prefix, title_prefix, classes } => {
            let explicit = split_explicit_title(payload);
            let target = explicit.as_ref().map_or(payload, |e| &e.target);
            let number = target.trim_start_matches('#').split('#').next().unwrap_or(target);
            let is_pep = effective_role == "pep";
            let is_rfc = effective_role == "rfc";
            let uri = if is_pep {
                format!("{prefix}{:0>4}/", number.parse::<u32>().unwrap_or(0))
            } else {
                format!("{prefix}{number}")
            };
            let title = explicit.as_ref().map_or_else(
                || format!("{title_prefix}{}", crate::names::whitespace_normalize_name(target)),
                |e| e.title.clone(),
            );
            // Sphinx's `sphinx.roles.PEP`/`RFC` are `ReferenceRole`s: alongside
            // the reference they emit an `index` entry and an explicit
            // `target` carrying a generated `index-N` id. Emit them in the
            // same order so the pseudoxml doctree matches the oracle.
            if is_pep || is_rfc {
                let serial = out.iter().filter(|n| n.kind == NodeKind::Index).count();
                let index_id = format!("index-{serial}");
                let label = if is_pep {
                    format!("Python Enhancement Proposals; PEP {target}")
                } else {
                    format!("RFC; RFC {target}")
                };
                let entries = format!("['single', '{label}', '{index_id}', '', None]");
                out.push(InlineNode::element(NodeKind::Index, full_span, 0)
                    .with_attr("entries", entries));
                out.push(InlineNode::element(NodeKind::Target, full_span, 0)
                    .with_attr("ids", vec![index_id]));
            }
            let mut reference = InlineNode::element(NodeKind::Reference, full_span, 0)
                .with_attr("refuri", uri)
                .with_attr("internal", false)
                .with_attr("classes", classes);
            let strong = InlineNode::element(NodeKind::Strong, payload_span, 0)
                .with_child(InlineNode::text_node(title, payload_span, 0));
            reference = reference.with_child(strong);
            out.push(reference);
        }
        RoleClass::Xref { domain, reftype, inner, warn } => {
            emit_xref_role(
                effective_role, &domain, &reftype, inner, warn, payload, payload_span, full_span, options, out,
            );
        }
    }

    apply_custom_class(out, out_start, custom_class);
}

/// Merges the custom role's name into the `classes` attribute of the element(s)
/// emitted by its base role, mirroring Docutils' `CustomRole`.
fn apply_custom_class(out: &mut [InlineNode], from: usize, custom_class: Option<String>) {
    let Some(class_name) = custom_class else { return };
    for node in &mut out[from..] {
        // Text leaves and the index/target carried by reference roles are not
        // the class-bearing element; leave them untouched.
        if node.text.is_some() || matches!(node.kind, NodeKind::Index | NodeKind::Target) {
            continue;
        }
        // Docutils' `CustomRole` merges the base role's default classes with
        // the custom name (e.g. `.. role:: shiny(code)` → `code shiny`).
        let mut classes = node
            .attrs
            .iter()
            .find(|(key, _)| key == "classes")
            .and_then(|(_, value)| match value {
                AttributeValue::List(values) => Some(values.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if !classes.iter().any(|c| c == &class_name) {
            classes.push(class_name.clone());
        }
        node.attrs.retain(|(key, _)| key != "classes");
        node.attrs
            .push(("classes".to_owned(), AttributeValue::List(classes)));
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_direct_role(
    role: &str,
    kind: NodeKind,
    classes: Vec<String>,
    behavior: crate::role_registry::DirectBehavior,
    payload: &str,
    payload_span: Span,
    full_span: Span,
    out: &mut Vec<InlineNode>,
) {
    use crate::role_registry::DirectBehavior;

    if behavior == DirectBehavior::Keyboard {
        let parts = split_keyboard(payload);
        let mut index = 0usize;
        for (piece, is_separator) in parts {
            let len = piece.len() as u32;
            let span = span_from_local(payload_span, index, len as usize);
            if is_separator {
                out.push(InlineNode::text_node(piece, span, 0));
            } else {
                let mut node = InlineNode::element(kind, span, 0);
                if !classes.is_empty() {
                    node = node.with_attr("classes", classes.clone());
                }
                node = node.with_child(InlineNode::text_node(piece.clone(), span, 0));
                out.push(node);
            }
            index += len as usize;
        }
        return;
    }

    let mut node = InlineNode::element(kind, full_span, 0);
    if !classes.is_empty() {
        node = node.with_attr("classes", classes);
    }
    let mut text = payload.to_owned();
    match behavior {
        DirectBehavior::Abbreviation => {
            if let Some(index) = payload.rfind(" (") {
                if payload.ends_with(')') {
                    node = node.with_attr(
                        "explanation",
                        payload[index + 2..payload.len() - 1].to_owned(),
                    );
                    text = payload[..index].to_owned();
                }
            }
        }
        DirectBehavior::MenuSelection => text = payload.replace("-->", "\u{25b8}"),
        DirectBehavior::Manpage => {
            let mut page = payload.to_owned();
            let mut section = String::new();
            if let Some(open) = payload.rfind(['(', '[']) {
                if payload.ends_with([')', ']']) {
                    page = payload[..open].trim_end_matches(['.', ' ']).to_owned();
                    section = payload[open + 1..payload.len() - 1].to_owned();
                }
            }
            node = node.with_attr("path", payload.to_owned());
            node = node.with_attr("page", page);
            node = node.with_attr("section", section);
        }
        DirectBehavior::Keyboard | DirectBehavior::Plain => {}
    }
    if role == "file" || role == "samp" {
        node = node.with_attr("role", role.to_owned());
    }
    node = node.with_child(InlineNode::text_node(text, payload_span, 0));
    out.push(node);
}

/// Splits `Ctrl-C` into `("Ctrl", key)`, `("-", separator)`, `("C", key)`.
fn split_keyboard(payload: &str) -> Vec<(String, bool)> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut separator = String::new();
    for ch in payload.chars() {
        if matches!(ch, '-' | '+' | '^') {
            if !current.is_empty() {
                parts.push((std::mem::take(&mut current), false));
            }
            separator.push(ch);
        } else {
            if !separator.is_empty() {
                parts.push((std::mem::take(&mut separator), true));
            }
            current.push(ch);
        }
    }
    if !current.is_empty() {
        parts.push((current, false));
    }
    if !separator.is_empty() {
        parts.push((separator, true));
    }
    parts
}

#[allow(clippy::too_many_arguments)]
fn emit_xref_role(
    role: &str,
    domain: &str,
    reftype: &str,
    inner: crate::role_registry::InnerKind,
    warn: bool,
    payload: &str,
    payload_span: Span,
    full_span: Span,
    options: &InlineOptions,
    out: &mut Vec<InlineNode>,
) {
    // Split explicit title and apply modifiers.
    let explicit = split_explicit_title(payload);
    let mut title = explicit.as_ref().map_or_else(|| payload.to_owned(), |e| e.title.clone());
    let mut target = explicit.as_ref().map_or_else(|| payload.to_owned(), |e| e.target.clone());
    // Upstream's `TokenXRefRole.process_link` overrides the generic modifiers:
    // an implicit leading `~` drops the `domain:` prefix from the displayed
    // token, while an explicit title keeps its literal `~`.
    let token_tilde_implicit = reftype == "token" && explicit.is_none() && title.starts_with('~');
    let token_tilde_explicit = reftype == "token" && explicit.is_some() && title.starts_with('~');
    let original_token_title = title.clone();
    let mut specific = false;
    apply_modifiers(&mut title, &mut target, &mut specific);
    if token_tilde_implicit {
        if let Some((_, bare)) = title.split_once(':') {
            title = bare.to_owned();
        }
    } else if token_tilde_explicit {
        title = original_token_title;
    }

    // Compute spans of title/target within the payload. After `~`/`.` modifier
    // processing the displayed title and normalized target are substrings of
    // the original payload; a reverse search gives the most specific match.
    let locate = |needle: &str| -> (usize, usize) {
        if needle.is_empty() {
            return (0, 0);
        }
        match payload.rfind(needle) {
            Some(pos) => (pos, pos + needle.len()),
            None => (0, needle.len().min(payload.len())),
        }
    };
    let (t0, t1) = locate(&title);
    let (g0, g1) = locate(&target);
    let title_span = span_from_local(payload_span, t0, t1.saturating_sub(t0));
    let target_span = span_from_local(payload_span, g0, g1.saturating_sub(g0));

    let classes = if domain.is_empty() {
        vec!["xref".to_owned(), reftype.to_owned()]
    } else {
        vec![
            "xref".to_owned(),
            domain.to_owned(),
            format!("{domain}-{reftype}"),
        ]
    };
    let mut node = InlineNode::element(NodeKind::PendingXref, full_span, 0)
        .with_attr("refdomain", domain.to_owned())
        .with_attr("reftype", reftype.to_owned())
        .with_attr("reftarget", crate::names::whitespace_normalize_name(&target))
        .with_attr("refexplicit", explicit.is_some())
        .with_attr("refspecific", specific)
        .with_attr("refwarn", warn);
    if let Some(doc) = options.source_name.as_deref() {
        node = node.with_attr("refdoc", doc.to_owned());
    }
    node = node.with_attr("classes", classes);
    let inner_node = InlineNode::element(inner.node_kind(), title_span, 0)
        .with_child(InlineNode::text_node(title.clone(), title_span, 0));
    node = node.with_child(inner_node);
    out.push(node);

    let meta = XrefMeta {
        role: role.to_owned(),
        raw_payload: payload.to_owned(),
        target,
        target_span,
        title_span,
        explicit_title: explicit.is_some(),
        specific,
    };
    out.push(InlineNode::element(NodeKind::Generated, full_span, 0)
        .with_attr("__scribe_xref_meta", meta_to_json(&meta)));
}

fn span_from_local(base: Span, local_start: usize, local_len: usize) -> Span {
    let start = base.start.saturating_add(local_start.min(base.len() as usize) as u32);
    let end = start.saturating_add(local_len.min((base.end - start) as usize) as u32);
    Span::new(start.min(base.end), end.min(base.end))
}

fn meta_to_json(meta: &XrefMeta) -> String {
    serde_json::to_string(meta).unwrap_or_default()
}

// ----- materialisation ------------------------------------------------------

/// Node ids plus the xref contract side table returned by materialisation.
pub type MaterializedInline = (Vec<NodeId>, Vec<(NodeId, XrefMeta)>);

/// Debug-only invariant for the materialisation path: every emitted child span
/// must be well-formed and contained in its parent. Checking here (rather than
/// only in the property tests) catches a span regression at its source: any
/// future byte-length-vs-token-index mix-up will trip an in-debug-build assert
/// instead of silently emitting an out-of-parent `Text` node. This never panics
/// in release builds and adds no `.unwrap()`/`.expect()` to production code.
#[inline]
fn checked_child_span(parent: Span, child: Span) {
    debug_assert!(
        child.start <= child.end,
        "inverted inline child span {child:?} under parent {parent:?}"
    );
    debug_assert!(
        child.start >= parent.start && child.end <= parent.end,
        "inline child span {child:?} outside parent {parent:?}"
    );
}

/// Materialises detached inline nodes into arena nodes under `parent`.
///
/// Returns `(node ids in document order, collected xref metadata keyed by the
/// pending_xref node id)`. Meta sentinel nodes (`NodeKind::Generated` carrying
/// `__scribe_xref_meta`) are consumed here and never enter the arena.
pub fn materialize_inline(
    doc: &mut Document,
    nodes: &[InlineNode],
    parent: NodeId,
    ctx: &InlineContext,
) -> Result<MaterializedInline, scribe_ast::AstError> {
    let mut ids = Vec::new();
    let mut xrefs = Vec::new();
    for (idx, node) in nodes.iter().enumerate() {
        if node.kind == NodeKind::Generated
            && node.attrs.iter().any(|(k, _)| k == "__scribe_xref_meta")
        {
            // Attach the meta to the immediately preceding pending_xref.
            if let Some(&xref_id) = ids.last() {
                if doc.get(xref_id).is_ok_and(|n| n.kind == NodeKind::PendingXref) {
                    if let Some(json) = node.attrs.iter().find(|(k, _)| k == "__scribe_xref_meta")
                        .and_then(|(_, v)| match v { AttributeValue::Str(s) => Some(s.clone()), _ => None })
                    {
                        if let Some(meta) = meta_from_json(&json, ctx.source_name.as_deref()) {
                            xrefs.push((xref_id, meta));
                        }
                    }
                }
            }
            let _ = idx;
            continue;
        }
        let id = materialize_one(doc, node, parent, ctx)?;
        ids.push(id);
    }
    Ok((ids, xrefs))
}

fn materialize_one(
    doc: &mut Document,
    node: &InlineNode,
    parent: NodeId,
    ctx: &InlineContext,
) -> Result<NodeId, scribe_ast::AstError> {
    let mut arena = if node.kind == NodeKind::Text {
        scribe_ast::Node::text(node.text.clone().unwrap_or_default())
    } else {
        Node::new(node.kind)
    };
    for (key, value) in &node.attrs {
        arena.attrs.insert(key.clone(), value.clone());
    }
    arena.span = Some(node.span);
    let mut source = scribe_ast::SourcePos::new(
        ctx.source_name.clone(),
        Some(if node.line == 0 { ctx.line } else { node.line }),
        Some(node.span.start),
    );
    let _ = &mut source;
    arena.source = Some(source);
    let id = doc.add_node(arena, Some(parent))?;
    for child in &node.children {
        checked_child_span(node.span, child.span);
        materialize_one(doc, child, id, ctx)?;
    }
    Ok(id)
}

fn meta_from_json(json: &str, _source_name: Option<&str>) -> Option<XrefMeta> {
    serde_json::from_str(json).ok()
}

/// Clears inherited line numbers on a parsed node tree (all nodes use the
/// enclosing context's line unless they are multi-line, which the block
/// parser handles by leaving them at zero).
pub fn set_line(nodes: &mut [InlineNode], line: u32) {
    for node in nodes {
        node.line = line;
        set_line(&mut node.children, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> InlineParseResult {
        parse_inline(&InlineSource::single(text, 100), &InlineOptions::default())
    }

    fn tags(result: &InlineParseResult) -> Vec<&'static str> {
        result.nodes.iter().map(|n| n.kind.tag_name()).collect()
    }

    #[test]
    fn emphasis_and_strong() {
        assert_eq!(tags(&parse("*em* **strong**")), vec!["emphasis", "#text", "strong"]);
    }

    #[test]
    fn literal_preserves_backslash_and_spaces() {
        let r = parse("``a  b``");
        assert_eq!(tags(&r), vec!["literal"]);
        assert_eq!(r.nodes[0].children[0].text.as_deref(), Some("a  b"));
    }

    #[test]
    fn escaped_delimiters_are_literal() {
        let r = parse(r"\*not\* \`not\`");
        assert_eq!(tags(&r), vec!["#text"]);
        assert_eq!(r.nodes[0].text.as_deref(), Some("*not* `not`"));
    }

    #[test]
    fn spans_are_absolute() {
        let r = parse("*em*");
        assert_eq!(r.nodes[0].span, Span::new(100, 104));
        assert_eq!(r.nodes[0].children[0].span, Span::new(101, 103));
    }

    #[test]
    fn math_role() {
        let r = parse(":math:`E=mc^2`");
        assert_eq!(tags(&r), vec!["math"]);
    }

    #[test]
    fn standalone_uri() {
        let r = parse("see https://example.com/x now");
        assert_eq!(tags(&r), vec!["#text", "reference", "#text"]);
        assert_eq!(r.nodes[1].attrs[0].1, AttributeValue::Str("https://example.com/x".into()));
    }

    #[test]
    fn footnote_and_citation() {
        let expected: Vec<&str> = vec![
            "footnote_reference", "#text", "footnote_reference", "#text",
            "footnote_reference", "#text", "citation_reference",
        ];
        assert_eq!(tags(&parse("[1]_ [#]_ [*]_ [CIT]_")), expected);
    }

    /// Regression for the Phase 05 fuzz failure: an inline literal whose
    /// closing `` starts at `body` yields empty text. The synthesised text
    /// child must carry a zero-width span clamped inside the literal, never
    /// the global `Span { 0, 0 }`.
    #[test]
    fn empty_literal_child_span_is_clamped_to_parent() {
        let r = parse("||[````]");
        let literal = r.nodes.iter().find(|n| n.kind == NodeKind::Literal).unwrap();
        assert_eq!(literal.span, Span::new(103, 107)); // base offset 100 + [3, 7)
        let child = &literal.children[0];
        assert_eq!(child.span, Span::new(105, 105));
        assert!(child.span.start >= literal.span.start && child.span.end <= literal.span.end);
    }
}
