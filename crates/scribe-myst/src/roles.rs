//! MyST inline roles lowered to the same nodes as RST roles.
//!
//! `` {role}`payload` `` (and `` {prefix:role}`payload` ``) are intercepted by
//! the block lowering engine and routed here. We reuse `scribe-rst`'s
//! [`classify_builtin`](scribe_rst::role_registry::classify_builtin) as the
//! single source of truth for which node a role produces, and mirror the exact
//! attribute payload that `scribe-rst`'s inline recogniser emits (§9.4 of
//! `ARCHITECTURE.md`), so RST and MyST roles are byte-for-byte equivalent at
//! the AST level.

use scribe_ast::{Document, Node, NodeId, NodeKind, SourcePos, Span};
use scribe_rst::role_registry::{classify_builtin, DirectBehavior, RoleClass};
use scribe_rst::XrefMeta;

use crate::error::MystResult;
use crate::names::whitespace_normalize_name;

/// Classifies a role through the shared registry, with the same
/// domain-qualified fallback the RST inline parser uses.
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

/// Emits a role invocation under `parent` and returns its node ids plus the
/// optional pending-xref side-table entry.
#[allow(clippy::too_many_arguments)]
pub fn emit_role(
    role: &str,
    payload: &str,
    full_span: Span,
    payload_span: Span,
    line: u32,
    source_name: Option<&str>,
    doc: &mut Document,
    parent: NodeId,
) -> MystResult<(Vec<NodeId>, Option<XrefMeta>)> {
    let Some(class) = classify_role(role) else {
        // Unknown role → `Problematic`, matching Docutils/Sphinx.
        let mut node = Node::new(NodeKind::Problematic);
        node.span = Some(full_span);
        node.source = Some(SourcePos::new(None, Some(line), None));
        let id = doc.add_node(node, Some(parent))?;
        let mut text = Node::text(payload.to_owned());
        text.span = Some(payload_span);
        doc.add_node(text, Some(id))?;
        return Ok((vec![id], None));
    };

    match class {
        RoleClass::Direct { kind, classes, behavior, .. } => {
            emit_direct(role, kind, classes, behavior, payload, full_span, payload_span, line, doc, parent)
        }
        RoleClass::External { prefix, title_prefix, classes } => {
            emit_external(role, &prefix, &title_prefix, classes, payload, full_span, payload_span, line, doc, parent)
        }
        RoleClass::Xref { domain, reftype, inner, warn } => emit_xref(
            role,
            &domain,
            &reftype,
            inner,
            warn,
            payload,
            full_span,
            payload_span,
            line,
            source_name,
            doc,
            parent,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_direct(
    role: &str,
    kind: NodeKind,
    classes: Vec<String>,
    behavior: DirectBehavior,
    payload: &str,
    full_span: Span,
    payload_span: Span,
    line: u32,
    doc: &mut Document,
    parent: NodeId,
) -> MystResult<(Vec<NodeId>, Option<XrefMeta>)> {
    if behavior == DirectBehavior::Keyboard {
        let mut ids = Vec::new();
        let mut index = 0usize;
        for (piece, is_separator) in split_keyboard(payload) {
            let len = piece.len();
            let span = span_from_local(payload_span, index, len);
            if is_separator {
                let mut text = Node::text(piece);
                text.span = Some(span);
                ids.push(doc.add_node(text, Some(parent))?);
            } else {
                let mut node = Node::new(kind);
                node.span = Some(span);
                node.source = Some(SourcePos::new(None, Some(line), None));
                if !classes.is_empty() {
                    node.attrs.set_list("classes", classes.clone());
                }
                let id = doc.add_node(node, Some(parent))?;
                let mut text = Node::text(piece);
                text.span = Some(span);
                doc.add_node(text, Some(id))?;
                ids.push(id);
            }
            index += len;
        }
        return Ok((ids, None));
    }

    let mut node = Node::new(kind);
    node.span = Some(full_span);
    node.source = Some(SourcePos::new(None, Some(line), None));
    if !classes.is_empty() {
        node.attrs.set_list("classes", classes);
    }
    let mut text = payload.to_owned();
    match behavior {
        DirectBehavior::Abbreviation => {
            if let Some(index) = payload.rfind(" (") {
                if payload.ends_with(')') {
                    node.attrs.set_str(
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
            node.attrs.set_str("path", payload.to_owned());
            node.attrs.set_str("page", page);
            node.attrs.set_str("section", section);
        }
        DirectBehavior::Keyboard | DirectBehavior::Plain => {}
    }
    if role == "file" || role == "samp" {
        node.attrs.set_str("role", role.to_owned());
    }
    let id = doc.add_node(node, Some(parent))?;
    let mut text_node = Node::text(text);
    text_node.span = Some(payload_span);
    doc.add_node(text_node, Some(id))?;
    Ok((vec![id], None))
}

#[allow(clippy::too_many_arguments)]
fn emit_external(
    role: &str,
    prefix: &str,
    title_prefix: &str,
    classes: Vec<String>,
    payload: &str,
    full_span: Span,
    payload_span: Span,
    line: u32,
    doc: &mut Document,
    parent: NodeId,
) -> MystResult<(Vec<NodeId>, Option<XrefMeta>)> {
    let explicit = split_explicit_title(payload);
    let target = explicit.as_ref().map_or(payload, |e| e.target.as_str());
    let number = target.trim_start_matches('#').split('#').next().unwrap_or(target);
    let uri = if role == "pep" {
        format!("{prefix}{:0>4}/", number.parse::<u32>().unwrap_or(0))
    } else {
        format!("{prefix}{number}")
    };
    let title = explicit.as_ref().map_or_else(
        || format!("{title_prefix}{}", whitespace_normalize_name(target)),
        |e| e.title.clone(),
    );

    let mut reference = Node::new(NodeKind::Reference);
    reference.span = Some(full_span);
    reference.source = Some(SourcePos::new(None, Some(line), None));
    reference.attrs.set_str("refuri", uri);
    reference.attrs.set_bool("internal", false);
    reference.attrs.set_list("classes", classes);
    let ref_id = doc.add_node(reference, Some(parent))?;
    let mut strong = Node::new(NodeKind::Strong);
    strong.span = Some(payload_span);
    let strong_id = doc.add_node(strong, Some(ref_id))?;
    let mut text = Node::text(title);
    text.span = Some(payload_span);
    doc.add_node(text, Some(strong_id))?;
    Ok((vec![ref_id], None))
}

#[allow(clippy::too_many_arguments)]
fn emit_xref(
    role: &str,
    domain: &str,
    reftype: &str,
    inner: scribe_rst::role_registry::InnerKind,
    warn: bool,
    payload: &str,
    full_span: Span,
    payload_span: Span,
    line: u32,
    source_name: Option<&str>,
    doc: &mut Document,
    parent: NodeId,
) -> MystResult<(Vec<NodeId>, Option<XrefMeta>)> {
    let explicit = split_explicit_title(payload);
    let mut title = explicit.as_ref().map_or_else(|| payload.to_owned(), |e| e.title.clone());
    let mut target = explicit.as_ref().map_or_else(|| payload.to_owned(), |e| e.target.clone());
    let mut specific = false;
    apply_modifiers(&mut title, &mut target, &mut specific);

    let (t0, t1) = locate_in_payload(payload, &title);
    let (g0, g1) = locate_in_payload(payload, &target);
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

    let mut node = Node::new(NodeKind::PendingXref);
    node.span = Some(full_span);
    node.source = Some(SourcePos::new(None, Some(line), None));
    node.attrs.set_str("refdomain", domain.to_owned());
    node.attrs.set_str("reftype", reftype.to_owned());
    node.attrs.set_str("reftarget", whitespace_normalize_name(&target));
    node.attrs.set_bool("refexplicit", explicit.is_some());
    node.attrs.set_bool("refspecific", specific);
    node.attrs.set_bool("refwarn", warn);
    if let Some(docname) = source_name {
        node.attrs.set_str("refdoc", docname.to_owned());
    }
    node.attrs.set_list("classes", classes);
    let xref_id = doc.add_node(node, Some(parent))?;

    let mut inner = Node::new(inner.node_kind());
    inner.span = Some(title_span);
    let inner_id = doc.add_node(inner, Some(xref_id))?;
    let mut text = Node::text(title.clone());
    text.span = Some(title_span);
    doc.add_node(text, Some(inner_id))?;

    let meta = XrefMeta {
        role: role.to_owned(),
        raw_payload: payload.to_owned(),
        target,
        target_span,
        title_span,
        explicit_title: explicit.is_some(),
        specific,
    };
    Ok((vec![xref_id], Some(meta)))
}

struct ExplicitTitle {
    title: String,
    target: String,
}

fn split_explicit_title(payload: &str) -> Option<ExplicitTitle> {
    let bytes = payload.as_bytes();
    if bytes.last() != Some(&b'>') {
        return None;
    }
    let mut lt = None;
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
    Some(ExplicitTitle {
        title: title_raw.trim_end().to_owned(),
        target: payload[lt + 1..payload.len() - 1].to_owned(),
    })
}

fn apply_modifiers(title: &mut String, target: &mut String, specific: &mut bool) {
    let t = title.trim_start_matches('.');
    *title = t.to_owned();
    *target = target.trim_start_matches('~').to_owned();
    if title.starts_with('~') {
        let rest = &title[1..];
        *title = rest.rsplit_once('.').map_or(rest, |(_, last)| last).to_owned();
    }
    if target.starts_with('.') {
        *target = target[1..].to_owned();
        *specific = true;
    }
}

fn locate_in_payload(payload: &str, needle: &str) -> (usize, usize) {
    if needle.is_empty() {
        return (0, 0);
    }
    match payload.rfind(needle) {
        Some(pos) => (pos, pos + needle.len()),
        None => (0, needle.len().min(payload.len())),
    }
}

fn span_from_local(base: Span, local_start: usize, local_len: usize) -> Span {
    let start = base.start.saturating_add(local_start.min(base.len() as usize) as u32);
    let end = start.saturating_add(local_len.min((base.end - start) as usize) as u32);
    Span::new(start.min(base.end), end.min(base.end))
}

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
