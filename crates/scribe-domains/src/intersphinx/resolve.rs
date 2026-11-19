//! Intersphinx cross-reference resolution.
//!
//! This is the Rust equivalent of `sphinx.ext.intersphinx._resolve`:
//!
//! * untyped / `:any:` references search the merged inventory across domains;
//! * typed references map their role to object types via the owning domain;
//! * `std:label` / `std:term` match case-insensitively;
//! * `:external:`-annotated xrefs are restricted to a named inventory;
//! * `intersphinx_disabled_reftypes` is honored for ordinary missing-reference
//!   lookups but not for explicit `:external:` roles (upstream's
//!   `honor_disabled_refs and inv_name is None` rule).

use std::collections::{BTreeMap, BTreeSet};

use scribe_ast::{Document, Node, NodeId};

use crate::diagnostics::{codes, DomainDiagnostic};
use crate::error::DomainResult;
use crate::registry::DomainRegistry;
use crate::traits::DomainContext;
use crate::types::ResolvedReference;

use super::inventory::InventoryItem;
use super::load::FetchOutcome;
use super::Inventory;

/// A parsed `:external:` / `:external+inventory:` role name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalRole {
    /// Explicit inventory name (`external+name:...`), if given.
    pub inventory: Option<String>,
    /// Explicit domain (`external:name:domain:role`), if given.
    pub domain: Option<String>,
    /// Role name within the domain.
    pub role: String,
}

impl ExternalRole {
    /// Parses a role name such as `external+python:py:func` or
    /// `external:py:func`.  Returns `None` for anything else.
    pub fn parse(name: &str) -> Option<Self> {
        if name.len() <= 9 || !name.starts_with("external") {
            return None;
        }
        let suffix = &name[8..];
        let (inventory, rest) = match suffix.as_bytes().first()? {
            b'+' => {
                let (inv, tail) = suffix[1..].split_once(':')?;
                (Some(inv.to_owned()), tail.to_owned())
            }
            b':' => (None, suffix[1..].to_owned()),
            _ => return None,
        };
        if rest.is_empty() {
            return None;
        }
        let (domain, role) = match rest.split_once(':') {
            Some((domain, role)) if !role.is_empty() && !role.contains(':') => {
                (Some(domain.to_owned()), role.to_owned())
            }
            Some(_) => return None,
            None => (None, rest),
        };
        Some(Self { inventory, domain, role })
    }

    /// Resolves the domain that owns this role (standard domain first).
    pub fn infer_domain_role(&self, registry: &DomainRegistry) -> (String, String) {
        if let Some(domain) = &self.domain {
            return (domain.clone(), self.role.clone());
        }
        let mut names = registry.domain_names();
        names.sort_by(|a, b| (a != "std", a).cmp(&(b != "std", b)));
        for name in names {
            if registry
                .with_domain(&name, |domain| domain.role_names().contains(&self.role.as_str()))
                .unwrap_or(false)
            {
                return (name, self.role.clone());
            }
        }
        ("std".to_owned(), self.role.clone())
    }

    /// Annotates an existing pending xref so the intersphinx resolver claims
    /// it exclusively.  The frozen pending-xref shape is only extended with
    /// the two string/bool attributes upstream uses (`intersphinx`,
    /// `inventory`); no AST node kind is invented.
    pub fn annotate(
        &self,
        document: &mut Document,
        node_id: NodeId,
        registry: &DomainRegistry,
    ) -> DomainResult<()> {
        let (domain, role) = self.infer_domain_role(registry);
        let node = document.get_mut(node_id)?;
        node.attrs.set_bool("intersphinx", true);
        if let Some(inventory) = &self.inventory {
            node.attrs.set_str("inventory", inventory);
        }
        if let Some(mut view) = node.pending_xref_mut() {
            view.set_refdomain(Some(domain));
            view.set_reftype(Some(role));
        }
        Ok(())
    }
}

/// An intersphinx resolution plus the inventory it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntersphinxReference {
    /// The reference descriptor to materialize in place of the pending xref.
    pub reference: ResolvedReference,
    /// Name of the inventory that matched (`None` for the merged inventory).
    pub inventory_name: Option<String>,
    /// The exact name that matched (may differ in case for std types).
    pub matched_name: String,
}

/// Loaded inventories and their merged lookup view.
#[derive(Clone, Debug, Default)]
pub struct IntersphinxResolver {
    named: BTreeMap<String, Inventory>,
    /// `objtype -> name -> (item, source inventory name)`.
    main: BTreeMap<String, BTreeMap<String, (InventoryItem, String)>>,
    disabled_reftypes: BTreeSet<String>,
}

