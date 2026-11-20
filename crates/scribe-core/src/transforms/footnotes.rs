//! Docutils `Footnotes` transform (priority 620), ported for the frozen AST.
//!
//! Transcribed from `docutils/transforms/references.py::Footnotes`: numbering
//! of `[#]`/`[#name]` footnotes, symbol footnotes (`[*]`), manual footnotes and
//! citations, plus `refid`/`backrefs` links. The frozen AST has no `document`
//! registry side tables, so name/id lookup maps are rebuilt per invocation
//! and generated ids follow the same `idN` scheme as the rest of scribe.

use std::collections::BTreeMap;

use scribe_ast::{Document, NodeId, NodeKind};

use super::{
    collect_used_ids, fresh_id, int_attr, list_attr, set_list_attr, str_attr,
    Transform, TransformContext, TransformResult,
};

const SYMBOLS: &[&str] = &[
    "*", "\u{2020}", "\u{2021}", "\u{a7}", "\u{b6}", "#", "\u{2660}", "\u{2665}", "\u{2666}",
    "\u{2663}",
];

#[derive(Clone, Debug)]
struct Note {
    id: NodeId,
    kind: NodeKind,
    names: Vec<String>,
    ids: Vec<String>,
    auto: Option<String>,
    number: Option<i64>,
    backrefs: Vec<String>,
}

#[derive(Clone, Debug)]
struct Ref {
    id: NodeId,
    kind: NodeKind,
    refname: Option<String>,
    auto: Option<String>,
    ids: Vec<String>,
}

fn ctx_docname(document: &Document) -> String {
    document.metadata.docname.clone().unwrap_or_default()
}

/// Docutils `Footnotes` — number footnotes/citations and link their refs.
#[derive(Debug)]
pub struct FootnotesTransform;

