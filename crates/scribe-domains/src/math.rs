//! The math domain (`math`) — port of `sphinx/domains/math.py` (Phase 13).
//!
//! Registers labelled display equations and resolves `:math:numref:` /
//! `:eq:` references to equation numbers, consuming the Phase 09/10 numfig
//! metadata when present.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind};

use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ReferenceNodeKind, ResolvedReference};

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();

/// Upstream `MathDomain.object_types` is empty.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(Vec::new)
}

/// Upstream `MathDomain.roles`.
pub const ROLES: &[&str] = &["numref", "eq"];

/// Serializable math inventory: label id → (docname, equation number).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MathDomainData {
    /// Label id → (docname, eqno).
    pub objects: std::collections::BTreeMap<String, (String, usize)>,
}

/// The `math` domain implementation.
pub struct MathDomain {
    data: parking_lot::RwLock<MathDomainData>,
    next_number: parking_lot::Mutex<usize>,
}

impl Default for MathDomain {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Debug for MathDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MathDomain").field("equations", &self.data.read().objects.len()).finish_non_exhaustive()
    }
}

impl MathDomain {
    /// Creates an empty domain.
    pub fn new() -> Self {
        Self { data: parking_lot::RwLock::new(MathDomainData::default()), next_number: parking_lot::Mutex::new(0) }
    }
    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: MathDomainData) -> Self {
        let next = data.objects.values().map(|(_, number)| *number).max().unwrap_or(0);
        Self { data: parking_lot::RwLock::new(data), next_number: parking_lot::Mutex::new(next) }
    }
    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> MathDomainData {
        self.data.read().clone()
    }
    /// Registers a labelled equation (upstream `note_equation`).
    pub fn note_equation(&self, docname: &str, label: &str) {
        self.note_equation_with_number(docname, label, None);
    }

    /// Registers a labelled equation with an optional Phase 10 `fignumber`
    /// override (a dotted string such as `2.3`).
    pub fn note_equation_with_number(&self, docname: &str, label: &str, fignumber: Option<&str>) {
        let number = fignumber
            .and_then(|value| value.rsplit('.').next())
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_else(|| {
                let mut next = self.next_number.lock();
                *next += 1;
                *next
            });
        self.data.write().objects.insert(label.to_owned(), (docname.to_owned(), number));
    }
    /// Returns the equation number for a label.
    pub fn equation_number(&self, label: &str) -> Option<usize> {
        self.data.read().objects.get(label).map(|(_, number)| *number)
    }
    /// Documentation for a label (its owning docname).
    pub fn equation_doc(&self, label: &str) -> Option<String> {
        self.data.read().objects.get(label).map(|(docname, _)| docname.clone())
    }
}

impl Domain for MathDomain {
    fn name(&self) -> &str { "math" }
    fn label(&self) -> &str { "mathematics" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { &[] }
    fn role_names(&self) -> &[&'static str] { ROLES }
    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &MathDomainData::default()).unwrap_or_default()
    }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, docname: &str) {
        self.data.write().objects.retain(|_, (doc, _)| doc != docname);
    }
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<MathDomainData>() else { return };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (label, (doc, number)) in other.objects {
            if owned.contains(doc.as_str()) && !data.objects.contains_key(&label) {
                data.objects.insert(label, (doc, number));
            }
        }
    }
    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            if node.kind != NodeKind::MathBlock { continue; }
            // Labelled equations carry their label in `ids` / `names` / `label`.
            let labels = node.ids().unwrap_or_default().into_iter()
                .chain(node.names().unwrap_or_default())
                .chain(node.get_str("label").ok().flatten().map(str::to_owned));
            // Phase 09/10 write `fignumber` onto numbered display math; when
            // present it supersedes the sequential equation counter.
            let fignumber = node.get_str("fignumber").ok().flatten().map(str::to_owned);
            for label in labels {
                if !label.is_empty() && !label.starts_with("equation-") {
                    self.note_equation_with_number(ctx.docname, &label, fignumber.as_deref());
                }
            }
        }
        Ok(())
    }
    fn resolve_xref(
        &self, ctx: &DomainContext<'_>, typ: &str, target: &str,
        _node_id: NodeId, _contnode_id: NodeId, _document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        if typ != "eq" && typ != "numref" { return Ok(None); }
        let Some((docname, number)) = self.data.read().objects.get(target).cloned() else {
            return Ok(None);
        };
        // `math_eqref_format` defaults to `({number})`; explicit formats may
        // also use `{name}`.
        let format = ctx.get_str("math_eqref_format").unwrap_or("({number})");
        let title = format
            .replace("{number}", &number.to_string())
            .replace("{name}", target)
            .replace("%s", &number.to_string());
        let mut reference = ResolvedReference::new()
            .with_node_kind(ReferenceNodeKind::NumberReference)
            .with_reftitle(format.to_owned());
        reference.title = Some(title);
        reference.docname = Some(docname.clone());
        let anchor = format!("equation-{}", target);
        if ctx.docname == docname {
            reference.refid = Some(anchor);
        } else {
            reference.refuri = Some(format!("{}#{}", ctx.relative_uri(ctx.docname, &docname), anchor));
        }
        reference.classes = vec!["xref".to_owned(), "math".to_owned(), "math-numref".to_owned()];
        Ok(Some(reference))
    }
    fn resolve_any_xref(
        &self, ctx: &DomainContext<'_>, target: &str,
        node_id: NodeId, contnode_id: NodeId, document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        self.resolve_xref(ctx, "eq", target, node_id, contnode_id, document)
            .map(|reference| reference.into_iter().map(|reference| ("math:eq".to_owned(), reference)).collect())
    }
    fn get_objects(&self) -> Vec<ObjectEntry> {
        self.data.read().objects.iter().map(|(label, (docname, _number))| {
            ObjectEntry::new(label.clone(), label.clone(), "equation", docname.clone(), format!("equation-{label}"), 1)
        }).collect()
    }
    fn export_data(&self) -> DomainResult<DomainData> {
        DomainData::from_payload(self.data_version(), &self.snapshot())
    }
    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()> {
        if data.version != self.data_version() { return Ok(()); }
        let loaded: MathDomainData = data.decode()?;
        let next = loaded.objects.values().map(|(_, number)| *number).max().unwrap_or(0);
        *self.data.write() = loaded;
        *self.next_number.lock() = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert!(object_types().is_empty());
        assert_eq!(ROLES, ["numref", "eq"]);
    }

    #[test]
    fn equations_are_numbered_sequentially() {
        let domain = MathDomain::new();
        domain.note_equation("index", "eq1");
        domain.note_equation("index", "eq2");
        assert_eq!(domain.equation_number("eq1"), Some(1));
        assert_eq!(domain.equation_number("eq2"), Some(2));
    }
}