impl IntersphinxResolver {
    /// Builds a resolver from fetch outcomes (failures are simply absent).
    pub fn new(outcomes: Vec<FetchOutcome>, disabled_reftypes: &[String]) -> Self {
        let named: BTreeMap<String, Inventory> = outcomes
            .into_iter()
            .filter_map(|outcome| outcome.inventory.map(|inventory| (outcome.name, inventory)))
            .collect();
        Self::from_inventories(named, disabled_reftypes)
    }

    /// Builds a resolver from already-loaded inventories.
    pub fn from_inventories(
        named: BTreeMap<String, Inventory>,
        disabled_reftypes: &[String],
    ) -> Self {
        let mut main: BTreeMap<String, BTreeMap<String, (InventoryItem, String)>> = BTreeMap::new();
        for (name, inventory) in &named {
            for (objtype, objects) in &inventory.data {
                let bucket = main.entry(objtype.clone()).or_default();
                for (object_name, item) in objects {
                    // Deterministic shadowing: later (lexicographically larger)
                    // inventory names override earlier ones, like upstream's
                    // sorted `(NAME, EXPIRY)` update loop.
                    bucket.insert(object_name.clone(), (item.clone(), name.clone()));
                }
            }
        }
        Self {
            named,
            main,
            disabled_reftypes: disabled_reftypes.iter().cloned().collect(),
        }
    }

    /// True when a named inventory has been loaded.
    pub fn contains_inventory(&self, name: &str) -> bool {
        self.named.contains_key(name)
    }

    /// Sorted inventory names.
    pub fn inventory_names(&self) -> Vec<String> {
        self.named.keys().cloned().collect()
    }

    /// Borrows a named inventory.
    pub fn inventory(&self, name: &str) -> Option<&Inventory> {
        self.named.get(name)
    }

    /// True when no inventories were loaded.
    pub fn is_empty(&self) -> bool {
        self.named.is_empty()
    }

