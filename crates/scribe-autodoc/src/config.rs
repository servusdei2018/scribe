//! Autodoc/napoleon configuration and directive option handling (Phase 21).

use std::collections::{BTreeMap, BTreeSet};

use scribe_config::{Config, ConfigDefault, ConfigEntry, ConfigError, ConfigRegistry, ConfigType, ConfigValue};
use scribe_rst::directive_registry::DirectiveContext;

use crate::napoleon::NapoleonOptions;

/// Member ordering, mirroring `autodoc_member_order` and `:member-order:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberOrder {
    /// Alphabetical by member name (upstream default).
    Alphabetical,
    /// Source declaration order.
    BySource,
    /// Group by kind (methods, attributes, nested classes) then alphabetically.
    Groupwise,
}

impl MemberOrder {
    /// Parses a `:member-order:` / `autodoc_member_order` value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "alphabetical" => Some(Self::Alphabetical),
            "bysource" | "source" => Some(Self::BySource),
            "groupwise" => Some(Self::Groupwise),
            _ => None,
        }
    }
}

/// Which class docstring `autoclass` uses, mirroring `autoclass_content` and
/// `:class-doc-from:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassDocFrom {
    /// Class docstring only (upstream default).
    Class,
    /// `__init__` docstring only.
    Init,
    /// Both, init first.
    Both,
}

