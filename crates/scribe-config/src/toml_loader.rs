//! Loader for native `scribe.toml` configuration files.
//!
//! TOML values are converted into the same [`ConfigValue`] model used by the
//! `conf.py` bridges. A file may either use flat top-level keys or nest them
//! under a `[config]` table; both forms are accepted.

use crate::{ConfigError, ConfigRegistry, ConfigValue, ConfigWarning, ConfigWarningKind};
use std::collections::BTreeMap;
use std::path::Path;

/// Load a `scribe.toml` file into raw configuration values.
pub fn load_toml(
    path: &Path,
    registry: &ConfigRegistry,
) -> Result<(BTreeMap<String, ConfigValue>, Vec<ConfigWarning>), ConfigError> {
    let contents = std::fs::read_to_string(path)?;
    let document: toml::Value = toml::from_str(&contents)?;

    let table = match &document {
        toml::Value::Table(root) => match root.get("config") {
            Some(toml::Value::Table(nested)) => nested,
            _ => root,
        },
        _ => {
            return Err(ConfigError::InvalidConfigFile {
                path: path.to_path_buf(),
                message: "top-level TOML value must be a table".to_owned(),
            });
        }
    };

    let mut values = BTreeMap::new();
    let mut warnings = Vec::new();
    for (key, value) in table {
        let converted = toml_to_value(value);
        if !registry.contains(key) {
            warnings.push(
                ConfigWarning::new(
                    ConfigWarningKind::UnknownKey,
                    format!("unknown config value `{key}` in scribe.toml, ignoring"),
                )
                .with_path(path),
            );
            continue;
        }
        values.insert(key.clone(), converted);
    }

    Ok((values, warnings))
}

fn toml_to_value(value: &toml::Value) -> ConfigValue {
    match value {
        toml::Value::String(text) => ConfigValue::Str(text.clone()),
        toml::Value::Integer(number) => ConfigValue::Int(*number),
        toml::Value::Float(number) => ConfigValue::Float(*number),
        toml::Value::Boolean(flag) => ConfigValue::Bool(*flag),
        toml::Value::Datetime(datetime) => ConfigValue::Str(datetime.to_string()),
        toml::Value::Array(items) => {
            ConfigValue::List(items.iter().map(toml_to_value).collect())
        }
        toml::Value::Table(entries) => ConfigValue::Dict(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), toml_to_value(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::load_toml;
    use crate::ConfigRegistry;
    use std::path::PathBuf;

    #[test]
    fn converts_flat_and_nested_values() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_scribe.toml");
        let (values, warnings) = load_toml(&path, &ConfigRegistry::builtin()).expect("sample TOML");
        assert!(warnings.is_empty());
        assert_eq!(values.get("project").and_then(|v| v.get_str()), Some("TOML Project"));
        assert_eq!(values.get("nitpicky").and_then(|v| v.get_bool()), Some(true));
        assert_eq!(
            values.get("extensions").and_then(|v| v.as_string_list()),
            Some(vec!["sphinx.ext.autodoc".to_owned()])
        );
        assert!(values.get("html_theme_options").and_then(|v| v.get_dict()).is_some());
    }
}