impl Transform for FootnotesTransform {
    fn name(&self) -> &'static str {
        "Footnotes"
    }

    fn default_priority(&self) -> u32 {
        super::priority::FOOTNOTES
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let order = document.preorder_ids();
        let mut notes: Vec<Note> = Vec::new();
        let mut refs: Vec<Ref> = Vec::new();
        for id in &order {
            let Ok(node) = document.get(*id) else {
                continue;
            };
            match node.kind {
                NodeKind::Footnote | NodeKind::Citation => {
                    let mut names = list_attr(node, "names");
                    // The Phase 06 parser stores `[#]` as auto="0" and `[*]`
                    // as auto="1"; translate to docutils' canonical values so
                    // the rest of this transform (and pseudo-XML) matches
                    // upstream: auto="1" (auto-numbered) and auto="*" (symbol).
                    let mut auto = match str_attr(node, "auto") {
                        Some("0") => Some("1".to_owned()),
                        Some("1") | Some("*") => Some("*".to_owned()),
                        other => other.map(str::to_owned),
                    };
                    let number = int_attr(node, "number");
                    // `[#name]` is stored by the parser as a name beginning
                    // with `#`; treat it as an auto-numbered footnote with the
                    // leading marker stripped, matching docutils.
                    if let Some(name) = names.iter_mut().find(|name| name.starts_with('#')) {
                        *name = name.trim_start_matches('#').to_owned();
                        auto = Some("1".to_owned());
                    }
                    // `[1]` is stored as a `number` with no name; refs use the
                    // number string as their refname.
                    if let Some(number) = number {
                        let label = number.to_string();
                        if !names.contains(&label) {
                            names.push(label);
                        }
                    }
                    notes.push(Note {
                        id: *id,
                        kind: node.kind,
                        names,
                        ids: list_attr(node, "ids"),
                        auto,
                        number,
                        backrefs: list_attr(node, "backrefs"),
                    })
                }
                NodeKind::FootnoteReference | NodeKind::CitationReference => refs.push(Ref {
                    id: *id,
                    kind: node.kind,
                    refname: str_attr(node, "refname").map(str::to_owned),
                    auto: str_attr(node, "auto").map(str::to_owned),
                    ids: list_attr(node, "ids"),
                }),
                _ => {}
            }
        }
        if notes.is_empty() && refs.is_empty() {
            return TransformResult::unchanged();
        }

        // Write normalized names back (`[#name]` → `name`, `[1]` → `1`).
        for note in &notes {
            if let Ok(node) = document.get_mut(note.id) {
                if list_attr(node, "names") != note.names {
                    node.attrs.set_list("names", note.names.clone());
                }
                // Canonical `auto` value; parser-only attributes are removed.
                match note.auto.as_deref() {
                    Some(kind @ ("1" | "*")) => node.attrs.set_str("auto", kind),
                    _ => {
                        node.attrs.remove("auto");
                    }
                }
                node.attrs.remove("number");
                let classes: Vec<String> = list_attr(node, "classes")
                    .into_iter()
                    .filter(|class| class != "footnote")
                    .collect();
                set_list_attr(node, "classes", classes);
            }
        }

        let mut used_ids = collect_used_ids(document);

        // Every note and reference needs an id so backlinks have a target.
        for note in &mut notes {
            if note.ids.is_empty() {
                let generated = fresh_id(document, &mut used_ids);
                if let Ok(node) = document.get_mut(note.id) {
                    node.attrs.set_list("ids", vec![generated.clone()]);
                }
                note.ids.push(generated);
            }
        }
        for reference in &mut refs {
            if reference.ids.is_empty() {
                let generated = fresh_id(document, &mut used_ids);
                if let Ok(node) = document.get_mut(reference.id) {
                    node.attrs.set_list("ids", vec![generated.clone()]);
                }
                reference.ids.push(generated);
            }
        }
        let mut name_to_note: BTreeMap<String, NodeId> = BTreeMap::new();
        for note in &notes {
            for name in &note.names {
                name_to_note.entry(name.clone()).or_insert(note.id);
            }
        }

        // -- auto-numbered footnotes ([1], [#], [#name]) -------------------
        let mut start = 1i64;
        let mut autolabels: Vec<String> = Vec::new();
        for note in &notes {
            if note.kind != NodeKind::Footnote
                || note.auto.as_deref() != Some("0")
                    && note.auto.as_deref() != Some("1")
            {
                continue;
            }
            let label = loop {
                let candidate = start.to_string();
                start += 1;
                if !name_to_note.contains_key(&candidate) {
                    break candidate;
                }
            };
            prepend_label(document, note.id, &label);
            for name in &note.names {
                for reference in refs.iter().filter(|reference| {
                    reference.kind == NodeKind::FootnoteReference && reference.refname.as_deref() == Some(name.as_str())
                }) {
                    set_ref_text(document, reference.id, &label);
                    link(document, note, reference);
                }
            }
            if note.names.is_empty() {
                autolabels.push(label.clone());
                if let Ok(node) = document.get_mut(note.id) {
                    node.attrs.set_list("names", vec![label.clone()]);
                }
                name_to_note.insert(label, note.id);
            }
        }
        let auto_refs: Vec<Ref> = refs
            .iter()
            .filter(|reference| {
                reference.kind == NodeKind::FootnoteReference
                    && reference.auto.as_deref() == Some("1")
                    && !ref_has_refid(document, reference.id)
            })
            .cloned()
            .collect();
        for reference in auto_refs {
            let Some(label) = autolabels.first() else {
                continue;
            };
            let label = label.clone();
            if let Some(&note_id) = name_to_note.get(&label) {
                if let Some(note) = notes.iter_mut().find(|note| note.id == note_id) {
                    set_ref_text(document, reference.id, &label);
                    link(document, note, &reference);
                }
            }
            autolabels.remove(0);
        }

        // -- symbol footnotes ([*]) ----------------------------------------
        let symbol_notes: Vec<&Note> = notes
            .iter()
            .filter(|note| note.kind == NodeKind::Footnote && note.auto.as_deref() == Some("*"))
            .collect();
        let symbols: Vec<String> = symbol_notes
            .iter()
            .enumerate()
            .map(|(index, note)| {
                let (reps, position) = (index / SYMBOLS.len(), index % SYMBOLS.len());
                let label = SYMBOLS[position].repeat(reps + 1);
                prepend_label(document, note.id, &label);
                label
            })
            .collect();
        let mut symbol_index = 0usize;
        let symbol_refs: Vec<Ref> = refs
            .iter()
            .filter(|reference| {
                reference.kind == NodeKind::FootnoteReference && reference.auto.as_deref() == Some("*")
            })
            .cloned()
            .collect();
        for reference in symbol_refs {
            if ref_has_refid(document, reference.id) || symbol_index >= symbols.len() {
                symbol_index += 1;
                continue;
            }
            let label = symbols[symbol_index].clone();
            if let Some(note) = symbol_notes.get(symbol_index) {
                set_ref_text(document, reference.id, &label);
                link(document, note, &reference);
            }
            symbol_index += 1;
        }

        // -- explicitly numbered footnotes ([1] with an explicit number) ---
        for note in &notes {
            let Some(number) = note.number else {
                continue;
            };
            let label = number.to_string();
            prepend_label(document, note.id, &label);
            for name in &note.names {
                for reference in refs.iter().filter(|reference| {
                    reference.kind == NodeKind::FootnoteReference && reference.refname.as_deref() == Some(name.as_str())
                }) {
                    set_ref_text(document, reference.id, &label);
                    link(document, note, reference);
                }
            }
        }

        // -- manual footnotes and citations -------------------------------
        for note in &notes {
            if note.auto.is_some() || note.number.is_some() {
                continue;
            }
            if note.kind == NodeKind::Citation {
                if let Some(name) = note.names.first() {
                    prepend_label(document, note.id, name);
                    // Upstream marks citation labels as non-smartquotable.
                    let label_id = document.children(note.id).ok().and_then(|children| children.first().copied());
                    if let Some(label_id) = label_id {
                        if let Ok(label) = document.get_mut(label_id) {
                            label.attrs.set_bool("support_smartquotes", false);
                        }
                    }
                }
            }
            for name in &note.names {
                for reference in refs.iter().filter(|reference| {
                    reference.refname.as_deref() == Some(name.as_str())
                        && reference.kind == matching_ref_kind(note.kind)
                }) {
                    link(document, note, reference);
                }
            }
        }

        for reference in &refs {
            if reference.refname.is_some() && !ref_has_refid(document, reference.id) {
                let name = reference.refname.as_deref().unwrap_or("");
                // Cross-document footnote/citation references: look for the
                // definition in every other recorded doctree and turn the
                // local reference into a normal link when found.
                let mut external: Option<(String, String, String)> = None;
                if let Some(env) = ctx.env {
                    for (other_doc, snapshot) in &env.documents {
                        if other_doc.as_str() == ctx_docname(document) { continue; }
                        let Some(other) = snapshot.doctree.as_ref() else { continue };
                        for id in other.preorder_ids() {
                            let Ok(note) = other.get(id) else { continue };
                            if note.kind != NodeKind::Footnote && note.kind != NodeKind::Citation { continue; }
                            let names = list_attr(note, "names");
                            if names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                                let anchor = note.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_else(|| name.to_owned());
                                external = Some((other_doc.clone(), anchor, name.to_owned()));
                                break;
                            }
                        }
                        if external.is_some() { break; }
                    }
                }
                if let Some((other_doc, anchor, label)) = external {
                    if let Ok(node) = document.get_mut(reference.id) {
                        node.kind = NodeKind::Reference;
                        node.attrs.set_str("refuri", format!("{}.html#{}", other_doc, anchor));
                        node.attrs.set_list("classes", vec!["reference".to_owned(), "internal".to_owned()]);
                        let text_id = document.new_text(format!("[{}]", label), Some(reference.id)).unwrap_or(reference.id);
                        if text_id != reference.id {
                            let _ = document.replace_children(reference.id, vec![text_id]);
                        }
                    }
                    continue;
                }
                ctx.warn(
                    "Footnotes",
                    format!("reference target not found: {}", name),
                    document.get(reference.id).ok().and_then(|node| node.span),
                );
            }
        }

        let mut changed = false;
        for note in &notes {
            if let Ok(node) = document.get_mut(note.id) {
                let backrefs = list_attr(node, "backrefs");
                if backrefs != note.backrefs {
                    changed = true;
                }
                set_list_attr(node, "backrefs", backrefs);
            }
        }
        // Labels/generated ids always count as a change when notes or refs exist.
        if !notes.is_empty() || !refs.is_empty() {
            changed = true;
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

fn ref_has_refid(document: &Document, id: NodeId) -> bool {
    document.get(id).ok().and_then(|node| str_attr(node, "refid")).is_some()
}

fn matching_ref_kind(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::Citation => NodeKind::CitationReference,
        _ => NodeKind::FootnoteReference,
    }
}

