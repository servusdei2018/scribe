//! The standard domain (`std`) — faithful port of `sphinx/domains/std`.
//!
//! Only the inventory and resolution logic lives here; the RST parser emits
//! the frozen AST nodes (including `pending_xref`s) and the pipeline applies
//! the returned [`ResolvedReference`]s.  Every object table is keyed by a
//! flattened string (`type \\x1f name`) so it round-trips through JSON.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use scribe_ast::{Document, Node, NodeId, NodeKind};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

use crate::diagnostics::{DomainDiagnostic, DomainSeverity};
use crate::error::DomainResult;
use crate::traits::{docname_join, Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

const SEP: char = '\u{1f}';

/// Sphinx's `ws_re = re.compile(r'\s+')` normalization.
pub fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sphinx/docutils `nodes.make_id`: NFKD, lowercase, `[^a-z0-9]` → `-`.
pub fn make_id(prefix: &str, name: &str) -> String {
    let combined = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}-{name}")
    };
    let normalized: String = combined
        .nfkd()
        .filter_map(|ch| {
            if is_combining_mark(ch) {
                None // NFKD decomposes `ä` into `a` + diaeresis; docutils drops the mark.
            } else if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else {
                Some('-')
            }
        })
        .collect();
    let mut result = String::new();
    let mut previous_dash = false;
    for ch in normalized.chars() {
        if ch == '-' {
            if !previous_dash && !result.is_empty() {
                result.push(ch);
            }
            previous_dash = true;
        } else {
            result.push(ch);
            previous_dash = false;
        }
    }
    result.trim_matches('-').to_owned()
}

fn object_key(objtype: &str, name: &str) -> String {
    format!("{objtype}{SEP}{name}")
}

fn program_key(program: Option<&str>, option: &str) -> String {
    format!("{}{SEP}{option}", program.unwrap_or_default())
}

fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(SEP)
}

/// Mutable inventory of the standard domain.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandardDomainData {
    /// `(objtype, name) → (docname, labelid)`.
    pub objects: BTreeMap<String, (String, String)>,
    /// Lowercased glossary term → `(docname, labelid)` (case-insensitive fallback).
    pub terms: BTreeMap<String, (String, String)>,
    /// `(program, option) → (docname, labelid)`; `program` is empty for global options.
    pub progoptions: BTreeMap<String, (String, String)>,
    /// Label → `(docname, labelid, sectionname)` (named labels).
    pub labels: BTreeMap<String, (String, String, String)>,
    /// Label → `(docname, labelid)` (anonymous-only labels).
    pub anonlabels: BTreeMap<String, (String, String)>,
    /// Downloadable file → `(docname, generated filename)`.
    pub downloads: BTreeMap<String, (String, String)>,
}

impl StandardDomainData {
    fn fresh() -> Self {
        let mut data = Self::default();
        // Upstream `initial_data`: built-in virtual labels.
        for (label, docname, title) in [
            ("genindex", "genindex", "Index"),
            ("modindex", "py-modindex", "Module Index"),
            ("search", "search", "Search Page"),
        ] {
            data.labels.insert(label.to_owned(), (docname.to_owned(), String::new(), title.to_owned()));
            data.anonlabels.insert(label.to_owned(), (docname.to_owned(), String::new()));
        }
        data
    }

    fn retain_docs(&mut self, docname: &str) {
        self.objects.retain(|_, value| value.0 != docname);
        self.terms.retain(|_, value| value.0 != docname);
        self.progoptions.retain(|_, value| value.0 != docname);
        self.labels.retain(|_, value| value.0 != docname);
        self.anonlabels.retain(|_, value| value.0 != docname);
        self.downloads.retain(|_, value| value.0 != docname);
    }
}

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();
static DIRECTIVES: OnceLock<Vec<&'static str>> = OnceLock::new();
static ROLES: OnceLock<Vec<&'static str>> = OnceLock::new();

fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("term", "glossary term", &["term"]).with_priority(-1),
            ObjectType::new("token", "grammar token", &["token"]).with_priority(-1),
            ObjectType::new("label", "reference label", &["ref", "keyword"]).with_priority(-1),
            ObjectType::new("confval", "configuration value", &["confval"]),
            ObjectType::new("event", "event", &["event"]),
            ObjectType::new("envvar", "environment variable", &["envvar"]),
            ObjectType::new("cmdoption", "program option", &["option"]),
            ObjectType::new("doc", "document", &["doc"]).with_priority(-1),
        ]
    })
}

fn directive_names() -> &'static [&'static str] {
    DIRECTIVES.get_or_init(|| {
        vec!["program", "cmdoption", "option", "confval", "envvar", "glossary", "productionlist", "event"]
    })
}

fn role_names() -> &'static [&'static str] {
    ROLES.get_or_init(|| {
        vec![
            "option", "confval", "envvar", "token", "term", "ref", "numref", "keyword", "doc",
            "download", "label", "event", "any",
        ]
    })
}

/// The `std` domain implementation.
pub struct StandardDomain {
    data: RwLock<StandardDomainData>,
}

impl Default for StandardDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for StandardDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandardDomain")
            .field("objects", &self.data.read().objects.len())
            .field("labels", &self.data.read().labels.len())
            .finish_non_exhaustive()
    }
}

