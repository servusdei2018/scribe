//! Tagged configuration values and their typed accessors.
//!
//! Every value that enters the configuration engine — whether from a built-in
//! default, a `conf.py` namespace, a `scribe.toml` file or a `-D` override — is
//! normalized into [`ConfigValue`]. Keeping a single tagged representation
//! makes override coercion deterministic and lets the CLI print values without
//! having to know their provenance.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The discriminant of a [`ConfigValue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigValueKind {
    /// A UTF-8 string.
    Str,
    /// A boolean.
    Bool,
    /// A signed integer.
    Int,
    /// A floating-point number.
    Float,
    /// An ordered sequence of values.
    List,
    /// A string-keyed mapping (ordered by key for deterministic output).
    Dict,
    /// A filesystem path.
    Path,
    /// Python `None` / an explicitly absent value.
    None,
}

impl fmt::Display for ConfigValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Str => "str",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::List => "list",
            Self::Dict => "dict",
            Self::Path => "path",
            Self::None => "none",
        };
        formatter.write_str(name)
    }
}

/// A strongly-tagged configuration value.
///
/// The variant set mirrors the simple Python values that Sphinx configuration
/// files conventionally contain. `Float` is included in addition to the core
/// variants because upstream registers numeric values such as
/// `linkcheck_timeout = 30` with `float` in their valid-type set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConfigValue {
    /// A string value.
    Str(String),
    /// A boolean value.
    Bool(bool),
    /// A signed integer value.
    Int(i64),
    /// A floating-point value.
    Float(f64),
    /// A list (Python `list` or `tuple`).
    List(Vec<ConfigValue>),
    /// A mapping with string keys (Python `dict`).
    Dict(BTreeMap<String, ConfigValue>),
    /// A filesystem path.
    Path(PathBuf),
    /// An absent / null value (Python `None`).
    None,
}

impl ConfigValue {
    /// Return the discriminant of this value.
    pub fn kind(&self) -> ConfigValueKind {
        match self {
            Self::Str(_) => ConfigValueKind::Str,
            Self::Bool(_) => ConfigValueKind::Bool,
            Self::Int(_) => ConfigValueKind::Int,
            Self::Float(_) => ConfigValueKind::Float,
            Self::List(_) => ConfigValueKind::List,
            Self::Dict(_) => ConfigValueKind::Dict,
            Self::Path(_) => ConfigValueKind::Path,
            Self::None => ConfigValueKind::None,
        }
    }

    /// Borrow the value as a string, if it is one.
    pub fn get_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    /// Copy the value as a boolean, if it is one.
    pub fn get_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Copy the value as an integer, if it is one.
    pub fn get_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Copy the value as a float, if it is one.
    pub fn get_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// Borrow the value as a list, if it is one.
    pub fn get_list(&self) -> Option<&[ConfigValue]> {
        match self {
            Self::List(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow the value as a mapping, if it is one.
    pub fn get_dict(&self) -> Option<&BTreeMap<String, ConfigValue>> {
        match self {
            Self::Dict(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow the value as a path, if it is one.
    pub fn get_path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            _ => None,
        }
    }

    /// Whether this value represents Python `None`.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Return a list whose elements are all strings (or path-like values).
    ///
    /// Returns `None` when the value is not a list or when any element is not a
    /// string. This is the common shape for Sphinx settings such as
    /// `extensions`, `exclude_patterns` and `templates_path`.
    pub fn as_string_list(&self) -> Option<Vec<String>> {
        let values = self.get_list()?;
        values
            .iter()
            .map(|value| match value {
                Self::Str(text) => Some(text.clone()),
                Self::Path(path) => Some(path.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect()
    }

    /// Convert to a plain JSON value (no tagged-enum wrapper).
    ///
    /// This is the representation used by the subprocess bridge and makes
    /// differential comparisons against Python values straightforward.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Str(text) => serde_json::Value::String(text.clone()),
            Self::Bool(flag) => serde_json::Value::Bool(*flag),
            Self::Int(number) => serde_json::Value::Number((*number).into()),
            Self::Float(number) => serde_json::Number::from_f64(*number)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::List(items) => serde_json::Value::Array(items.iter().map(Self::to_json).collect()),
            Self::Dict(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
            Self::Path(path) => serde_json::Value::String(path.to_string_lossy().into_owned()),
            Self::None => serde_json::Value::Null,
        }
    }

    /// Render the value in a compact, human-readable form for CLI output.
    pub fn display_string(&self) -> String {
        match self {
            Self::Str(text) => format!("\"{text}\""),
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::List(values) => {
                let items: Vec<String> = values.iter().map(Self::display_string).collect();
                format!("[{}]", items.join(", "))
            }
            Self::Dict(values) => {
                let items: Vec<String> = values
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", value.display_string()))
                    .collect();
                format!("{{{}}}", items.join(", "))
            }
            Self::Path(path) => format!("\"{}\"", path.display()),
            Self::None => "None".to_owned(),
        }
    }
}

impl fmt::Display for ConfigValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_string())
    }
}

impl From<&str> for ConfigValue {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for ConfigValue {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<bool> for ConfigValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for ConfigValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for ConfigValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<PathBuf> for ConfigValue {
    fn from(value: PathBuf) -> Self {
        Self::Path(value)
    }
}

impl From<Vec<ConfigValue>> for ConfigValue {
    fn from(value: Vec<ConfigValue>) -> Self {
        Self::List(value)
    }
}

impl From<BTreeMap<String, ConfigValue>> for ConfigValue {
    fn from(value: BTreeMap<String, ConfigValue>) -> Self {
        Self::Dict(value)
    }
}

impl From<Option<&str>> for ConfigValue {
    fn from(value: Option<&str>) -> Self {
        match value {
            Some(text) => Self::Str(text.to_owned()),
            None => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigValue, ConfigValueKind};

    #[test]
    fn typed_accessors_respect_the_tag() {
        let text = ConfigValue::from("sphinx");
        assert_eq!(text.kind(), ConfigValueKind::Str);
        assert_eq!(text.get_str(), Some("sphinx"));
        assert!(text.get_bool().is_none());
        assert!(text.as_string_list().is_none());

        let list = ConfigValue::List(vec![ConfigValue::from("a"), ConfigValue::from("b")]);
        assert_eq!(list.as_string_list(), Some(vec!["a".to_owned(), "b".to_owned()]));

        let mixed = ConfigValue::List(vec![ConfigValue::from("a"), ConfigValue::Int(1)]);
        assert!(mixed.as_string_list().is_none());
    }

    #[test]
    fn display_string_is_deterministic() {
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("b".to_owned(), ConfigValue::Bool(true));
        dict.insert("a".to_owned(), ConfigValue::Int(2));
        let value = ConfigValue::Dict(dict);
        assert_eq!(value.display_string(), "{a: 2, b: true}");
        assert_eq!(ConfigValue::None.to_string(), "None");
    }
}