/// Links `reference` to `note` via `refid`/`backrefs` and drops `refname`.
fn link(document: &mut Document, note: &Note, reference: &Ref) {
    let Some(note_id) = note.ids.first() else {
        return;
    };
    let Some(ref_id) = reference.ids.first() else {
        return;
    };
    if let Ok(node) = document.get_mut(reference.id) {
        node.attrs.remove("refname");
        node.attrs.set_str("refid", note_id.clone());
    }
    if let Ok(node) = document.get_mut(note.id) {
        let mut backrefs = list_attr(node, "backrefs");
        if !backrefs.contains(ref_id) {
            backrefs.push(ref_id.clone());
            set_list_attr(node, "backrefs", backrefs);
        }
    }
}

/// Prepends a `<label>` containing `text` to the given note.
fn prepend_label(document: &mut Document, note: NodeId, text: &str) {
    // Avoid duplicating a label when the note already begins with one.
    if document.children(note).ok().and_then(|children| children.first().copied())
        .is_some_and(|first| document.get(first).is_ok_and(|node| node.kind == NodeKind::Label))
    {
        return;
    }
    let text_id = match document.new_text(text, None) {
        Ok(id) => id,
        Err(_) => return,
    };
    let label_id = match document.add(NodeKind::Label, None) {
        Ok(id) => id,
        Err(_) => return,
    };
    if document.append_child(label_id, text_id).is_err() {
        return;
    }
    let _ = document.insert_child(note, 0, label_id);
}

/// Replaces the display text of a footnote/citation reference.
fn set_ref_text(document: &mut Document, reference: NodeId, text: &str) {
    let existing = document
        .children(reference)
        .ok()
        .and_then(|children| children.iter().copied().find(|&child| {
            document.get(child).is_ok_and(|node| node.kind == NodeKind::Text)
        }));
    if let Some(child) = existing {
        if let Ok(node) = document.get_mut(child) {
            node.set_text(text);
        }
        return;
    }
    if let Ok(child) = document.new_text(text, None) {
        let _ = document.append_child(reference, child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_index_math_matches_docutils() {
        let (reps, index) = (4usize / SYMBOLS.len(), 4usize % SYMBOLS.len());
        assert_eq!(reps, 0);
        assert_eq!(index, 4);
        let (reps, index) = (10usize / SYMBOLS.len(), 10usize % SYMBOLS.len());
        assert_eq!(reps, 1);
        assert_eq!(index, 0);
    }
}