impl StandardDomain {
    /// Creates a domain with Sphinx's initial data (genindex/modindex/search).
    pub fn new() -> Self {
        Self { data: RwLock::new(StandardDomainData::fresh()) }
    }

    /// Creates a domain from a persisted snapshot.
    pub fn from_data(data: StandardDomainData) -> Self {
        Self { data: RwLock::new(data) }
    }

    /// Returns a consistent snapshot of the inventory.
    pub fn snapshot(&self) -> StandardDomainData {
        self.data.read().clone()
    }

    /// Registers a generic object, warning on a conflicting duplicate.
    pub fn note_object(
        &self,
        ctx: &DomainContext<'_>,
        objtype: &str,
        name: &str,
        labelid: &str,
        span: Option<scribe_ast::Span>,
    ) {
        self.note_object_at(ctx, objtype, name, labelid, span, None);
    }

    /// Like [`Self::note_object`], but attaches the source position of the
    /// declaring node so builders can render the located duplicate warning
    /// upstream emits from `StandardDomain.note_object`.
    pub fn note_object_at(
        &self,
        ctx: &DomainContext<'_>,
        objtype: &str,
        name: &str,
        labelid: &str,
        span: Option<scribe_ast::Span>,
        source: Option<&scribe_ast::SourcePos>,
    ) {
        let key = object_key(objtype, name);
        let mut data = self.data.write();
        if let Some((other_doc, _)) = data.objects.get(&key) {
            if other_doc != ctx.docname {
                ctx.push_diagnostic(
                    DomainDiagnostic::duplicate_object(
                        format!(
                            "duplicate {objtype} description of {name}, other instance in {other_doc}"
                        ),
                        objtype,
                        name,
                        Some(ctx.docname.to_owned()),
                        span,
                    )
                    .with_source(source),
                );
            }
        }
        data.objects.insert(key, (ctx.docname.to_owned(), labelid.to_owned()));
    }

    /// Registers a glossary term (exact and case-insensitive forms).
    pub fn note_term(
        &self,
        ctx: &DomainContext<'_>,
        term: &str,
        labelid: &str,
        span: Option<scribe_ast::Span>,
    ) {
        self.note_object(ctx, "term", term, labelid, span);
        self.data
            .write()
            .terms
            .insert(term.to_lowercase(), (ctx.docname.to_owned(), labelid.to_owned()));
    }

    /// Registers a program option; the first declaration wins (upstream).
    pub fn note_program_option(&self, docname: &str, program: Option<&str>, option: &str, labelid: &str) {
        let mut data = self.data.write();
        let key = program_key(program, option);
        data.progoptions
            .entry(key)
            .or_insert_with(|| (docname.to_owned(), labelid.to_owned()));
    }

    /// Registers an explicit label, warning when it was defined elsewhere.
    pub fn note_label(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        labelid: &str,
        title: Option<&str>,
        span: Option<scribe_ast::Span>,
    ) {
        let mut data = self.data.write();
        if let Some((other_doc, _, _)) = data.labels.get(name) {
            if other_doc != ctx.docname {
                ctx.push_diagnostic(DomainDiagnostic::duplicate_object(
                    format!("duplicate label {name}, other instance in {other_doc}"),
                    "label",
                    name,
                    Some(ctx.docname.to_owned()),
                    span,
                ));
            }
        }
        data.anonlabels.insert(name.to_owned(), (ctx.docname.to_owned(), labelid.to_owned()));
        if let Some(title) = title {
            data.labels.insert(
                name.to_owned(),
                (ctx.docname.to_owned(), labelid.to_owned(), title.to_owned()),
            );
        }
    }

    /// Registers a downloadable file under `_downloads/<basename>`.
    pub fn note_download(&self, docname: &str, path: &str) {
        if path.contains("://") {
            return;
        }
        let filename = path.rsplit('/').next().unwrap_or(path).to_owned();
        self.data
            .write()
            .downloads
            .insert(path.to_owned(), (docname.to_owned(), filename));
    }

    // ----- resolution helpers -------------------------------------------------

    fn make_refnode(
        &self,
        ctx: &DomainContext<'_>,
        docname: &str,
        labelid: &str,
        title: Option<String>,
        classes: &[&str],
    ) -> ResolvedReference {
        let mut reference = if ctx.docname == docname && !labelid.is_empty() {
            ResolvedReference::local(labelid, title)
        } else {
            let mut uri = ctx.relative_uri(ctx.docname, docname);
            if !labelid.is_empty() {
                uri.push('#');
                uri.push_str(labelid);
            }
            ResolvedReference::uri(uri, title)
        };
        reference.docname = Some(docname.to_owned());
        reference.classes.extend(classes.iter().map(|class| (*class).to_owned()));
        reference
    }

    fn object_lookup(&self, typ: &str, target: &str) -> Option<(String, String)> {
        let data = self.data.read();
        for object in object_types() {
            if object.roles.iter().any(|role| role == typ) {
                let key = object_key(&object.name, target);
                if let Some(value) = data.objects.get(&key) {
                    return Some(value.clone());
                }
            }
        }
        None
    }

