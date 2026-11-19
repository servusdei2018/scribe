//! `sphinx.ext.ifconfig`: safe boolean expressions over configuration values.
//!
//! Upstream evaluates the directive argument with Python `eval()` against the
//! config namespace. This module implements the same observable grammar with
//! a hand-written tokenizer and recursive-descent parser over tagged values;
//! no Python interpreter and no dynamic code execution is involved. Invalid
//! expressions are reported with Python-shaped exception messages.

use std::collections::BTreeMap;
use std::fmt;

use scribe_ast::{Document, Node, NodeId, NodeKind};
use scribe_config::{Config, ConfigValue};
use scribe_rst::{Directive, DirectiveContext, DirectiveError, OptionSpec};

use super::{EXT_IFCONFIG, EXT_MARKER};

/// A value in the safe expression evaluator.
#[derive(Clone, Debug, PartialEq)]
pub enum IfConfigValue {
    /// Boolean literal or config flag.
    Bool(bool),
    /// String literal or config string.
    Str(String),
    /// Integer literal or config integer.
    Int(i64),
    /// Floating-point literal or config float.
    Float(f64),
    /// Python `None`.
    None,
    /// A list/tuple literal.
    List(Vec<IfConfigValue>),
}

impl IfConfigValue {
    /// Python truthiness.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Str(text) => !text.is_empty(),
            Self::Int(value) => *value != 0,
            Self::Float(value) => *value != 0.0,
            Self::None => false,
            Self::List(items) => !items.is_empty(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Str(_) => "str",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::None => "NoneType",
            Self::List(_) => "list",
        }
    }
}

/// Errors from tokenizing, parsing or evaluating an ifconfig expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    /// The expression was empty.
    Empty,
    /// A lexical error at a byte position.
    UnexpectedToken { token: String, position: usize },
    /// A syntax error at a byte position.
    Syntax(String),
    /// An unknown config name was looked up.
    NameError(String),
    /// A value had the wrong type for the operator.
    TypeError(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SyntaxError: unexpected EOF while parsing"),
            Self::UnexpectedToken { token, position } => {
                write!(formatter, "SyntaxError: invalid token '{token}' at position {position}")
            }
            Self::Syntax(detail) => write!(formatter, "SyntaxError: {detail}"),
            Self::NameError(name) => write!(formatter, "NameError: name '{name}' is not defined"),
            Self::TypeError(detail) => write!(formatter, "TypeError: {detail}"),
        }
    }
}

/// Evaluate `expression` against the configuration namespace.
pub fn eval(expression: &str, config: &Config, builder_name: &str) -> Result<bool, EvalError> {
    let mut namespace: BTreeMap<String, IfConfigValue> = BTreeMap::new();
    for (name, value) in config.iter() {
        namespace.insert(name.to_owned(), from_config_value(value));
    }
    namespace.insert("builder".to_owned(), IfConfigValue::Str(builder_name.to_owned()));
    let ast = Parser::new(expression)?.parse()?;
    let value = evaluate(&ast, &namespace)?;
    Ok(value.is_truthy())
}

fn from_config_value(value: &ConfigValue) -> IfConfigValue {
    match value {
        ConfigValue::Bool(value) => IfConfigValue::Bool(*value),
        ConfigValue::Str(text) => IfConfigValue::Str(text.clone()),
        ConfigValue::Int(value) => IfConfigValue::Int(*value),
        ConfigValue::Float(value) => IfConfigValue::Float(*value),
        ConfigValue::Path(path) => IfConfigValue::Str(path.to_string_lossy().into_owned()),
        ConfigValue::List(items) => {
            IfConfigValue::List(items.iter().map(from_config_value).collect())
        }
        ConfigValue::Dict(_) | ConfigValue::None => IfConfigValue::None,
    }
}

// ----- tokenizer -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Name(String),
    Str(String),
    Int(i64),
    Float(f64),
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    Comma,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    position: usize,
}