    /// All object names across the merged inventory, for fuzzy suggestions.
    pub fn candidate_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for objects in self.main.values() {
            names.extend(objects.keys().cloned());
        }
        names.sort();
        names.dedup();
        names
    }

    /// Resolves one pending xref against the loaded inventories.
    ///
    /// `domain_hint` is the pending xref's `refdomain`; when it is empty we
    /// follow upstream and decline (only domain-owned objects live in an
    /// inventory).
    #[allow(clippy::too_many_arguments)] // mirrors upstream `_resolve_reference`'s parameter set
    pub fn resolve_reference(
        &self,
        registry: &DomainRegistry,
        ctx: &DomainContext<'_>,
        domain_hint: Option<&str>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> Option<IntersphinxReference> {
        let node = document.get(node_id).ok()?;
        let external = node.attrs.get_bool("intersphinx").ok().flatten().unwrap_or(false);
        let explicit_inventory = node
            .get_str("inventory")
            .ok()
            .flatten()
            .map(str::to_owned)
            .or_else(|| {
                // `:external+name:` may also be encoded in the target as
                // `name:target` when no role annotation was recorded.
                let _ = ctx;
                None
            });
        let honor_disabled = !external && explicit_inventory.is_none();
        if honor_disabled && self.disabled_reftypes.contains("*") {
            return None;
        }

        let explicit_view = explicit_inventory.as_deref();
        if let Some(name) = explicit_view {
            if !self.contains_inventory(name) {
                return None;
            }
        }

        // Self-referential targets (`intersphinx_resolve_self`) carry the
        // project name as a prefix; strip it before looking anything up.
        let resolve_self = ctx.get_str("intersphinx_resolve_self").unwrap_or("");
        let mut effective_target = target;
        if !resolve_self.is_empty() {
            if let Some(rest) = target.strip_prefix(&format!("{resolve_self}:")) {
                effective_target = rest;
            }
        }

        let mut matched: Option<(InventoryItem, String, Option<String>, String)> = None;
        if typ == "any" {
            let mut domains = registry.domain_names();
            domains.sort_by(|a, b| (a != "std", a).cmp(&(b != "std", b)));
            'domains: for domain_name in domains {
                if honor_disabled && self.disabled_reftypes.contains(&format!("{domain_name}:*")) {
                    continue;
                }
                let Some(shared) = registry.domain(&domain_name) else {
                    continue;
                };
                let objtypes: Vec<String> = {
                    let guard = shared.lock();
                    guard.object_types().iter().map(|object| object.name.clone()).collect()
                };
                if let Some(found) = self.lookup_in_domain(
                    &domain_name,
                    &objtypes,
                    effective_target,
                    explicit_view,
                    ctx,
                    document,
                    node_id,
                    registry,
                ) {
                    matched = Some((found.0, found.1, found.2, domain_name));
                    break 'domains;
                }
            }
        } else {
            let domain_name = domain_hint.filter(|name| !name.is_empty())?;
            if honor_disabled && self.disabled_reftypes.contains(&format!("{domain_name}:*")) {
                return None;
            }
            let shared = registry.domain(domain_name)?;
            let mut objtypes: Vec<String> = {
                let guard = shared.lock();
                guard
                    .object_types()
                    .iter()
                    .filter(|object| object.roles.iter().any(|role| role == typ) || object.name == typ)
                    .map(|object| object.name.clone())
                    .collect()
            };
            // Backwards-compatibility aliases from upstream `_resolve_reference`.
            if domain_name == "std" && objtypes.iter().any(|kind| kind == "cmdoption") {
                objtypes.push("option".to_owned());
            }
            if domain_name == "py" && objtypes.iter().any(|kind| kind == "attribute") {
                objtypes.push("method".to_owned());
            }
            objtypes.dedup();
            if objtypes.is_empty() {
                return None;
            }
            if let Some(found) = self.lookup_in_domain(
                domain_name,
                &objtypes,
                effective_target,
                explicit_view,
                ctx,
                document,
                node_id,
                registry,
            ) {
                matched = Some((found.0, found.1, found.2, domain_name.to_owned()));
            }
        }

        let (item, matched_name, inventory_name, domain_name) = matched?;
        let reference = self.build_reference(
            &item,
            inventory_name.as_deref(),
            &domain_name,
            typ,
            node,
            contnode_id,
            document,
            honor_disabled,
        );
        Some(IntersphinxReference { reference, inventory_name, matched_name })
    }

    /// Looks up a target in one domain's object types, falling back to the
    /// domain's fully qualified name (upstream's second lookup attempt).
    #[allow(clippy::too_many_arguments)]
    fn lookup_in_domain(
        &self,
        domain_name: &str,
        objtypes: &[String],
        target: &str,
        explicit_inventory: Option<&str>,
        ctx: &DomainContext<'_>,
        document: &Document,
        node_id: NodeId,
        registry: &DomainRegistry,
    ) -> Option<(InventoryItem, String, Option<String>)> {
        if let Some(found) = self.lookup_by_target(domain_name, objtypes, target, explicit_inventory, ctx) {
            return Some(found);
        }
        let node = document.get(node_id).ok()?;
        let full_name = registry.with_domain(domain_name, |domain| domain.get_full_qualified_name(node))??;
        self.lookup_by_target(domain_name, objtypes, &full_name, explicit_inventory, ctx)
    }

    fn lookup_by_target(
        &self,
        domain_name: &str,
        objtypes: &[String],
        target: &str,
        explicit_inventory: Option<&str>,
        ctx: &DomainContext<'_>,
    ) -> Option<(InventoryItem, String, Option<String>)> {
        for objtype in objtypes {
            let full_type = format!("{domain_name}:{objtype}");
            if explicit_inventory.is_none()
                && (self.disabled_reftypes.contains(&full_type)
                    || self.disabled_reftypes.contains(objtype.as_str()))
            {
                continue;
            }
            if let Some(inventory) = explicit_inventory.and_then(|name| self.named.get(name)) {
                if let Some((name, item)) = inventory.lookup(&full_type, target) {
                    return Some((item.clone(), name.to_owned(), explicit_inventory.map(str::to_owned)));
                }
                continue;
            }
            let Some(objects) = self.main.get(&full_type) else {
                continue;
            };
            if let Some((item, inv)) = objects.get(target) {
                return Some((item.clone(), target.to_owned(), Some(inv.clone())));
            }
            if full_type == "std:label" || full_type == "std:term" {
                let lowered = target.to_lowercase();
                if let Some((name, (item, inv))) = objects
                    .iter()
                    .find(|(name, _)| name.to_lowercase() == lowered)
                {
                    return Some((item.clone(), name.clone(), Some(inv.clone())));
                }
            }
        }
        let _ = ctx;
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn build_reference(
        &self,
        item: &InventoryItem,
        inventory_name: Option<&str>,
        domain_name: &str,
        typ: &str,
        node: &Node,
        contnode_id: NodeId,
        document: &Document,
        honor_disabled: bool,
    ) -> ResolvedReference {
        let _ = honor_disabled;
        let mut uri = item.uri.clone();
        if !uri.contains("://") {
            if let Some(refdoc) = node.pending_xref().and_then(|view| view.refdoc()) {
                if !refdoc.is_empty() {
                    let depth = refdoc
                        .rsplit_once('/')
                        .map_or(0, |(parent, _)| parent.split('/').filter(|part| !part.is_empty()).count());
                    if depth > 0 {
                        uri = format!("{}{}", "../".repeat(depth), uri);
                    }
                }
            }
        }

        let reftitle = if item.project_version.is_empty() {
            format!("(in {})", item.project_name)
        } else if item.project_version.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            format!("(in {} v{})", item.project_name, item.project_version)
        } else {
            format!("(in {} {})", item.project_name, item.project_version)
        };

        let view = node.pending_xref();
        let explicit_title = view.and_then(|xref| xref.refexplicit()).unwrap_or(false);
        let title = if explicit_title {
            None
        } else if item.display_name == "-" || (domain_name == "std" && typ == "keyword") {
            let content = document
                .get(contnode_id)
                .map(|contnode| contnode.astext(document))
                .unwrap_or_default();
            let stripped = inventory_name
                .and_then(|name| content.strip_prefix(&format!("{name}:")))
                .unwrap_or(&content);
            // Upstream's `PyXRefRole` / `CXRefRole` append `()` to the *content*
            // node for callable roles at parse time; scribe defers that to
            // resolution, so reproduce it here when the inventory has no
            // explicit display name.
            let callable = matches!(
                (domain_name, typ),
                ("py", "func" | "meth") | ("c", "func") | ("cpp", "func")
            );
            let rendered = if callable && !stripped.ends_with("()") {
                format!("{stripped}()")
            } else {
                stripped.to_owned()
            };
            (rendered != content).then_some(rendered)
        } else {
            Some(item.display_name.clone())
        };

        let mut reference = ResolvedReference::uri(uri, title)
            .with_reftitle(reftitle)
            .with_extra("internal", "False");
        reference.classes.clear();
        reference
    }
}