    fn resolve_numref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        cont_title: String,
        explicit: bool,
        node_id: NodeId,
        document: &Document,
    ) -> Option<ResolvedReference> {
        let data = self.data.read();
        let (docname, labelid, figname) = if let Some((docname, labelid, name)) = data.labels.get(target) {
            (docname.clone(), labelid.clone(), Some(name.clone()))
        } else {
            let (docname, labelid) = data.anonlabels.get(target)?.clone();
            (docname, labelid, None)
        };
        drop(data);

        // Upstream logs every numfig diagnostic at the location of the pending
        // xref itself and passes no `type`/`subtype`, so the rendered line has
        // no `[code]` suffix (see `build_common::domain_warning_line`).
        let warn = |message: String| {
            let source = document.get(node_id).ok().and_then(|node| node.source.as_ref());
            ctx.push_diagnostic(
                DomainDiagnostic::bare_warning(
                    message,
                    Some(ctx.docname.to_owned()),
                    ctx.span_of(document, node_id),
                )
                .with_source(source),
            );
        };

        let doctree = ctx.env.and_then(|env| env.doctree(&docname));
        let target_node = doctree.and_then(|tree| {
            let order = tree.preorder_ids();
            let position = order.iter().position(|&id| {
                tree.get(id).is_ok_and(|node| node.ids().unwrap_or_default().contains(&labelid))
            })?;
            let mut node = tree.get(order[position]).ok()?;
            // Our RST parser keeps an explicit `.. _label:` as a standalone
            // target node (upstream's PropagateTargets moves the ids onto the
            // next block). Follow it to the enumerable node it labels.
            if node.kind == NodeKind::Target {
                node = order[position + 1..]
                    .iter()
                    .find_map(|&id| tree.get(id).ok())
                    .filter(|next| next.kind != NodeKind::Target)?;
            }
            Some(node)
        });
        let target_node = target_node?;
        let figtype = match target_node.kind {
            NodeKind::Section => "section",
            NodeKind::Figure => "figure",
            NodeKind::Table => "table",
            NodeKind::Container => "code-block",
            _ => return None,
        };
        if figtype != "section" && !ctx.get_bool("numfig").unwrap_or(false) {
            warn("numfig is disabled. :numref: is ignored.".to_owned());
            return Some(ResolvedReference::default().passthrough());
        }
        let number = numref_number(doctree, target_node, figtype).unwrap_or_default();
        if number.is_empty() {
            warn(format!(
                "Failed to create a cross reference. Any number is not assigned: {labelid}"
            ));
            return Some(ResolvedReference::default().passthrough());
        }

        let format = if explicit {
            cont_title
        } else {
            numfig_format(ctx, figtype)
        };
        if figname.is_none() && format.contains("{name}") {
            warn(format!("the link has no caption: {format}"));
            return Some(ResolvedReference::default().passthrough());
        }
        let newtitle = match apply_numref_format(&format, figname.as_deref(), &number) {
            Some(title) => title,
            None => {
                warn(numfig_format_error_message(&format));
                return Some(ResolvedReference::default().passthrough());
            }
        };
        let mut reference = self.make_refnode(ctx, &docname, &labelid, Some(newtitle), &["std", "std-numref"]);
        reference.node_kind = crate::types::ReferenceNodeKind::NumberReference;
        reference.reftitle = Some(format);
        Some(reference)
    }

    fn resolve_doc(
        &self,
        ctx: &DomainContext<'_>,
        refdoc: &str,
        target: &str,
        cont_title: String,
        explicit: bool,
    ) -> Option<ResolvedReference> {
        // Phase 11 extension: support `:doc:` targets with a `#fragment`.
        // Upstream 9.x rejects them as unknown documents; Scribe appends the
        // fragment to the generated URI instead (documented in §15).
        let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
        // A leading `/` makes the target project-root-relative (upstream
        // `:doc:` semantics); joining it against `refdoc` would duplicate the
        // first path component and yield "unknown document".
        let docname = match path.strip_prefix('/') {
            Some(absolute) => docname_join("", absolute),
            // `./name` is project-root relative in Sphinx's `:doc:` role.
            None if path.starts_with("./") => docname_join("", &path[2..]),
            None => docname_join(refdoc, path),
        };
        if !ctx.env.is_some_and(|env| env.has_doc(&docname)) {
            return None;
        }
        let caption = if explicit {
            cont_title
        } else {
            ctx.env
                .and_then(|env| env.title(&docname))
                .unwrap_or_else(|| docname.clone())
        };
        let mut uri = ctx.relative_uri(ctx.docname, &docname);
        if !fragment.is_empty() {
            uri.push('#');
            uri.push_str(fragment);
        }
        let mut reference = ResolvedReference::uri(uri, Some(caption));
        reference.docname = Some(docname);
        reference.classes = vec!["doc".to_owned()];
        Some(reference)
    }

    fn resolve_option(&self, ctx: &DomainContext<'_>, target: &str) -> Option<ResolvedReference> {
        let data = self.data.read();
        let lookup = |program: Option<&str>, name: &str| -> Option<(String, String)> {
            data.progoptions.get(&program_key(program, name)).cloned()
        };
        let target = target.trim();
        let mut found = lookup(ctx.program, target);
        if found.is_none() {
            // `:option:` may include a value: `-foo=bar`, `-foo[=bar]`, `-foo bar`.
            for needle in ["=", "[=", " "] {
                if let Some((stem, _, _)) = split_once_keep(target, needle) {
                    found = lookup(ctx.program, stem);
                    if found.is_some() {
                        break;
                    }
                }
            }
        }
        if found.is_none() {
            // Subcommand folding: `git commit` looks up program `git` with
            // option `--amend`, then program `git-commit`, etc.
            let mut commands: Vec<&str> = ctx.program.into_iter().collect();
            let mut remainder = target;
            while let Some(index) = remainder.find(char::is_whitespace) {
                let (subcommand, rest) = remainder.split_at(index);
                commands.push(subcommand);
                remainder = rest.trim_start();
                let joined = commands.join("-");
                found = lookup(Some(&joined), remainder);
                if found.is_some() {
                    break;
                }
            }
        }
        let (docname, labelid) = found?;
        let mut reference = self.make_refnode(ctx, &docname, &labelid, None, &[]);
        reference.docname = Some(docname);
        Some(reference)
    }

    fn resolve_term(&self, target: &str) -> Option<(String, String)> {
        let data = self.data.read();
        if let Some(value) = self.object_lookup("term", target) {
            return Some(value);
        }
        data.terms.get(&target.to_lowercase()).cloned()
    }

    fn resolve_download(&self, ctx: &DomainContext<'_>, target: &str) -> ResolvedReference {
        if target.contains("://") {
            return ResolvedReference::uri(target.to_owned(), None)
                .with_node_kind(crate::types::ReferenceNodeKind::DownloadReference);
        }
        let data = self.data.read();
        let filename = data
            .downloads
            .get(target)
            .map(|(_, filename)| filename.clone())
            .unwrap_or_else(|| target.rsplit('/').next().unwrap_or(target).to_owned());
        drop(data);
        let uri = format!("{}{}", ctx.relative_uri(ctx.docname, "_downloads"), "");
        let uri = if uri.ends_with("_downloads") || uri.ends_with("_downloads/") {
            format!("{uri}{filename}")
        } else {
            format!("{uri}/{filename}")
        };
        ResolvedReference::uri(uri, None).with_node_kind(crate::types::ReferenceNodeKind::DownloadReference)
    }

    // ----- inventory collection -----------------------------------------------

    fn collect_from_document(&self, ctx: &DomainContext<'_>, docname: &str, document: &Document) {
        let mut current_program: Option<String> = None;
        let order = document.preorder_ids();
        let ids_in_order: Vec<NodeId> = order.clone();
        for id in ids_in_order {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let span = node.span;
            let names = node.names().unwrap_or_default();
            let ids = node.ids().unwrap_or_default();
            let objtype = node.get_str("objtype").ok().flatten().unwrap_or("").to_owned();
            // Domain-owned description nodes (e.g. Phase 12's `py:*`) carry a
            // `domain` attribute; the standard domain must ignore them even
            // though they expose a generic `objtype`.
            if node.get_str("domain").ok().flatten().is_some_and(|domain| domain != "std") {
                continue;
            }

            // `.. program::` markers are emitted as `desc` nodes with objtype
            // `program` by the RST directive; the name is the first `names`
            // entry (or the signature text).
            if objtype == "program" {
                let name = names.first().cloned().unwrap_or_else(|| node.astext(document).trim().to_owned());
                current_program = if name == "None" { None } else { Some(normalize_ws(&name).replace(' ', "-")) };
                continue;
            }

            if !objtype.is_empty() && !ids.is_empty() {
                let name = names.first().cloned().unwrap_or_else(|| node.astext(document).trim().to_owned());
                let labelid = ids.first().cloned().unwrap_or_default();
                if objtype == "option" || objtype == "cmdoption" {
                    // Upstream's anchor is `cmdoption[-program]-name`, not the
                    // parser's generic `option-name` id.
                    let prefix = current_program
                        .as_deref()
                        .map_or_else(|| "cmdoption".to_owned(), |program| format!("cmdoption-{program}"));
                    let anchor = make_id(&prefix, &name);
                    self.note_program_option(docname, current_program.as_deref(), &name, &anchor);
                    // Aliases from `allnames` are registered as well.
                    for alias in node.get_list("allnames").ok().flatten().unwrap_or_default() {
                        let alias_anchor = make_id(&prefix, &alias);
                        self.note_program_option(docname, current_program.as_deref(), &alias, &alias_anchor);
                    }
                } else {
                    // A single directive may declare several aliases (e.g.
                    // `.. confval:: master_doc\n   root_doc`). Register every
                    // name with its matching anchor, not just the first.
                    for (index, alias) in names.iter().enumerate() {
                        let alias_id = ids.get(index).unwrap_or(&labelid);
                        self.note_object(ctx, &objtype, alias, alias_id, span);
                    }
                }
            }

            if node.kind == NodeKind::Glossary {
                self.collect_glossary_terms(ctx, document, id, span);
            }
            if node.kind == NodeKind::Productionlist {
                self.collect_productions(ctx, document, id, span);
            }
            if node.kind == NodeKind::DownloadReference {
                if let Some(target) = node.get_str("reftarget").ok().flatten() {
                    self.note_download(docname, target);
                }
            }

            // Explicit labels: a `target` node still carrying names, or a node
            // whose ids were the recipient of a propagated target.  Implicit
            // section titles (which also carry `names`) are deliberately
            // excluded, matching `StandardDomain.process_doc`'s nametypes test.
            if node.kind == NodeKind::Target {
                for name in &names {
                    if node.get_str("refuri").ok().flatten().is_some() {
                        continue;
                    }
                    let labelid = ids.first().cloned().unwrap_or_default();
                    let title = title_for_label(document, id);
                    self.note_label(ctx, name, &labelid, title.as_deref(), span);
                }
            } else if matches!(node.kind, NodeKind::Container | NodeKind::LiteralBlock)
                && (node.kind != NodeKind::Container
                    || node.attrs.get_bool("literal_block").unwrap_or_default().unwrap_or(false))
                && !names.is_empty()
                && !ids.is_empty()
            {
                // `.. code-block::` with `:name:` stores the explicit target
                // directly on its wrapper container (upstream's
                // `literal_block` enumerable node). Without a `:caption:` the
                // name lives on the `literal_block` node itself. Register both
                // shapes so `:ref:`/`:numref:` can resolve them.
                let labelid = ids.first().cloned().unwrap_or_default();
                let title = title_for_label(document, id);
                for name in &names {
                    self.note_label(ctx, name, &labelid, title.as_deref(), span);
                }
            } else if node.kind != NodeKind::Section {
                let propagated = ids.iter().any(|labelid| {
                    document.get(id).is_ok_and(|node| node.get_str("refid").ok().flatten() == Some(labelid.as_str()))
                        || order.iter().any(|&maybe_target| {
                            document.get(maybe_target).is_ok_and(|target| {
                                target.kind == NodeKind::Target
                                    && target.get_str("refid").ok().flatten() == Some(labelid.as_str())
                            })
                        })
                });
                if propagated {
                    for name in &names {
                        let labelid = ids.first().cloned().unwrap_or_default();
                        let title = title_for_label(document, id);
                        self.note_label(ctx, name, &labelid, title.as_deref(), span);
                    }
                }
            }
        }
    }

    fn collect_glossary_terms(
        &self,
        ctx: &DomainContext<'_>,
        document: &Document,
        glossary: NodeId,
        span: Option<scribe_ast::Span>,
    ) {
        let mut group: Vec<(NodeId, String)> = Vec::new();
        for child in document.subtree_ids(glossary) {
            let Ok(node) = document.get(child) else {
                continue;
            };
            if node.kind != NodeKind::Term {
                continue;
            }
            let text = node.astext(document).trim().to_owned();
            if text.is_empty() {
                continue;
            }
            group.push((child, text));
        }
        for (index, (id, text)) in group.iter().enumerate() {
            let labelid = document
                .get(*id)
                .ok()
                .and_then(|node| node.ids().ok())
                .and_then(|ids| ids.first().cloned())
                .unwrap_or_else(|| make_id("term", text));
            self.note_term(ctx, text, &labelid, span);
            // Upstream's older `seealso` aliases: every additional term in a
            // glossary definition points at the first term's anchor.
            if index > 0 {
                if let Some((_, first_text)) = group.first() {
                    let first_id = make_id("term", first_text);
                    self.note_term(ctx, text, &first_id, span);
                }
            }
        }
    }

    fn collect_productions(
        &self,
        ctx: &DomainContext<'_>,
        document: &Document,
        productionlist: NodeId,
        span: Option<scribe_ast::Span>,
    ) {
        for child in document.subtree_ids(productionlist) {
            let Ok(node) = document.get(child) else {
                continue;
            };
            if node.kind != NodeKind::Production {
                continue;
            }
            let name = node.get_str("tokenname").ok().flatten().unwrap_or("").to_owned();
            if name.is_empty() {
                continue;
            }
            let anchor = document
                .get(child)
                .ok()
                .and_then(|production| production.children.first().copied())
                .and_then(|target| document.get(target).ok())
                .and_then(|node| node.ids().ok())
                .and_then(|ids| ids.first().cloned())
                .unwrap_or_else(|| make_id("grammar-token", &name));
            // Upstream qualifies token names with the production group, so
            // `P1:A` and `P2:A` are distinct targets (and `A` its own target).
            // The parser encodes that group in the anchor id emitted by
            // `grammar-token-{group}-{name}`, so recover it from there without
            // adding an extra attribute to the frozen AST.
            let group = anchor
                .strip_prefix("grammar-token-")
                .and_then(|rest| rest.strip_suffix(&format!("-{name}")))
                .unwrap_or("");
            let obj_name = if group.is_empty() {
                name
            } else {
                format!("{group}:{name}")
            };
            self.note_object_at(ctx, "token", &obj_name, &anchor, span, node.source.as_ref());
        }
    }
}

