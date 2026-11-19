//! Typed directive option specifications, mirroring Docutils'
//! `docutils.parsers.rst.directives` conversion functions.
//!
//! Every built-in directive declares an [`OptionSpec`]; [`parse_options`]
//! validates the raw `:name: value` map carried by a Phase 04 `Pending` node
//! and produces typed [`OptionValue`]s. Unknown names, malformed values and
//! duplicate option lines are reported as [`OptionError`] miette diagnostics
//! with byte spans supplied by the caller.
//!
//! The conversions follow the pinned oracle verbatim (Docutils 0.23):
//! `flag`, `unchanged`, `unchanged_required`, `path`, `uri`, `int`,
//! `positive_int`, `nonnegative_int`, `percentage`, `class_option`,
//! `length_or_unitless`, `length_or_percentage_or_unitless`, `choice`,
//! `encoding`, `boolean` and the Sphinx helpers `optional_int` /
//! `positive_int_list`.

use std::collections::BTreeMap;
use std::fmt;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

/// A validated option value.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    /// A flag such as `:hidden:` (Docutils `flag` yields `None`).
    Flag,
    /// An `unchanged`/`unchanged_required`/`path`/`uri` string.
    Str(String),
    /// An integer-valued option.
    Int(i64),
    /// A list-valued option (notably `class_option`).
    List(Vec<String>),
    /// A boolean option.
    Bool(bool),
    /// A floating-point option (extension hooks).
    Float(f64),
}

impl OptionValue {
    /// Borrows a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Borrows an integer value.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Borrows a list value.
    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Self::List(values) => Some(values),
            _ => None,
        }
    }

    /// `true` for a flag option.
    pub fn is_flag(&self) -> bool {
        matches!(self, Self::Flag)
    }

    /// Human-readable type name used in diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Str(_) => "string",
            Self::Int(_) => "integer",
            Self::List(_) => "list",
            Self::Bool(_) => "boolean",
            Self::Float(_) => "number",
        }
    }
}

impl fmt::Display for OptionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag => write!(f, ""),
            Self::Str(s) => write!(f, "{s}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::List(values) => write!(f, "{}", values.join(" ")),
            Self::Bool(value) => write!(f, "{}", if *value { "1" } else { "0" }),
            Self::Float(value) => write!(f, "{value}"),
        }
    }
}

/// The kind of conversion applied to one option.
///
/// `PartialEq` is intentionally not derived: the `Custom` variant holds a
/// function pointer whose comparison is not meaningful across codegen units.
#[derive(Clone, Debug)]
pub enum OptionKind {
    /// `directives.flag` — no argument allowed.
    Flag,
    /// `directives.unchanged` — any argument, empty allowed.
    Unchanged,
    /// `directives.unchanged_required` — a non-empty argument is required.
    UnchangedRequired,
    /// `directives.path` — joined path lines.
    Path,
    /// `directives.uri` — unescaped whitespace removed.
    Uri,
    /// Python `int`.
    Int,
    /// Python `int` with `value >= 1`.
    PositiveInt,
    /// Python `int` with `value >= 0`.
    NonnegativeInt,
    /// Sphinx `optional_int` — empty means zero.
    OptionalInt,
    /// `directives.percentage` — integer with optional `%`.
    Percentage,
    /// `directives.class_option` — whitespace-separated CSS ids.
    ClassOption,
    /// `directives.length_or_unitless`.
    Length,
    /// `directives.length_or_percentage_or_unitless` with a default unit.
    LengthOrPercent(String),
    /// `directives.choice` over a fixed candidate set.
    Choice(Vec<String>),
    /// `directives.encoding` (normalised to a lower-case codec name).
    Encoding,
    /// Python `bool` accepting `1/0/true/false/yes/no/on/off`.
    Boolean,
    /// `directives.positive_int_list` — comma/space separated positive ints.
    PositiveIntList,
    /// `directives.value_or(('auto',), positive_int_list)`.
    AutoOrPositiveIntList,
    /// `directives.single_char_or_unicode`.
    SingleChar,
    /// `directives.value_or((None,), int)` — empty means `None`.
    ValueOrNoneInt,
    /// An extension-provided conversion (`name -> Result<OptionValue, String>`).
    Custom(&'static str, fn(&str) -> Result<OptionValue, String>),
}

impl OptionKind {
    /// Short description used in `OptionError::InvalidValue`.
    pub fn expected(&self) -> String {
        match self {
            Self::Flag => "no argument".to_owned(),
            Self::Unchanged => "any text".to_owned(),
            Self::UnchangedRequired => "non-empty text".to_owned(),
            Self::Path => "a path".to_owned(),
            Self::Uri => "a URI".to_owned(),
            Self::Int => "an integer".to_owned(),
            Self::PositiveInt => "a positive integer".to_owned(),
            Self::NonnegativeInt => "a non-negative integer".to_owned(),
            Self::OptionalInt => "an integer (empty = 0)".to_owned(),
            Self::Percentage => "a percentage".to_owned(),
            Self::ClassOption => "one or more class names".to_owned(),
            Self::Length => "a CSS length or unitless number".to_owned(),
            Self::LengthOrPercent(unit) => {
                format!("a length, percentage or unitless number (default unit: {unit})")
            }
            Self::Choice(values) => format!("one of {}", values.join(", ")),
            Self::Encoding => "a character encoding".to_owned(),
            Self::Boolean => "a boolean (1/0, true/false, yes/no, on/off)".to_owned(),
            Self::PositiveIntList => "a list of positive integers".to_owned(),
            Self::AutoOrPositiveIntList => "'auto' or a list of positive integers".to_owned(),
            Self::SingleChar => "a single character".to_owned(),
            Self::ValueOrNoneInt => "an integer or empty".to_owned(),
            Self::Custom(_, _) => "a custom value".to_owned(),
        }
    }