impl ClassDocFrom {
    /// Parses an `autoclass_content` / `:class-doc-from:` value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "class" => Some(Self::Class),
            "init" => Some(Self::Init),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Fully-resolved options for one auto directive invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoOptions {
    /// `None` → no members; `Some([])` → all members; `Some(names)` → explicit.
    pub members: Option<Vec<String>>,
    /// Include undocumented members.
    pub undoc_members: bool,
    /// Include private (`_name`) members.
    pub private_members: bool,
    /// Include special (`__name__`) members.
    pub special_members: bool,
    /// Include inherited members.
    pub inherited_members: bool,
    /// Show base classes in `desc_content`.
    pub show_inheritance: bool,
    /// Names to exclude.
    pub exclude_members: std::collections::BTreeSet<String>,
    /// Member ordering.
    pub member_order: MemberOrder,
    /// Suppress indexing (`no-index` / `noindex`).
    pub no_index: bool,
    /// Suppress the index entry only.
    pub no_index_entry: bool,
    /// Ignore `__all__` when selecting members.
    pub ignore_module_all: bool,
    /// Which class docstring to use.
    pub class_doc_from: ClassDocFrom,
    /// Override the displayed type annotation (assignment directives).
    pub annotation: Option<String>,
    /// Suppress the displayed value (assignment directives).
    pub no_value: bool,
}

impl Default for AutoOptions {
    fn default() -> Self {
        Self {
            members: None,
            undoc_members: false,
            private_members: false,
            special_members: false,
            inherited_members: false,
            show_inheritance: false,
            exclude_members: BTreeSet::new(),
            member_order: MemberOrder::Alphabetical,
            no_index: false,
            no_index_entry: false,
            ignore_module_all: false,
            class_doc_from: ClassDocFrom::Class,
            annotation: None,
            no_value: false,
        }
    }
}

/// Declares every config key owned by the autodoc/napoleon extensions.
///
/// Idempotent: keys already present in the built-in registry are left alone.
pub fn register_config_keys(registry: &mut ConfigRegistry) -> Result<(), ConfigError> {
    let entries: &[(&str, ConfigValue, ConfigType, &str)] = &[
        ("autodoc_path", ConfigValue::List(Vec::new()), ConfigType::List, "env"),
        ("napoleon_google_docstring", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("napoleon_numpy_docstring", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("napoleon_use_param", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("napoleon_use_rtype", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("napoleon_use_keyword", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("napoleon_use_ivar", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("napoleon_use_admonition_for_examples", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("napoleon_use_admonition_for_notes", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("napoleon_include_init_with_doc", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("napoleon_include_private_with_doc", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("napoleon_include_special_with_doc", ConfigValue::Bool(false), ConfigType::Bool, "env"),
    ];
    // Phase 22: autosummary-owned keys.
    crate::autosummary::register_config_keys(registry)?;
    for (name, default, value_type, rebuild) in entries {
        if registry.contains(name) {
            continue;
        }
        registry.register(ConfigEntry {
            name,
            default: ConfigDefault::Value(default.clone()),
            value_type: value_type.clone(),
            rebuild,
            description: "Phase 21 native autodoc/napoleon setting.",
        })?;
    }
    Ok(())
}

/// Builds the [`NapoleonOptions`] from resolved configuration.
#[must_use]
pub fn napoleon_options(config: Option<&Config>) -> NapoleonOptions {
    let get = |key: &str, default: bool| config.and_then(|config| config.get_bool(key)).unwrap_or(default);
    NapoleonOptions {
        google: get("napoleon_google_docstring", true),
        numpy: get("napoleon_numpy_docstring", true),
        use_param: get("napoleon_use_param", true),
        use_rtype: get("napoleon_use_rtype", true),
        use_keyword: get("napoleon_use_keyword", true),
        use_ivar: get("napoleon_use_ivar", false),
        use_admonition_for_examples: get("napoleon_use_admonition_for_examples", false),
        use_admonition_for_notes: get("napoleon_use_admonition_for_notes", false),
    }
}

/// Resolves all auto-directive options from a directive context and config.
///
/// Directive options take precedence over `autodoc_default_options`, which in
/// turn take precedence over upstream config defaults. Extendable options
/// (`members`, `exclude-members`, `private-members`, `special-members`) with a
/// leading `+` are merged rather than replaced, matching upstream.
#[must_use]
pub fn auto_options(ctx: &DirectiveContext<'_>, config: Option<&Config>) -> AutoOptions {
    let mut options = AutoOptions {
        member_order: config
            .and_then(|config| config.get_str("autodoc_member_order"))
            .and_then(MemberOrder::parse)
            .unwrap_or(MemberOrder::Alphabetical),
        class_doc_from: config
            .and_then(|config| config.get_str("autoclass_content"))
            .and_then(ClassDocFrom::parse)
            .unwrap_or(ClassDocFrom::Class),
        ..AutoOptions::default()
    };

    if let Some(config) = config {
        if let Some(defaults) = config.get_dict("autodoc_default_options") {
            apply_dict(&mut options, defaults);
        }
    }

    // Direct options override the defaults.
    if let Some(value) = option(ctx, "member-order") {
        if let Some(order) = MemberOrder::parse(&value) {
            options.member_order = order;
        }
    }
    if let Some(value) = option(ctx, "class-doc-from") {
        if let Some(from) = ClassDocFrom::parse(&value) {
            options.class_doc_from = from;
        }
    }
    if flag(ctx, "undoc-members") {
        options.undoc_members = true;
    }
    if flag(ctx, "private-members") {
        options.private_members = true;
    }
    if flag(ctx, "special-members") {
        options.special_members = true;
    }
    if flag(ctx, "inherited-members") {
        options.inherited_members = true;
    }
    if flag(ctx, "show-inheritance") {
        options.show_inheritance = true;
    }
    if flag(ctx, "ignore-module-all") {
        options.ignore_module_all = true;
    }
    if flag(ctx, "no-value") {
        options.no_value = true;
    }
    if flag(ctx, "no-index") || flag(ctx, "noindex") {
        options.no_index = true;
    }
    if flag(ctx, "no-index-entry") || flag(ctx, "noindexentry") {
        options.no_index_entry = true;
    }
    if let Some(annotation) = option(ctx, "annotation") {
        options.annotation = Some(annotation);
    }

    let members_value = raw_value(ctx, "members");
    if members_value.is_some() {
        options.members = Some(parse_member_list(members_value.as_deref()));
    }
    let exclude_value = raw_value(ctx, "exclude-members");
    if let Some(value) = exclude_value {
        options.exclude_members.extend(parse_member_list(Some(&value)));
    }
    let private_value = raw_value(ctx, "private-members");
    if private_value.is_some() {
        options.private_members = true;
        options.members.get_or_insert_with(Vec::new).extend(parse_member_list(private_value.as_deref()));
    }
    let special_value = raw_value(ctx, "special-members");
    if special_value.is_some() {
        options.special_members = true;
        options.members.get_or_insert_with(Vec::new).extend(parse_member_list(special_value.as_deref()));
    }
    options
}

fn apply_dict(options: &mut AutoOptions, defaults: &BTreeMap<String, ConfigValue>) {
    for (key, value) in defaults {
        match key.as_str() {
            "members" => options.members = Some(parse_member_list(value.get_str())),
            "undoc-members" | "undoc_members" => options.undoc_members = value.get_bool().unwrap_or(false),
            "private-members" | "private_members" => options.private_members = value.get_bool().unwrap_or(false),
            "special-members" | "special_members" => options.special_members = value.get_bool().unwrap_or(false),
            "inherited-members" | "inherited_members" => options.inherited_members = value.get_bool().unwrap_or(false),
            "show-inheritance" | "show_inheritance" => options.show_inheritance = value.get_bool().unwrap_or(false),
            "no-index" | "no_index" | "noindex" => options.no_index = value.get_bool().unwrap_or(false),
            "no-index-entry" | "no_index_entry" => options.no_index_entry = value.get_bool().unwrap_or(false),
            "member-order" | "member_order" => {
                if let Some(value) = value.get_str().and_then(MemberOrder::parse) {
                    options.member_order = value;
                }
            }
            "class-doc-from" | "class_doc_from" => {
                if let Some(value) = value.get_str().and_then(ClassDocFrom::parse) {
                    options.class_doc_from = value;
                }
            }
            "exclude-members" | "exclude_members" => {
                if let Some(value) = value.get_str() {
                    options.exclude_members.extend(parse_member_list(Some(value)));
                }
            }
            _ => {}
        }
    }
}

/// Reads a raw option value, preferring the converted typed value but falling
/// back to the raw map (so flags declared as `Unchanged` are distinguishable).
fn raw_value(ctx: &DirectiveContext<'_>, name: &str) -> Option<String> {
    if let Some(value) = ctx.option_str(name) {
        return Some(value.to_owned());
    }
    ctx.raw_options.get(name).cloned()
}

fn option(ctx: &DirectiveContext<'_>, name: &str) -> Option<String> {
    raw_value(ctx, name)
}

fn flag(ctx: &DirectiveContext<'_>, name: &str) -> bool {
    ctx.flag(name) || ctx.raw_options.contains_key(name)
}

/// Splits a comma/space separated member list. `None` or an empty string means
/// "all members" (upstream `ALL`).
fn parse_member_list(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split([',', ' '])
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_order_parsing() {
        assert_eq!(MemberOrder::parse("alphabetical"), Some(MemberOrder::Alphabetical));
        assert_eq!(MemberOrder::parse("bysource"), Some(MemberOrder::BySource));
        assert_eq!(MemberOrder::parse("groupwise"), Some(MemberOrder::Groupwise));
        assert_eq!(MemberOrder::parse("nope"), None);
    }

    #[test]
    fn config_keys_are_idempotent() {
        let mut registry = ConfigRegistry::builtin();
        register_config_keys(&mut registry).unwrap();
        let len = registry.len();
        register_config_keys(&mut registry).unwrap();
        assert_eq!(registry.len(), len);
        assert!(registry.contains("napoleon_google_docstring"));
    }
}