fn title_for_label(document: &Document, id: NodeId) -> Option<String> {
    let node = document.get(id).ok()?;
    match node.kind {
        NodeKind::Target => {
            // Our RST parser keeps an explicit `.. _label:` as a standalone
            // target node rather than propagating its ids onto the next block
            // (upstream `PropagateTargets`).  Upstream titles such labels from
            // the node that follows, so do the same here; otherwise `:ref:`
            // only sees an anonymous label and emits "A title or caption not
            // found" for what is really a perfectly good section label.
            let parent = document.parent(id).ok().flatten()?;
            let siblings = document.children(parent).ok()?;
            let position = siblings.iter().position(|&child| child == id)?;
            if let Some(title) = siblings[position + 1..]
                .iter()
                .copied()
                .find(|&next| document.get(next).is_ok_and(|node| node.kind != NodeKind::Target))
                .and_then(|next| title_for_label(document, next))
            {
                return Some(title);
            }
            // Explicit targets are sometimes appended to the end of their
            // container while the labelled section stays nested at its source
            // position.  The section's span starts immediately after the
            // target's, so pick the nearest enumerable node after it.
            let start = node.span.map(|span| span.end)?;
            let next = document
                .subtree_ids(document.root)
                .iter()
                .copied()
                .filter_map(|candidate| {
                    let candidate_node = document.get(candidate).ok()?;
                    let candidate_start = candidate_node.span.map(|span| span.start)?;
                    if candidate_start < start || !matches!(candidate_node.kind, NodeKind::Section | NodeKind::Figure | NodeKind::Table) {
                        return None;
                    }
                    Some((candidate_start, candidate))
                })
                .min_by_key(|&(span_start, _)| span_start)
                .map(|(_, candidate)| candidate)?;
            title_for_label(document, next)
        }
        NodeKind::Section => document
            .children(id)
            .ok()?
            .iter()
            .find(|&&child| document.get(child).is_ok_and(|node| node.kind == NodeKind::Title))
            .and_then(|&child| document.get(child).ok())
            .map(|title| title.astext(document).trim().to_owned()),
        NodeKind::Rubric | NodeKind::Term | NodeKind::FieldName | NodeKind::Title | NodeKind::Caption => {
            Some(node.astext(document).trim().to_owned())
        }
        NodeKind::Figure | NodeKind::Table | NodeKind::Container => document
            .children(id)
            .ok()?
            .iter()
            .find(|&&child| {
                document.get(child).is_ok_and(|node| matches!(node.kind, NodeKind::Caption | NodeKind::Title))
            })
            .and_then(|&child| document.get(child).ok())
            .map(|title| title.astext(document).trim().to_owned()),
        NodeKind::Toctree => None,
        _ => {
            // Definition-list / field-list labels: use the first term/name.
            for child in document.subtree_ids(id) {
                if let Ok(inner) = document.get(child) {
                    if matches!(inner.kind, NodeKind::Term | NodeKind::FieldName) {
                        return Some(inner.astext(document).trim().to_owned());
                    }
                }
            }
            None
        }
    }
}