    fn convert(&self, raw: &str) -> Result<OptionValue, String> {
        match self {
            Self::Flag => {
                if raw.trim().is_empty() {
                    Ok(OptionValue::Flag)
                } else {
                    Err(format!("no argument is allowed; \"{raw}\" supplied"))
                }
            }
            Self::Unchanged => Ok(OptionValue::Str(raw.to_owned())),
            Self::UnchangedRequired | Self::Path | Self::Uri => {
                if raw.trim().is_empty() {
                    Err("argument required but none supplied".to_owned())
                } else if matches!(self, Self::Path) {
                    Ok(OptionValue::Str(
                        raw.lines().map(str::trim).collect::<String>(),
                    ))
                } else if matches!(self, Self::Uri) {
                    Ok(OptionValue::Str(raw.split_whitespace().collect()))
                } else {
                    Ok(OptionValue::Str(raw.to_owned()))
                }
            }
            Self::Int | Self::PositiveInt | Self::NonnegativeInt | Self::OptionalInt => {
                let trimmed = raw.trim();
                let value = if matches!(self, Self::OptionalInt) && trimmed.is_empty() {
                    0
                } else {
                    trimmed
                        .parse::<i64>()
                        .map_err(|_| format!("invalid integer literal: \"{raw}\""))?
                };
                if matches!(self, Self::PositiveInt) && value < 1 {
                    return Err("value must be positive".to_owned());
                }
                if matches!(self, Self::NonnegativeInt) && value < 0 {
                    return Err("negative value; must be positive or zero".to_owned());
                }
                Ok(OptionValue::Int(value))
            }
            Self::Percentage => {
                let trimmed = raw.trim().trim_end_matches([' ', '%']);
                let value = trimmed
                    .parse::<i64>()
                    .map_err(|_| format!("invalid percentage: \"{raw}\""))?;
                if value < 0 {
                    return Err("negative value; must be positive or zero".to_owned());
                }
                Ok(OptionValue::Int(value))
            }
            Self::ClassOption => {
                let names = raw
                    .split_whitespace()
                    .map(crate::names::make_id)
                    .collect::<Vec<_>>();
                if names.iter().any(String::is_empty) {
                    Err(format!("cannot make \"{raw}\" into a class name"))
                } else if names.is_empty() {
                    Err("argument required but none supplied".to_owned())
                } else {
                    Ok(OptionValue::List(names))
                }
            }
            Self::Length | Self::LengthOrPercent(_) => {
                let default = match self {
                    Self::LengthOrPercent(unit) => unit.as_str(),
                    _ => "",
                };
                parse_measure(raw, default).ok_or_else(|| format!("invalid length: \"{raw}\""))
            }
            Self::Choice(values) => values
                .iter()
                .find(|candidate| candidate.as_str() == raw)
                .cloned()
                .map(OptionValue::Str)
                .ok_or_else(|| format!("\"{raw}\" is not one of {}", values.join(", "))),
            Self::Encoding => {
                let encoding = raw.trim().to_ascii_lowercase().replace('_', "-");
                if encoding.is_empty() {
                    Err("argument required but none supplied".to_owned())
                } else {
                    Ok(OptionValue::Str(encoding))
                }
            }
            Self::Boolean => {
                let value = match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    other => return Err(format!("invalid boolean: \"{other}\"")),
                };
                Ok(OptionValue::Bool(value))
            }
            Self::PositiveIntList | Self::AutoOrPositiveIntList => {
                let trimmed = raw.trim();
                if matches!(self, Self::AutoOrPositiveIntList) && trimmed == "auto" {
                    return Ok(OptionValue::Str("auto".to_owned()));
                }
                let mut values = Vec::new();
                for token in trimmed.split([',', ' ']).filter(|s| !s.is_empty()) {
                    let value = token
                        .parse::<i64>()
                        .map_err(|_| format!("invalid integer in list: \"{token}\""))?;
                    if value < 1 {
                        return Err("list values must be positive".to_owned());
                    }
                    values.push(value);
                }
                if values.is_empty() {
                    return Err("argument required but none supplied".to_owned());
                }
                Ok(OptionValue::List(
                    values.into_iter().map(|value| value.to_string()).collect(),
                ))
            }
            Self::SingleChar => {
                let mut chars = raw.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => Ok(OptionValue::Str(ch.to_string())),
                    _ => Err("exactly one character is required".to_owned()),
                }
            }
            Self::ValueOrNoneInt => {
                if raw.trim().is_empty() {
                    Ok(OptionValue::Flag)
                } else {
                    raw.trim()
                        .parse::<i64>()
                        .map(OptionValue::Int)
                        .map_err(|_| format!("invalid integer literal: \"{raw}\""))
                }
            }
            Self::Custom(_, convert) => convert(raw),
        }
    }
}