/// Builds the diagnostic emitted when an `:external:`-annotated xref cannot be
/// resolved.  This mirrors upstream's distinct
/// `external <domain>:<type> reference target not found: <target>` message;
/// ordinary (non-external) xrefs keep the Phase 11 message unchanged.
pub fn external_missing_reference_diagnostic(
    ctx: &DomainContext<'_>,
    domain: &str,
    typ: &str,
    target: &str,
    node_id: NodeId,
    document: &Document,
) -> DomainDiagnostic {
    let message = format!("external {domain}:{typ} reference target not found: {target}");
    let mut diagnostic = DomainDiagnostic::missing_reference(
        message,
        typ,
        target,
        Some(ctx.docname.to_owned()),
        ctx.span_of(document, node_id),
    );
    diagnostic.code = codes::MISSING_REFERENCE.to_owned();
    diagnostic.help = Some(format!(
        "check the intersphinx inventory for `{target}` or fix the external reference"
    ));
    diagnostic
}

/// Builds the warning for an explicit inventory that is not loaded.
pub fn inventory_not_found_diagnostic(
    ctx: &DomainContext<'_>,
    inventory: &str,
    node_id: NodeId,
    document: &Document,
) -> DomainDiagnostic {
    let mut diagnostic = DomainDiagnostic::malformed(
        format!("inventory for external cross-reference not found: '{inventory}'"),
        Some("external".to_owned()),
        Some(inventory.to_owned()),
        Some(ctx.docname.to_owned()),
        ctx.span_of(document, node_id),
    );
    diagnostic.code = codes::INTERSPHINX.to_owned();
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_external_role_names() {
        assert_eq!(
            ExternalRole::parse("external+python:py:func"),
            Some(ExternalRole {
                inventory: Some("python".into()),
                domain: Some("py".into()),
                role: "func".into(),
            })
        );
        assert_eq!(
            ExternalRole::parse("external:ref"),
            Some(ExternalRole { inventory: None, domain: None, role: "ref".into() })
        );
        assert_eq!(ExternalRole::parse("external+python:a:b:c"), None);
        assert_eq!(ExternalRole::parse("py:func"), None);
    }
}