fn numfig_format(ctx: &DomainContext<'_>, figtype: &str) -> String {
    let configured = ctx
        .config
        .and_then(|config| config.get_dict("numfig_format"))
        .and_then(|dict| dict.get(figtype))
        .and_then(|value| value.get_str())
        .map(str::to_owned);
    configured.unwrap_or_else(|| match figtype {
        "figure" => "Fig. %s".to_owned(),
        "table" => "Table %s".to_owned(),
        "code-block" => "Listing %s".to_owned(),
        "section" => "Section %s".to_owned(),
        _ => "%s".to_owned(),
    })
}

fn numref_number(document: Option<&Document>, node: &Node, figtype: &str) -> Option<String> {
    if let Some(value) = node.get_str("fignumber").ok().flatten() {
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    if figtype == "section" {
        if let Some(value) = node.get_str("secnumber").ok().flatten() {
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    let _ = document;
    None
}

fn apply_numref_format(format: &str, figname: Option<&str>, number: &str) -> Option<String> {
    // Upstream selects the new-style branch when the format mentions
    // `{name}` or the word "number" (cf. `Fig.{number}`).
    if format.contains("{name}") || format.contains("number") {
        if format.contains("{name}") && figname.is_none() {
            return None;
        }
        // Python's `str.format(name=..., number=...)` raises KeyError for any
        // other placeholder; surface that as a failed application.
        if unknown_format_field(format).is_some() {
            return None;
        }
        let mut result = format.to_owned();
        if let Some(name) = figname {
            result = result.replace("{name}", name);
        }
        Some(result.replace("{number}", number))
    } else if format.contains("%s") {
        // Old-style `%` with the single `fignum` argument raises TypeError
        // when more than one conversion is requested.
        if format.matches("%s").count() > 1 {
            return None;
        }
        Some(format.replacen("%s", number, 1))
    } else {
        Some(format.to_owned())
    }
}

/// First `{...}` placeholder in `format` that is neither `name` nor `number`
/// (ignoring Python's doubled-brace escapes). Mirrors the `KeyError` upstream
/// catches while formatting a `numfig_format` string.
fn unknown_format_field(format: &str) -> Option<&str> {
    let bytes = format.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if bytes.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            let rest = &format[i + 1..];
            let end = rest.find('}')?;
            let field = &rest[..end];
            if field != "name" && field != "number" {
                return Some(field);
            }
            i += end + 2;
        } else {
            i += 1;
        }
    }
    None
}

/// Exact upstream message for a `numfig_format` that raised while formatting.
/// A missing placeholder is a `KeyError` (its repr is embedded); an
/// incompatible `%s` count is a `TypeError` (bare message).
fn numfig_format_error_message(format: &str) -> String {
    if let Some(field) = unknown_format_field(format) {
        format!("invalid numfig_format: {format} ({})", py_repr(field))
    } else {
        format!("invalid numfig_format: {format}")
    }
}

fn split_once_keep<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let index = text.find(needle)?;
    Some((&text[..index], &text[index..index + needle.len()], &text[index + needle.len()..]))
}