// `IntList` is represented as `List` of strings so the public value enum stays
// small; integer-list options are read through `as_int_list`.
impl OptionValue {
    /// Returns the positive-integer list stored by a `PositiveIntList` option.
    pub fn as_int_list(&self) -> Option<Vec<i64>> {
        match self {
            Self::List(values) => values
                .iter()
                .map(|value| value.parse::<i64>().ok())
                .collect::<Option<Vec<_>>>(),
            Self::Flag => Some(Vec::new()),
            _ => None,
        }
    }
}

/// CSS 3 length units accepted by Docutils.
const CSS3_UNITS: &[&str] = &[
    "em", "ex", "ch", "rem", "vw", "vh", "vmin", "vmax", "cm", "mm", "Q", "in", "pt", "pc",
    "px",
];

fn parse_measure(raw: &str, default_unit: &str) -> Option<OptionValue> {
    let text = raw.trim();
    let split_index = text
        .char_indices()
        .find(|(_, ch)| ch.is_alphabetic())
        .map_or(text.len(), |(index, _)| index);
    let (number, unit) = text.split_at(split_index);
    let number = number.trim();
    if number.parse::<f64>().ok().is_none_or(|value| value < 0.0) {
        return None;
    }
    let normalized = match unit {
        "" => format!("{number}{default_unit}"),
        "%" => format!("{number}%"),
        other if CSS3_UNITS.contains(&other) => format!("{number}{other}"),
        _ => return None,
    };
    Some(OptionValue::Str(normalized))
}

/// An ordered collection of option name → conversion pairs.
#[derive(Clone, Debug, Default)]
pub struct OptionSpec {
    entries: BTreeMap<String, OptionKind>,
}

