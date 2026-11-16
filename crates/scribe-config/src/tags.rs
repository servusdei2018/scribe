//! Boolean tag-expression evaluator matching `sphinx.util.tags.Tags`.
//!
//! Upstream builds a restricted Jinja parser (`BooleanParser`) that only
//! allows conditional expressions and the `and` / `or` / `not` operators over
//! tag names and parentheses. This module implements the same grammar with a
//! hand-written tokenizer and recursive-descent parser — no Python `eval`, no
//! dependency on Jinja, and no dynamic code execution.
//!
//! # Fidelity note on constants
//!
//! Upstream's grammar *recognises* `true` / `True`, `false` / `False` and
//! `none` / `None` as Jinja `Const` nodes, but `Tags._eval_node` has no `Const`
//! branch and therefore raises `TypeError: invalid node, check parsing`. The
//! pinned oracle (`035a07c3`) behaves exactly this way. We reproduce it by
//! returning [`TagsError::InvalidNode`] when a constant is evaluated, and the
//! differential test asserts that behaviour against the oracle.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::fmt;
use thiserror::Error;

/// Errors raised while tokenizing, parsing or evaluating a tag expression.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TagsError {
    /// The lexer encountered a character Jinja would reject.
    #[error("unexpected token '{token}' at position {position}")]
    UnexpectedToken {
        /// The offending token text (`end of template` for EOF).
        token: String,
        /// Byte offset in the original expression.
        position: usize,
    },

    /// The parser expected a specific token (for example `)` or `else`).
    #[error("expected '{expected}' at position {position}, got '{found}'")]
    ExpectedToken {
        /// Expected token spelling.
        expected: &'static str,
        /// Token actually found.
        found: String,
        /// Byte offset in the original expression.
        position: usize,
    },

    /// Input remained after a complete expression.
    #[error("chunk after expression at position {position}")]
    ChunkAfterExpression {
        /// Byte offset of the first trailing token.
        position: usize,
    },

    /// An empty expression was supplied.
    #[error("empty condition")]
    EmptyCondition,

    /// A parsed node that upstream's `_eval_node` cannot evaluate (constants).
    #[error("invalid node, check parsing")]
    InvalidNode,

    /// The internal result cache was poisoned by a panic.
    #[error("tag evaluator internal error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Name(String),
    LeftParen,
    RightParen,
    /// An operator or other character Jinja lexes as a non-name token.
    Other(String),
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    position: usize,
}