impl Domain for StandardDomain {
    fn name(&self) -> &str {
        "std"
    }

    fn label(&self) -> &str {
        "Default"
    }

    fn object_types(&self) -> &[ObjectTypeAlias] {
        object_types()
    }

    fn directive_names(&self) -> &[&'static str] {
        directive_names()
    }

    fn role_names(&self) -> &[&'static str] {
        role_names()
    }

    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(1, &StandardDomainData::fresh()).unwrap_or_default()
    }

    fn data_version(&self) -> u32 {
        1
    }

    fn clear_doc(&mut self, docname: &str) {
        self.data.write().retain_docs(docname);
    }

    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<StandardDomainData>() else {
            return;
        };
        let mut data = self.data.write();
        let owned: std::collections::BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        for (key, value) in other.objects {
            if owned.contains(value.0.as_str()) {
                data.objects.insert(key, value);
            }
        }
        for (key, value) in other.terms {
            if owned.contains(value.0.as_str()) {
                data.terms.insert(key, value);
            }
        }
        for (key, value) in other.progoptions {
            if owned.contains(value.0.as_str()) {
                data.progoptions.insert(key, value);
            }
        }
        for (key, value) in other.labels {
            if owned.contains(value.0.as_str()) {
                data.labels.insert(key, value);
            }
        }
        for (key, value) in other.anonlabels {
            if owned.contains(value.0.as_str()) {
                data.anonlabels.insert(key, value);
            }
        }
        for (key, value) in other.downloads {
            if owned.contains(value.0.as_str()) {
                data.downloads.insert(key, value);
            }
        }
    }

    fn process_doc(&mut self, ctx: &DomainContext<'_>, docname: &str, document: &Document) -> DomainResult<()> {
        self.collect_from_document(ctx, docname, document);
        Ok(())
    }

    fn check_consistency(&self, ctx: &DomainContext<'_>) -> Vec<DomainDiagnostic> {
        let data = self.data.read();
        let mut diagnostics = Vec::new();
        for (key, (docname, labelid)) in &data.objects {
            if labelid.is_empty() && !key.ends_with(SEP) {
                if let Some((name, _)) = split_key(key) {
                    if name != "doc" {
                        diagnostics.push(DomainDiagnostic::inconsistency(
                            format!("object {name} in {docname} has no anchor"),
                            Some(docname.clone()),
                            None,
                        ));
                    }
                }
            }
        }
        let _ = ctx;
        diagnostics
    }

    fn resolve_xref(
        &self,
        ctx: &DomainContext<'_>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        let _ = (node_id, document);
        let cont_title = document.get(contnode_id).map(|node| node.astext(document)).unwrap_or_default();
        let explicit = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refexplicit())
            .unwrap_or(false);
        let lowered = target.to_lowercase();
        let result = match typ {
            "ref" => {
                let data = self.data.read();
                if explicit {
                    data.anonlabels.get(&lowered).map(|(docname, labelid)| {
                        self.make_refnode(ctx, docname, labelid, Some(cont_title.clone()), &["std", "std-ref"])
                    })
                } else {
                    data.labels.get(&lowered).map(|(docname, labelid, sectname)| {
                        let title = if sectname.is_empty() { None } else { Some(sectname.clone()) };
                        self.make_refnode(ctx, docname, labelid, title, &["std", "std-ref"])
                    })
                }
            }
            "numref" => self.resolve_numref(ctx, &lowered, cont_title, explicit, node_id, document),
            "keyword" | "label" => {
                let data = self.data.read();
                data.labels.get(target).map(|(docname, labelid, _)| {
                    self.make_refnode(ctx, docname, labelid, None, &["std", "std-keyword"])
                })
            }
            "doc" => {
                let refdoc = document
                    .get(node_id)
                    .ok()
                    .and_then(|node| node.pending_xref())
                    .and_then(|xref| xref.refdoc().map(str::to_owned))
                    .unwrap_or_else(|| ctx.docname.to_owned());
                self.resolve_doc(ctx, &refdoc, target, cont_title, explicit)
            }
            "option" => self.resolve_option(ctx, target),
            "term" => self.resolve_term(target).map(|(docname, labelid)| {
                self.make_refnode(ctx, &docname, &labelid, None, &["std", "std-term"])
            }),
            "download" => Some(self.resolve_download(ctx, target)),
            _ => self.object_lookup(typ, target).map(|(docname, labelid)| {
                self.make_refnode(ctx, &docname, &labelid, None, &["std", &format!("std-{typ}")])
            }),
        };
        Ok(result)
    }

    fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let mut results = Vec::new();
        let lowered = target.to_lowercase();
        // Upstream tries `ref` (lowercased) and `option` first, then all
        // object types.  We resolve through `resolve_xref` so the two paths
        // cannot diverge.
        for role in ["ref", "option"] {
            if let Some(reference) = self.resolve_xref(ctx, role, if role == "ref" { &lowered } else { target }, node_id, contnode_id, document)? {
                results.push((format!("std:{role}"), reference));
            }
        }
        for object in object_types() {
            let lookup = if object.name == "term" { &lowered } else { target };
            let data = self.data.read();
            let entry = data.objects.get(&object_key(&object.name, lookup)).cloned();
            drop(data);
            if let Some((docname, labelid)) = entry {
                let role = object.roles.first().map_or("obj", String::as_str);
                let reference = self.make_refnode(ctx, &docname, &labelid, None, &["std", &format!("std-{role}")]);
                results.push((format!("std:{role}"), reference));
            }
        }
        Ok(results)
    }

    fn get_objects(&self) -> Vec<ObjectEntry> {
        let data = self.data.read();
        let mut entries = Vec::new();
        // Whole-document entries are merged by `DomainRegistry::get_objects`
        // (which has the environment); see ARCHITECTURE.md §15.
        for (prog_option, (docname, anchor)) in &data.progoptions {
            if let Some((program, option)) = split_key(prog_option) {
                let name = if program.is_empty() {
                    option.to_owned()
                } else {
                    format!("{program}.{option}")
                };
                entries.push(ObjectEntry::new(name.clone(), name, "cmdoption", docname, anchor, 1));
            }
        }
        for (key, (docname, anchor)) in &data.objects {
            if let Some((objtype, name)) = split_key(key) {
                let priority = object_types()
                    .iter()
                    .find(|object| object.name == objtype)
                    .map_or(1, |object| object.priority);
                entries.push(ObjectEntry::new(name, name, objtype, docname, anchor, priority));
            }
        }
        for (name, (docname, anchor, sectionname)) in &data.labels {
            entries.push(ObjectEntry::new(name, sectionname, "label", docname, anchor, -1));
        }
        for (name, (docname, anchor)) in &data.anonlabels {
            if !data.labels.contains_key(name) {
                entries.push(ObjectEntry::new(name, name, "label", docname, anchor, -1));
            }
        }
        for (path, (docname, filename)) in &data.downloads {
            entries.push(ObjectEntry::new(path, filename, "download", docname, filename, 1));
        }
        // Upstream yields documents first; they are supplied by the registry
        // (which has the environment) and merged there.  Within each group the
        // order is deterministic by (name, docname, anchor).
        entries.sort_by(|a, b| {
            (a.objtype.as_str(), a.name.as_str(), a.docname.as_str(), a.anchor.as_str())
                .cmp(&(b.objtype.as_str(), b.name.as_str(), b.docname.as_str(), b.anchor.as_str()))
        });
        entries
    }

    fn get_type_name(&self, objtype: &str, primary: bool) -> String {
        let _ = primary;
        object_types()
            .iter()
            .find(|object| object.name == objtype)
            .map_or_else(|| objtype.to_owned(), |object| object.lname.clone())
    }

    fn get_full_qualified_name(&self, node: &Node) -> Option<String> {
        if node.get_str("reftype").ok().flatten() != Some("option") {
            return None;
        }
        let program = node.get_str("std:program").ok().flatten().unwrap_or("");
        let mut command: Vec<String> = node
            .get_str("reftarget")
            .ok()
            .flatten()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !program.is_empty() {
            command.insert(0, program.to_owned());
        }
        let option = command.pop()?;
        if command.is_empty() {
            None
        } else {
            Some(format!("{}.{option}", command.join("-")))
        }
    }

    fn export_data(&self) -> DomainResult<DomainData> {
        DomainData::from_payload(self.data_version(), &self.snapshot())
    }

    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()> {
        if data.version != self.data_version() {
            return Ok(());
        }
        let loaded: StandardDomainData = data.decode()?;
        *self.data.write() = loaded;
        Ok(())
    }
}