impl OptionSpec {
    /// An empty specification (no options accepted).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Registers an option, returning `self` for chaining.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, kind: OptionKind) -> Self {
        self.entries.insert(normalize_option_name(name.into()), kind);
        self
    }

    /// Merges another specification into this one.
    #[must_use]
    pub fn merge(mut self, other: OptionSpec) -> Self {
        self.entries.extend(other.entries);
        self
    }

    /// Looks up an option kind by normalized name.
    pub fn get(&self, name: &str) -> Option<&OptionKind> {
        self.entries.get(&normalize_option_name(name))
    }

    /// `true` when `name` is a declared option.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Number of declared options.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no options are declared.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates over `(name, kind)` in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &OptionKind)> {
        self.entries.iter()
    }

    /// The common `class`/`name` pair shared by most Docutils directives.
    pub fn common() -> Self {
        Self::empty()
            .with("class", OptionKind::ClassOption)
            .with("name", OptionKind::Unchanged)
    }

    /// Validates and converts a raw option map.
    pub fn convert(
        &self,
        raw: &BTreeMap<String, String>,
        span: SourceSpan,
    ) -> Result<BTreeMap<String, OptionValue>, OptionError> {
        let mut out = BTreeMap::new();
        for (name, value) in raw {
            let normalized = normalize_option_name(name);
            let Some(kind) = self.entries.get(&normalized) else {
                return Err(OptionError::UnknownOption {
                    name: normalized,
                    expected: self
                        .entries
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    span,
                });
            };
            let converted = kind.convert(value).map_err(|message| {
                OptionError::InvalidValue {
                    name: normalized.clone(),
                    expected: kind.expected(),
                    detail: message,
                    span,
                }
            })?;
            out.insert(normalized, converted);
        }
        Ok(out)
    }
}

fn normalize_option_name(name: impl Into<String>) -> String {
    normalize(name)
}

/// Normalizes an option name: trim, lower-case, underscores → hyphens.
pub fn normalize(name: impl Into<String>) -> String {
    name.into().trim().to_ascii_lowercase().replace('_', "-")
}

/// Converts a raw option map using a spec, reporting unknown/invalid options.
pub fn parse_options(
    raw: &BTreeMap<String, String>,
    spec: &OptionSpec,
    span: SourceSpan,
) -> Result<BTreeMap<String, OptionValue>, OptionError> {
    spec.convert(raw, span)
}

/// Parses literal `:name: value` option lines, detecting duplicates and
/// malformed markers. The Phase 04 `Pending` contract already stores a map, so
/// [`OptionSpec::convert`] is used there; this helper is exposed for callers
/// that still have the original lines (and for exhaustive option tests).
pub fn parse_option_lines(
    lines: &[String],
    spec: &OptionSpec,
    span: SourceSpan,
) -> Result<BTreeMap<String, OptionValue>, OptionError> {
    let mut raw = BTreeMap::new();
    for line in lines {
        let trimmed = line.trim_end();
        let Some(rest) = trimmed.strip_prefix(':') else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            return Err(OptionError::MalformedOptionLine {
                line: trimmed.to_owned(),
                span,
            });
        };
        let name = normalize_option_name(name);
        if raw.insert(name.clone(), value.trim().to_owned()).is_some() {
            return Err(OptionError::DuplicateOption { name, span });
        }
    }
    parse_options(&raw, spec, span)
}