impl Token {
    fn display(&self) -> String {
        match &self.kind {
            TokenKind::Name(name) => name.clone(),
            TokenKind::LeftParen => "(".to_owned(),
            TokenKind::RightParen => ")".to_owned(),
            TokenKind::Other(text) => text.clone(),
            TokenKind::End => "end of template".to_owned(),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, TagsError> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let ch = bytes[index] as char;
        if ch.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                let next = bytes[index] as char;
                if next.is_ascii_alphanumeric() || next == '_' {
                    index += 1;
                } else {
                    break;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Name(input[start..index].to_owned()),
                position: start,
            });
            continue;
        }
        match ch {
            '(' => tokens.push(Token { kind: TokenKind::LeftParen, position: index }),
            ')' => tokens.push(Token { kind: TokenKind::RightParen, position: index }),
            _ => tokens.push(Token {
                kind: TokenKind::Other(ch.to_string()),
                position: index,
            }),
        }
        index += 1;
    }

    tokens.push(Token { kind: TokenKind::End, position: bytes.len() });
    Ok(tokens)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Const {
    True,
    False,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Name(String),
    Const(Const),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `test ? consequent : alternative`, matching Jinja's `a if t else b`.
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A missing `else` branch: Jinja stores Python `None` there and
    /// `_eval_node` rejects it with "invalid node, check parsing".
    Invalid,
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, position: 0 }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.position]
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.position].clone();
        if self.position + 1 < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn is_name(&self, name: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Name(found) if found == name)
    }

    fn consume_name(&mut self, name: &str) -> bool {
        if self.is_name(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn parse(&mut self) -> Result<Expr, TagsError> {
        if matches!(self.current().kind, TokenKind::End) {
            return Err(TagsError::EmptyCondition);
        }
        let expression = self.parse_expression()?;
        if !matches!(self.current().kind, TokenKind::End) {
            return Err(TagsError::ChunkAfterExpression {
                position: self.current().position,
            });
        }
        Ok(expression)
    }

    /// `expression := condexpr` (Jinja `parse_expression`).
    fn parse_expression(&mut self) -> Result<Expr, TagsError> {
        self.parse_condexpr()
    }

    /// `condexpr := or ['if' or 'else' condexpr]` (Jinja `CondExpr`).
    fn parse_condexpr(&mut self) -> Result<Expr, TagsError> {
        let consequent = self.parse_or()?;
        if self.consume_name("if") {
            let test = self.parse_or()?;
            let alternative = if self.consume_name("else") {
                self.parse_condexpr()?
            } else {
                // Upstream builds a CondExpr whose `expr2` is Python `None`
                // when `else` is omitted. It is only rejected if evaluated.
                Expr::Invalid
            };
            return Ok(Expr::Cond(
                Box::new(test),
                Box::new(consequent),
                Box::new(alternative),
            ));
        }
        Ok(consequent)
    }

    /// `or := and ('or' and)*`.
    fn parse_or(&mut self) -> Result<Expr, TagsError> {
        let mut left = self.parse_and()?;
        while self.consume_name("or") {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `and := not ('and' not)*`.
    fn parse_and(&mut self) -> Result<Expr, TagsError> {
        let mut left = self.parse_not()?;
        while self.consume_name("and") {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `not := 'not' not | compare`.
    fn parse_not(&mut self) -> Result<Expr, TagsError> {
        if self.consume_name("not") {
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_compare()
    }

    /// The overridden `BooleanParser.parse_compare`: a name (or constant) or a
    /// parenthesized expression — nothing else.
    fn parse_compare(&mut self) -> Result<Expr, TagsError> {
        match self.current().kind.clone() {
            TokenKind::Name(name) => {
                self.advance();
                match name.as_str() {
                    "true" | "True" => Ok(Expr::Const(Const::True)),
                    "false" | "False" => Ok(Expr::Const(Const::False)),
                    "none" | "None" => Ok(Expr::Const(Const::None)),
                    // Jinja lexes `and`/`or`/`if`/`else` as plain names;
                    // higher parse levels consume them as operators. A
                    // leading operator therefore leaves trailing tokens and
                    // surfaces as `chunk after expression`, like upstream.
                    _ => Ok(Expr::Name(name)),
                }
            }
            TokenKind::LeftParen => {
                self.advance();
                let inner = self.parse_expression()?;
                if !matches!(self.current().kind, TokenKind::RightParen) {
                    return Err(TagsError::ExpectedToken {
                        expected: ")",
                        found: self.current().display(),
                        position: self.current().position,
                    });
                }
                self.advance();
                Ok(inner)
            }
            _ => Err(TagsError::UnexpectedToken {
                token: self.current().display(),
                position: self.current().position,
            }),
        }
    }
}

/// A set of enabled tags with a Jinja-compatible boolean expression evaluator.
#[derive(Debug)]
pub struct Tags {
    tags: BTreeSet<String>,
    cache: Mutex<HashMap<String, bool>>,
}

impl Default for Tags {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Tags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.tags.iter().map(String::as_str).collect();
        write!(formatter, "Tags({})", names.join(", "))
    }
}

impl Tags {
    /// Create an empty tag set.
    pub fn new() -> Self {
        Self {
            tags: BTreeSet::new(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Create a tag set pre-populated from any iterable of names.
    pub fn with_tags<I, S>(tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut result = Self::new();
        for tag in tags {
            result.add(tag);
        }
        result
    }

    /// Enable `tag`.
    pub fn add<S: Into<String>>(&mut self, tag: S) {
        self.tags.insert(tag.into());
        self.clear_cache();
    }

    /// Disable `tag` if it is enabled.
    pub fn remove(&mut self, tag: &str) {
        self.tags.remove(tag);
        self.clear_cache();
    }

    /// Whether `tag` is enabled.
    pub fn has(&self, tag: &str) -> bool {
        self.tags.contains(tag)
    }

    /// Alias for [`Tags::has`], matching Python's `tag in tags`.
    pub fn contains(&self, tag: &str) -> bool {
        self.has(tag)
    }

    /// Iterate over enabled tags in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(String::as_str)
    }

    /// Number of enabled tags.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Whether no tags are enabled.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// Evaluate a Sphinx `only` directive condition.
    ///
    /// This is the canonical name matching upstream `Tags.eval_condition`.
    pub fn eval_condition(&self, condition: &str) -> Result<bool, TagsError> {
        {
            let cache = self
                .cache
                .lock()
                .map_err(|error| TagsError::Internal(error.to_string()))?;
            if let Some(cached) = cache.get(condition) {
                return Ok(*cached);
            }
        }

        let tokens = tokenize(condition)?;
        let mut parser = Parser::new(tokens);
        let expression = parser.parse()?;
        let result = self.eval_expr(&expression)?;

        let mut cache = self
            .cache
            .lock()
            .map_err(|error| TagsError::Internal(error.to_string()))?;
        cache.insert(condition.to_owned(), result);
        Ok(result)
    }

    /// Short alias for [`Tags::eval_condition`].
    pub fn eval(&self, condition: &str) -> Result<bool, TagsError> {
        self.eval_condition(condition)
    }

    fn eval_expr(&self, expression: &Expr) -> Result<bool, TagsError> {
        match expression {
            Expr::Name(name) => Ok(self.has(name)),
            // Upstream's `_eval_node` has no branch for `Const` nodes, and a
            // missing `else` branch stores Python `None` instead of a node.
            Expr::Const(_) | Expr::Invalid => Err(TagsError::InvalidNode),
            Expr::And(left, right) => Ok(self.eval_expr(left)? && self.eval_expr(right)?),
            Expr::Or(left, right) => Ok(self.eval_expr(left)? || self.eval_expr(right)?),
            Expr::Not(inner) => Ok(!self.eval_expr(inner)?),
            Expr::Cond(test, consequent, alternative) => {
                if self.eval_expr(test)? {
                    self.eval_expr(consequent)
                } else {
                    self.eval_expr(alternative)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Tags, TagsError};

    fn evaluate(condition: &str) -> Result<bool, TagsError> {
        Tags::with_tags(["foo", "bar"]).eval_condition(condition)
    }

    #[test]
    fn evaluates_names_and_boolean_operators() {
        assert_eq!(evaluate("foo"), Ok(true));
        assert_eq!(evaluate("baz"), Ok(false));
        assert_eq!(evaluate("not foo"), Ok(false));
        assert_eq!(evaluate("not baz"), Ok(true));
        assert_eq!(evaluate("foo and bar"), Ok(true));
        assert_eq!(evaluate("foo and baz"), Ok(false));
        assert_eq!(evaluate("foo or baz"), Ok(true));
        assert_eq!(evaluate("baz or qux"), Ok(false));
        assert_eq!(evaluate("not (foo and bar)"), Ok(false));
        assert_eq!(evaluate("(foo or baz) and bar"), Ok(true));
    }

    #[test]
    fn evaluates_conditional_expressions() {
        assert_eq!(evaluate("foo if bar else baz"), Ok(true));
        assert_eq!(evaluate("foo if baz else bar"), Ok(true));
        assert_eq!(evaluate("bar if baz else foo"), Ok(true));
        assert_eq!(evaluate("baz if foo else qux"), Ok(false));
        assert_eq!(evaluate("foo if bar else baz if foo else qux"), Ok(true));
        // Jinja permits a missing `else`; the branch is only rejected if taken.
        assert_eq!(evaluate("foo if bar"), Ok(true));
        assert_eq!(evaluate("baz if foo"), Ok(false));
        assert_eq!(evaluate("foo if baz"), Err(TagsError::InvalidNode));
    }

    #[test]
    fn constants_raise_invalid_node_like_upstream() {
        for expression in ["true", "True", "false", "False", "none", "None", "not None"] {
            assert_eq!(evaluate(expression), Err(TagsError::InvalidNode), "{expression}");
        }
    }

    #[test]
    fn malformed_expressions_are_rejected() {
        assert_eq!(evaluate(""), Err(TagsError::EmptyCondition));
        assert!(matches!(evaluate("foo bar"), Err(TagsError::ChunkAfterExpression { .. })));
        assert!(matches!(evaluate("and foo"), Err(TagsError::ChunkAfterExpression { .. })));
        assert!(matches!(evaluate("foo == bar"), Err(TagsError::ChunkAfterExpression { .. })));
        assert!(matches!(evaluate("1"), Err(TagsError::UnexpectedToken { .. })));
        assert!(matches!(evaluate("(foo"), Err(TagsError::ExpectedToken { expected: ")", .. })));
        assert!(matches!(evaluate("not"), Err(TagsError::UnexpectedToken { .. })));
    }

    #[test]
    fn tags_can_be_modified_and_cache_invalidated() {
        let mut tags = Tags::with_tags(["foo"]);
        assert_eq!(tags.eval_condition("foo"), Ok(true));
        tags.remove("foo");
        assert_eq!(tags.eval_condition("foo"), Ok(false));
        tags.add("bar");
        assert_eq!(tags.eval("bar"), Ok(true));
        assert_eq!(tags.len(), 1);
        assert!(!tags.is_empty());
        assert_eq!(tags.to_string(), "Tags(bar)");
    }
}