/// Python `repr()` of a string (single quotes, as Sphinx's `%r` prints).
pub fn py_repr(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

/// Dangling-reference messages from `StandardDomain.dangling_warnings`.
pub fn dangling_warning(typ: &str, target: &str) -> Option<String> {
    let target = py_repr(target);
    Some(match typ {
        "term" => format!("term not in glossary: {target}"),
        "numref" => format!("undefined label: {target}"),
        "keyword" => format!("unknown keyword: {target}"),
        "doc" => format!("unknown document: {target}"),
        "option" => format!("unknown option: {target}"),
        _ => return None,
    })
}

/// Builds the exact `warn-missing-reference` message for `:ref:`.
pub fn ref_missing_message(target: &str, has_anonlabel: bool) -> String {
    let target = py_repr(target);
    if has_anonlabel {
        format!("Failed to create a cross reference. A title or caption not found: {target}")
    } else {
        format!("undefined label: {target}")
    }
}

/// Severity used for all standard-domain warnings.
pub fn warning_severity() -> DomainSeverity {
    DomainSeverity::Warning
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_id_matches_docutils_examples() {
        assert_eq!(make_id("", "Hello World"), "hello-world");
        assert_eq!(make_id("term", "foo_bar"), "term-foo-bar");
        assert_eq!(make_id("", "  umlauts äre  "), "umlauts-are");
    }

    #[test]
    fn numref_format_both_styles() {
        assert_eq!(apply_numref_format("Fig. %s", None, "1.2"), Some("Fig. 1.2".into()));
        assert_eq!(
            apply_numref_format("Fig. {number}", None, "3"),
            Some("Fig. 3".into())
        );
        assert_eq!(
            apply_numref_format("Fig {name}.{number}", Some("Caption"), "2"),
            Some("Fig Caption.2".into())
        );
        assert_eq!(apply_numref_format("Fig {name}", None, "2"), None);
    }

    #[test]
    fn dangling_messages_are_exact() {
        assert_eq!(dangling_warning("term", "foo").as_deref(), Some("term not in glossary: 'foo'"));
        assert_eq!(ref_missing_message("no-label", false), "undefined label: 'no-label'");
        assert_eq!(
            ref_missing_message("label", true),
            "Failed to create a cross reference. A title or caption not found: 'label'"
        );
    }
}