/// An option validation failure.
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum OptionError {
    /// An option name was not declared by the directive's spec.
    #[error("unknown option ':{name}': {expected}")]
    #[diagnostic(
        code(scribe::rst::unknown_option),
        help("remove the option or use one listed in the message"),
        severity(Error)
    )]
    UnknownOption {
        /// Normalized option name.
        name: String,
        /// Comma-separated list of accepted names.
        expected: String,
        /// Byte span of the directive marker.
        #[label("unknown option here")]
        span: SourceSpan,
    },

    /// An option value failed its typed conversion.
    #[error("invalid value for ':{name}': {detail} (expected {expected})")]
    #[diagnostic(
        code(scribe::rst::invalid_option_value),
        help("check the option value against the directive documentation"),
        severity(Error)
    )]
    InvalidValue {
        /// Normalized option name.
        name: String,
        /// Human-readable expected type.
        expected: String,
        /// Conversion failure detail.
        detail: String,
        /// Byte span of the directive marker.
        #[label("invalid value here")]
        span: SourceSpan,
    },

    /// The same option appeared more than once.
    #[error("duplicate option ':{name}'")]
    #[diagnostic(
        code(scribe::rst::duplicate_option),
        help("specify the option only once"),
        severity(Error)
    )]
    DuplicateOption {
        /// Normalized option name.
        name: String,
        /// Byte span of the directive marker.
        #[label("second occurrence here")]
        span: SourceSpan,
    },

    /// An option line lacked a closing colon.
    #[error("malformed option line: {line}")]
    #[diagnostic(
        code(scribe::rst::malformed_option_line),
        help("write options as ':name: value'"),
        severity(Error)
    )]
    MalformedOptionLine {
        /// The offending line.
        line: String,
        /// Byte span of the directive marker.
        #[label("malformed option here")]
        span: SourceSpan,
    },
}

impl OptionError {
    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownOption { .. } => "scribe::rst::unknown_option",
            Self::InvalidValue { .. } => "scribe::rst::invalid_option_value",
            Self::DuplicateOption { .. } => "scribe::rst::duplicate_option",
            Self::MalformedOptionLine { .. } => "scribe::rst::malformed_option_line",
        }
    }

    /// Byte span of the error.
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::UnknownOption { span, .. }
            | Self::InvalidValue { span, .. }
            | Self::DuplicateOption { span, .. }
            | Self::MalformedOptionLine { span, .. } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> OptionSpec {
        OptionSpec::common()
            .with("depth", OptionKind::NonnegativeInt)
            .with("class", OptionKind::ClassOption)
            .with("flag", OptionKind::Flag)
            .with("choice", OptionKind::Choice(vec!["a".into(), "b".into()]))
    }

    #[test]
    fn converts_typed_values() {
        let raw = BTreeMap::from([
            ("class".to_owned(), "foo bar".to_owned()),
            ("depth".to_owned(), "2".to_owned()),
            ("flag".to_owned(), String::new()),
            ("choice".to_owned(), "b".to_owned()),
        ]);
        let out = parse_options(&raw, &spec(), SourceSpan::new(0usize.into(), 1)).unwrap();
        assert_eq!(out["class"].as_list().unwrap(), &["foo", "bar"]);
        assert_eq!(out["depth"].as_i64(), Some(2));
        assert!(out["flag"].is_flag());
        assert_eq!(out["choice"].as_str(), Some("b"));
    }

    #[test]
    fn rejects_unknown_invalid_and_duplicate() {
        let span = SourceSpan::new(0usize.into(), 1);
        let unknown = BTreeMap::from([("nope".to_owned(), "x".to_owned())]);
        assert!(matches!(
            parse_options(&unknown, &spec(), span),
            Err(OptionError::UnknownOption { .. })
        ));
        let invalid = BTreeMap::from([("depth".to_owned(), "-1".to_owned())]);
        assert!(matches!(
            parse_options(&invalid, &spec(), span),
            Err(OptionError::InvalidValue { .. })
        ));
        let lines = vec![":depth: 1".to_owned(), ":DEPTH: 2".to_owned()];
        assert!(matches!(
            parse_option_lines(&lines, &spec(), span),
            Err(OptionError::DuplicateOption { .. })
        ));
    }

    #[test]
    fn parses_lengths_and_percentages() {
        let spec = OptionSpec::empty()
            .with("length", OptionKind::Length)
            .with("percent", OptionKind::Percentage)
            .with("width", OptionKind::LengthOrPercent("px".into()));
        let raw = BTreeMap::from([
            ("length".to_owned(), "3 pt".to_owned()),
            ("percent".to_owned(), "50%".to_owned()),
            ("width".to_owned(), "10".to_owned()),
        ]);
        let out = spec.convert(&raw, SourceSpan::new(0usize.into(), 1)).unwrap();
        assert_eq!(out["length"].as_str(), Some("3pt"));
        assert_eq!(out["percent"].as_i64(), Some(50));
        assert_eq!(out["width"].as_str(), Some("10px"));
    }
}