fn tokenize(input: &str) -> Result<Vec<Token>, EvalError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
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
        if ch == '\'' || ch == '"' {
            let quote = ch;
            index += 1;
            let start = index;
            let mut value = String::new();
            let mut closed = false;
            while index < bytes.len() {
                let current = bytes[index] as char;
                if current == '\\' && index + 1 < bytes.len() {
                    let next = bytes[index + 1] as char;
                    value.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                    index += 2;
                    continue;
                }
                if current == quote {
                    closed = true;
                    index += 1;
                    break;
                }
                let scalar = input[index..].chars().next().unwrap_or('\0');
                value.push(scalar);
                index += scalar.len_utf8();
            }
            if !closed {
                return Err(EvalError::UnexpectedToken {
                    token: input[start..].to_owned(),
                    position: start,
                });
            }
            tokens.push(Token { kind: TokenKind::Str(value), position: start });
            continue;
        }
        if ch.is_ascii_digit() {
            let start = index;
            let mut is_float = false;
            while index < bytes.len() {
                let next = bytes[index] as char;
                if next.is_ascii_digit() {
                    index += 1;
                } else if next == '.' && !is_float {
                    is_float = true;
                    index += 1;
                } else {
                    break;
                }
            }
            let text = &input[start..index];
            let kind = if is_float {
                text.parse::<f64>()
                    .map(TokenKind::Float)
                    .map_err(|_| EvalError::UnexpectedToken { token: text.to_owned(), position: start })?
            } else {
                text.parse::<i64>()
                    .map(TokenKind::Int)
                    .map_err(|_| EvalError::UnexpectedToken { token: text.to_owned(), position: start })?
            };
            tokens.push(Token { kind, position: start });
            continue;
        }
        let two = if index + 1 < bytes.len() { &input[index..index + 2] } else { "" };
        let (kind, width) = match two {
            "==" => (TokenKind::Eq, 2),
            "!=" => (TokenKind::Ne, 2),
            "<=" => (TokenKind::Le, 2),
            ">=" => (TokenKind::Ge, 2),
            _ => match ch {
                '<' => (TokenKind::Lt, 1),
                '>' => (TokenKind::Gt, 1),
                '(' => (TokenKind::LParen, 1),
                ')' => (TokenKind::RParen, 1),
                ',' => (TokenKind::Comma, 1),
                _ => {
                    return Err(EvalError::UnexpectedToken {
                        token: ch.to_string(),
                        position: index,
                    })
                }
            },
        };
        tokens.push(Token { kind, position: index });
        index += width;
    }
    Ok(tokens)
}

// ----- parser --------------------------------------------------------------

#[derive(Clone, Debug)]
enum Expr {
    Literal(IfConfigValue),
    Name(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Compare(Box<Expr>, CmpOp, Box<Expr>),
    List(Vec<Expr>),
}

#[derive(Clone, Copy, Debug)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Is,
    IsNot,
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn new(input: &str) -> Result<Self, EvalError> {
        if input.trim().is_empty() {
            return Err(EvalError::Empty);
        }
        Ok(Self { tokens: tokenize(input)?, position: 0 })
    }

    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn is_name(&self, name: &str) -> bool {
        matches!(self.current(), Some(Token { kind: TokenKind::Name(found), .. }) if found == name)
    }

    fn consume_name(&mut self, name: &str) -> bool {
        if self.is_name(name) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn parse(mut self) -> Result<Expr, EvalError> {
        let expression = self.parse_or()?;
        if self.position != self.tokens.len() {
            let token = self.current().map_or("end of expression".to_owned(), describe_token);
            return Err(EvalError::Syntax(format!("unexpected token '{token}'")));
        }
        Ok(expression)
    }

    fn parse_or(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_and()?;
        while self.consume_name("or") {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_not()?;
        while self.consume_name("and") {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, EvalError> {
        if self.consume_name("not") {
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> Result<Expr, EvalError> {
        let left = self.parse_primary()?;
        let op = match self.current().map(|token| &token.kind) {
            Some(TokenKind::Eq) => Some(CmpOp::Eq),
            Some(TokenKind::Ne) => Some(CmpOp::Ne),
            Some(TokenKind::Lt) => Some(CmpOp::Lt),
            Some(TokenKind::Le) => Some(CmpOp::Le),
            Some(TokenKind::Gt) => Some(CmpOp::Gt),
            Some(TokenKind::Ge) => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.position += 1;
            let right = self.parse_primary()?;
            return Ok(Expr::Compare(Box::new(left), op, Box::new(right)));
        }
        if self.consume_name("not") {
            if self.consume_name("in") {
                let right = self.parse_primary()?;
                return Ok(Expr::Compare(Box::new(left), CmpOp::NotIn, Box::new(right)));
            }
            self.position -= 1;
        }
        if self.consume_name("in") {
            let right = self.parse_primary()?;
            return Ok(Expr::Compare(Box::new(left), CmpOp::In, Box::new(right)));
        }
        if self.consume_name("is") {
            let op = if self.consume_name("not") { CmpOp::IsNot } else { CmpOp::Is };
            let right = self.parse_primary()?;
            return Ok(Expr::Compare(Box::new(left), op, Box::new(right)));
        }
        Ok(left)
    }

    fn parse_primary(&mut self) -> Result<Expr, EvalError> {
        let token = self.current().cloned().ok_or(EvalError::Empty)?;
        match token.kind {
            TokenKind::LParen => {
                self.position += 1;
                let mut items = Vec::new();
                let mut is_tuple = false;
                if !matches!(self.current().map(|t| &t.kind), Some(TokenKind::RParen)) {
                    loop {
                        items.push(self.parse_or()?);
                        if matches!(self.current().map(|t| &t.kind), Some(TokenKind::Comma)) {
                            self.position += 1;
                            is_tuple = true;
                            if matches!(self.current().map(|t| &t.kind), Some(TokenKind::RParen)) {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                if !matches!(self.current().map(|t| &t.kind), Some(TokenKind::RParen)) {
                    return Err(EvalError::Syntax("expected ')'".to_owned()));
                }
                self.position += 1;
                if is_tuple {
                    Ok(Expr::List(items))
                } else {
                    items.into_iter().next().ok_or(EvalError::Empty)
                }
            }
            TokenKind::Str(text) => {
                self.position += 1;
                Ok(Expr::Literal(IfConfigValue::Str(text)))
            }
            TokenKind::Int(value) => {
                self.position += 1;
                Ok(Expr::Literal(IfConfigValue::Int(value)))
            }
            TokenKind::Float(value) => {
                self.position += 1;
                Ok(Expr::Literal(IfConfigValue::Float(value)))
            }
            TokenKind::Name(name) => {
                self.position += 1;
                Ok(match name.as_str() {
                    "True" | "true" => Expr::Literal(IfConfigValue::Bool(true)),
                    "False" | "false" => Expr::Literal(IfConfigValue::Bool(false)),
                    "None" | "none" => Expr::Literal(IfConfigValue::None),
                    _ => Expr::Name(name),
                })
            }
            _ => Err(EvalError::UnexpectedToken {
                token: describe_token(&token),
                position: token.position,
            }),
        }
    }
}

fn describe_token(token: &Token) -> String {
    match &token.kind {
        TokenKind::Name(name) => name.clone(),
        TokenKind::Str(text) => format!("'{text}'"),
        TokenKind::Int(value) => value.to_string(),
        TokenKind::Float(value) => value.to_string(),
        TokenKind::Eq => "==".to_owned(),
        TokenKind::Ne => "!=".to_owned(),
        TokenKind::Lt => "<".to_owned(),
        TokenKind::Le => "<=".to_owned(),
        TokenKind::Gt => ">".to_owned(),
        TokenKind::Ge => ">=".to_owned(),
        TokenKind::LParen => "(".to_owned(),
        TokenKind::RParen => ")".to_owned(),
        TokenKind::Comma => ",".to_owned(),
    }
}

fn evaluate(expression: &Expr, namespace: &BTreeMap<String, IfConfigValue>) -> Result<IfConfigValue, EvalError> {
    match expression {
        Expr::Literal(value) => Ok(value.clone()),
        Expr::Name(name) => namespace
            .get(name)
            .cloned()
            .ok_or_else(|| EvalError::NameError(name.clone())),
        Expr::Not(inner) => Ok(IfConfigValue::Bool(!evaluate(inner, namespace)?.is_truthy())),
        Expr::And(left, right) => {
            let lhs = evaluate(left, namespace)?;
            if !lhs.is_truthy() {
                Ok(lhs)
            } else {
                evaluate(right, namespace)
            }
        }
        Expr::Or(left, right) => {
            let lhs = evaluate(left, namespace)?;
            if lhs.is_truthy() {
                Ok(lhs)
            } else {
                evaluate(right, namespace)
            }
        }
        Expr::List(items) => {
            let mut values = Vec::new();
            for item in items {
                values.push(evaluate(item, namespace)?);
            }
            Ok(IfConfigValue::List(values))
        }
        Expr::Compare(left, op, right) => {
            let lhs = evaluate(left, namespace)?;
            let rhs = evaluate(right, namespace)?;
            compare(lhs, *op, rhs)
        }
    }
}

fn compare(left: IfConfigValue, op: CmpOp, right: IfConfigValue) -> Result<IfConfigValue, EvalError> {
    let result = match op {
        CmpOp::Eq => values_equal(&left, &right),
        CmpOp::Ne => !values_equal(&left, &right),
        CmpOp::Is => values_equal(&left, &right),
        CmpOp::IsNot => !values_equal(&left, &right),
        CmpOp::In => contains(&right, &left),
        CmpOp::NotIn => !contains(&right, &left),
        CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
            let (Some(left_number), Some(right_number)) = (as_number(&left), as_number(&right)) else {
                return Err(EvalError::TypeError(format!(
                    "'{}' not supported between instances of '{}' and '{}'",
                    match op {
                        CmpOp::Lt => "<",
                        CmpOp::Le => "<=",
                        CmpOp::Gt => ">",
                        _ => ">=",
                    },
                    left.type_name(),
                    right.type_name()
                )));
            };
            match op {
                CmpOp::Lt => left_number < right_number,
                CmpOp::Le => left_number <= right_number,
                CmpOp::Gt => left_number > right_number,
                _ => left_number >= right_number,
            }
        }
    };
    Ok(IfConfigValue::Bool(result))
}

fn values_equal(left: &IfConfigValue, right: &IfConfigValue) -> bool {
    // Python compares numeric types across int/float/bool.
    if let (Some(a), Some(b)) = (as_number(left), as_number(right)) {
        if matches!(left, IfConfigValue::Bool(_)) != matches!(right, IfConfigValue::Bool(_))
            || !matches!(left, IfConfigValue::Bool(_))
        {
            return a == b;
        }
    }
    match (left, right) {
        (IfConfigValue::Bool(a), IfConfigValue::Bool(b)) => a == b,
        (IfConfigValue::Str(a), IfConfigValue::Str(b)) => a == b,
        (IfConfigValue::Int(a), IfConfigValue::Int(b)) => a == b,
        (IfConfigValue::Float(a), IfConfigValue::Float(b)) => a == b,
        (IfConfigValue::None, IfConfigValue::None) => true,
        (IfConfigValue::List(a), IfConfigValue::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_equal(x, y))
        }
        _ => false,
    }
}

fn contains(container: &IfConfigValue, item: &IfConfigValue) -> bool {
    match container {
        IfConfigValue::List(items) => items.iter().any(|value| values_equal(value, item)),
        IfConfigValue::Str(text) => match item {
            IfConfigValue::Str(needle) => text.contains(needle),
            _ => false,
        },
        _ => false,
    }
}

fn as_number(value: &IfConfigValue) -> Option<f64> {
    match value {
        IfConfigValue::Int(value) => Some(*value as f64),
        IfConfigValue::Float(value) => Some(*value),
        IfConfigValue::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

// ----- directive / transform ----------------------------------------------

/// `.. ifconfig:: expression` — content is kept only if the expression is true.
#[derive(Clone, Copy, Debug)]
pub struct IfConfigDirective;

impl Directive for IfConfigDirective {
    fn name(&self) -> &'static str {
        "ifconfig"
    }

    fn required_arguments(&self) -> usize {
        1
    }

    fn final_argument_whitespace(&self) -> bool {
        true
    }

    fn option_spec(&self) -> OptionSpec {
        OptionSpec::empty()
    }

    fn has_content(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let id = ctx.add(NodeKind::Generated, ctx.parent)?;
        let expression = ctx.argument(0).unwrap_or("").to_owned();
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_str(EXT_MARKER, EXT_IFCONFIG);
            node.attrs.set_str("expr", expression);
        }
        ctx.parse_nested_into(id)?;
        Ok(vec![id])
    }
}

/// Resolve every `ifconfig` node in `doc`.
///
/// Returns Python-shaped diagnostics for invalid expressions; the offending
/// node is replaced with a `system_message` instead of aborting the build.
#[must_use]
pub fn process(
    doc: &mut Document,
    config: &Config,
    builder_name: &str,
) -> Vec<String> {
    let ids: Vec<NodeId> = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            doc.get(id).is_ok_and(|node| {
                node.attrs.get_str(EXT_MARKER).ok().flatten() == Some(EXT_IFCONFIG)
            })
        })
        .collect();
    let mut warnings = Vec::new();
    for id in ids {
        let Ok(node) = doc.get(id) else { continue };
        let expression = node.attrs.get_str("expr").ok().flatten().unwrap_or("").to_owned();
        let children = node.children.clone();
        let result = eval(&expression, config, builder_name);
        match result {
            Ok(true) => {
                let _ = doc.replace_with_children(id);
            }
            Ok(false) => {
                let _ = doc.remove_node(id);
            }
            Err(error) => {
                let message = format!("Exception occurred in ifconfig expression: \n{error}");
                warnings.push(message.clone());
                let parent = doc.parent(id).ok().flatten().unwrap_or(doc.root);
                let mut system = Node::new(NodeKind::SystemMessage);
                system.attrs.set_list("classes", vec!["error".to_owned()]);
                system.attrs.set_int("level", 3);
                if let Ok(system_id) = doc.add_node(system, Some(parent)) {
                    let mut paragraph = Node::new(NodeKind::Paragraph);
                    paragraph.attrs.set_str("text", message);
                    let _ = doc.add_node(paragraph, Some(system_id));
                }
                let _ = doc.remove_node(id);
            }
        }
        // `children` is intentionally unused: `replace_with_children` splices
        // them in place. Keep the binding so future Phase 27 event hooks can
        // inspect the original subtree.
        let _ = children;
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::ConfigValue;

    fn config() -> Config {
        let mut config = Config::new();
        config.set("project", ConfigValue::from("Scribe")).unwrap();
        config.set("release", ConfigValue::from("")).unwrap();
        config.set("version", ConfigValue::Int(3)).unwrap();
        config
    }

    #[test]
    fn evaluates_boolean_combinations_and_comparisons() {
        let config = config();
        assert!(eval("project == 'Scribe'", &config, "html").unwrap());
        assert!(eval("project and not release", &config, "html").unwrap());
        assert!(eval("version >= 3 and builder == 'html'", &config, "html").unwrap());
        assert!(!eval("release or not project", &config, "html").unwrap());
        assert!(eval("'x' in ('x', 'y')", &config, "html").unwrap());
        assert!(eval("(version == 3) and (project != '')", &config, "html").unwrap());
    }

    #[test]
    fn unknown_names_and_syntax_errors_are_reported() {
        let config = config();
        assert!(matches!(eval("does_not_exist", &config, "html"), Err(EvalError::NameError(_))));
        assert!(eval("project ==", &config, "html").is_err());
        assert!(eval("", &config, "html").is_err());
    }

    #[test]
    fn malformed_expressions_never_panic() {
        let config = config();
        for expression in ["(((((", "'unterminated", "project @ 3", "1 + 2", ")("] {
            let _ = eval(expression, &config, "html");
        }
    }
}
