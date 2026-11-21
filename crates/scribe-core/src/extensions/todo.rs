//! `sphinx.ext.todo`: `.. todo::` admonitions and the `.. todolist::` collector.

use std::collections::BTreeMap;

use scribe_ast::{Document, Node, NodeId, NodeKind, SourcePos};
use scribe_config::Config;
use scribe_rst::directive_registry::clone_subtree;
use scribe_rst::{Directive, DirectiveContext, DirectiveError, OptionSpec};

use super::{relative_doc_uri, EXT_MARKER, EXT_TODO, EXT_TODOLIST};

/// The CSS class upstream puts on a todo admonition when no explicit class is given.
pub const TODO_CLASS: &str = "admonition-todo";

/// `.. todo::` — lowers to an admonition with a `Todo` title.
#[derive(Clone, Copy, Debug)]
pub struct TodoDirective;

impl Directive for TodoDirective {
    fn name(&self) -> &'static str {
        "todo"
    }

    fn optional_arguments(&self) -> usize {
        1
    }

    fn final_argument_whitespace(&self) -> bool {
        true
    }

    fn option_spec(&self) -> OptionSpec {
        OptionSpec::common().with("collapsible", scribe_rst::OptionKind::Choice(vec!["open".to_owned(), "closed".to_owned()]))
    }

    fn has_content(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let parent = ctx.parent;
        let id = ctx.add(NodeKind::Admonition, parent)?;
        let classes = if ctx.option_list("class").is_none() {
            vec![TODO_CLASS.to_owned()]
        } else {
            ctx.classes()
        };
        let docname = ctx
            .global
            .source_name
            .clone()
            .unwrap_or_else(|| "index".to_owned());
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_list("classes", classes);
            node.attrs.set_str(EXT_MARKER, EXT_TODO);
            node.attrs.set_str("docname", docname);
        }
        ctx.set_common(id)?;
        // Upstream inserts a `nodes.title(text=_('Todo'))` as the first child.
        let title = ctx.add(NodeKind::Title, id)?;
        ctx.add_text("Todo", title, ctx.line)?;
        // `BaseAdmonition.run` turns the optional argument into a paragraph
        // before nested-parsing the directive content.
        if let Some(argument) = ctx.argument(0).map(str::to_owned) {
            let paragraph = ctx.add(NodeKind::Paragraph, id)?;
            ctx.parse_inline_into(&argument, paragraph, ctx.line)?;
        }
        ctx.parse_nested_into(id)?;
        // `note_explicit_target` assigns `id1`, `id2`, ... when no name was given.
        let needs_id = ctx.doc.get(id)?.ids().unwrap_or_default().is_empty();
        if needs_id {
            let new_id = next_auto_id(ctx.doc, "id");
            ctx.doc.get_mut(id)?.attrs.set_list("ids", vec![new_id]);
        }
        Ok(vec![id])
    }
}

/// `.. todolist::` — placeholder replaced once every document has been read.
#[derive(Clone, Copy, Debug)]
pub struct TodoListDirective;

impl Directive for TodoListDirective {
    fn name(&self) -> &'static str {
        "todolist"
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let id = ctx.add(NodeKind::Generated, ctx.parent)?;
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_str(EXT_MARKER, EXT_TODOLIST);
        }
        ctx.set_common(id)?;
        Ok(vec![id])
    }
}

/// One collected todo entry and its backlink information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoEntry {
    /// Document the todo was declared in.
    pub docname: String,
    /// First explicit or auto-generated id of the todo node.
    pub id: String,
    /// Absolute source path (upstream's `todo.source`).
    pub source: String,
    /// One-based source line.
    pub line: u32,
    /// Node id inside `docname`'s doctree.
    pub node_id: NodeId,
    /// Short description (first content paragraph) used in warnings.
    pub description: String,
}

/// Accumulates todos across documents, then expands every `todolist`.
#[derive(Clone, Debug, Default)]
pub struct TodoCollector {
    todos: Vec<TodoEntry>,
}

impl TodoCollector {
    /// Scan a freshly parsed document and record its todos.
    ///
    /// Returns warnings when `todo_emit_warnings` is enabled.
    #[must_use]
    pub fn collect(&mut self, docname: &str, doc: &Document, config: &Config) -> Vec<String> {
        let emit = config.get_bool("todo_emit_warnings").unwrap_or(false);
        let mut warnings = Vec::new();
        for id in doc.preorder_ids() {
            let Ok(node) = doc.get(id) else { continue };
            let is_todo = node.attrs.get_str(EXT_MARKER).ok().flatten() == Some(EXT_TODO)
                || node.classes().unwrap_or_default().iter().any(|class| class == TODO_CLASS);
            if !is_todo {
                continue;
            }
            let entry_id = node
                .ids()
                .ok()
                .and_then(|ids| ids.first().cloned())
                .unwrap_or_else(|| next_auto_id(doc, "id"));
            let source = node
                .source
                .as_ref()
                .and_then(|pos| pos.source.clone())
                .unwrap_or_else(|| docname.to_owned());
            let line = node.source.as_ref().and_then(|pos| pos.line).unwrap_or(0);
            let description = first_content_text(doc, id);
            if emit {
                warnings.push(format!("{docname}: TODO entry found: {description}"));
            }
            self.todos.push(TodoEntry {
                docname: docname.to_owned(),
                id: entry_id,
                source,
                line,
                node_id: id,
                description,
            });
        }
        warnings
    }

    /// All collected todos, in document/read order.
    #[must_use]
    pub fn todos(&self) -> &[TodoEntry] {
        &self.todos
    }

    /// Replace every `todolist` node with copies of all todos plus backlinks.
    ///
    /// `target_uris` maps docnames to their output page URI. When
    /// `todo_include_todos` is false every `todolist` is simply removed,
    /// matching upstream's `TodoListProcessor`.
    pub fn expand(
        &self,
        docs: &mut BTreeMap<String, Document>,
        config: &Config,
        target_uris: &BTreeMap<String, String>,
    ) {
        let include = config.get_bool("todo_include_todos").unwrap_or(false);
        let link_only = config.get_bool("todo_link_only").unwrap_or(false);

        // Upstream's `todo` directives contribute nothing to the output unless
        // `todo_include_todos` is enabled: drop the admonitions themselves.
        if !include {
            for doc in docs.values_mut() {
                let todos: Vec<NodeId> = doc.preorder_ids().into_iter().filter(|&id| {
                    doc.get(id).is_ok_and(|node| {
                        node.attrs.get_str(EXT_MARKER).ok().flatten() == Some(EXT_TODO)
                    })
                }).collect();
                for id in todos {
                    let _ = doc.remove_node(id);
                }
            }
        }

        // Snapshot each todo into a detached document so we can clone it into
        // any target document without aliasing arena ids or borrow conflicts.
        let mut snapshots: Vec<(TodoEntry, Document, NodeId)> = Vec::new();
        for entry in &self.todos {
            let Some(source_doc) = docs.get(&entry.docname) else { continue };
            let mut detached = Document::new(None);
            let detached_root = detached.root;
            if let Ok(root_child) = clone_subtree(source_doc, &mut detached, entry.node_id, detached_root, 0) {
                if let Ok(node) = detached.get_mut(root_child) {
                    node.attrs.remove("ids");
                    node.attrs.remove("names");
                    node.attrs.remove(EXT_MARKER);
                }
                snapshots.push((entry.clone(), detached, root_child));
            }
        }

        let todolist_docs: Vec<String> = docs
            .iter()
            .filter(|(_, doc)| find_todolists(doc).next().is_some())
            .map(|(name, _)| name.clone())
            .collect();

        for docname in todolist_docs {
            let Some(mut doc) = docs.remove(&docname) else { continue };
            let placeholders: Vec<NodeId> = find_todolists(&doc).collect();
            for placeholder in placeholders {
                let parent = doc.parent(placeholder).ok().flatten().unwrap_or(doc.root);
                if !include {
                    let _ = doc.remove_node(placeholder);
                    continue;
                }
                let original_children: Vec<NodeId> = doc.children(parent).unwrap_or_default().to_vec();
                let mut content: Vec<NodeId> = Vec::new();
                for (entry, snapshot, root_child) in &snapshots {
                    if let Ok(clone_id) = clone_subtree(snapshot, &mut doc, *root_child, parent, 0) {
                        content.push(clone_id);
                    }
                    content.push(make_source_paragraph(
                        &mut doc,
                        parent,
                        entry,
                        &docname,
                        target_uris,
                        link_only,
                    ));
                }
                let content_set: std::collections::BTreeSet<NodeId> = content.iter().copied().collect();
                let mut order = Vec::with_capacity(original_children.len() + content.len());
                for child in original_children {
                    if child == placeholder {
                        order.extend(content.iter().copied());
                    } else if !content_set.contains(&child) {
                        order.push(child);
                    }
                }
                let _ = doc.remove_node(placeholder);
                let _ = doc.replace_children(parent, order);
            }
            docs.insert(docname, doc);
        }
    }
}

fn find_todolists(doc: &Document) -> impl Iterator<Item = NodeId> + '_ {
    doc.preorder_ids().into_iter().filter(move |&id| {
        doc.get(id).is_ok_and(|node| {
            node.attrs.get_str(EXT_MARKER).ok().flatten() == Some(EXT_TODOLIST)
        })
    })
}

fn next_auto_id(doc: &Document, prefix: &str) -> String {
    let mut used = std::collections::BTreeSet::new();
    for id in doc.preorder_ids() {
        if let Ok(node) = doc.get(id) {
            for candidate in node.ids().unwrap_or_default() {
                if let Some(number) = candidate.strip_prefix(prefix).and_then(|rest| rest.parse::<u32>().ok()) {
                    used.insert(number);
                }
            }
        }
    }
    let mut number = 1;
    while used.contains(&number) {
        number += 1;
    }
    format!("{prefix}{number}")
}

fn first_content_text(doc: &Document, id: NodeId) -> String {
    let Ok(node) = doc.get(id) else { return String::new() };
    for &child in &node.children {
        if let Ok(child_node) = doc.get(child) {
            if child_node.kind != NodeKind::Title {
                return child_node.astext(doc);
            }
        }
    }
    String::new()
}

fn make_source_paragraph(
    doc: &mut Document,
    parent: NodeId,
    entry: &TodoEntry,
    current_doc: &str,
    target_uris: &BTreeMap<String, String>,
    link_only: bool,
) -> NodeId {
    let para = doc
        .add(NodeKind::Paragraph, Some(parent))
        .unwrap_or(doc.root);
    if let Ok(node) = doc.get_mut(para) {
        node.attrs.set_list("classes", vec!["todo-source".to_owned()]);
    }
    let (prefix, suffix) = if link_only {
        (String::new(), String::new())
    } else {
        (
            "(The ".to_owned(),
            format!(" is located in {}, line {}.)", entry.source, entry.line),
        )
    };
    if !prefix.is_empty() {
        let _ = add_text(doc, para, &prefix);
    }
    let reference = doc
        .add(NodeKind::Reference, Some(para))
        .unwrap_or(doc.root);
    if let Ok(node) = doc.get_mut(reference) {
        node.attrs.set_bool("internal", true);
        let uri = relative_doc_uri(target_uris, current_doc, &entry.docname);
        let resolved = if uri.starts_with('#') {
            format!("#{}", entry.id)
        } else {
            format!("{uri}#{}", entry.id)
        };
        node.attrs.set_str("refuri", resolved);
    }
    let emphasis = doc
        .add(NodeKind::Emphasis, Some(reference))
        .unwrap_or(doc.root);
    let _ = add_text(doc, emphasis, "original entry");
    if !suffix.is_empty() {
        let _ = add_text(doc, para, &suffix);
    }
    para
}

fn add_text(doc: &mut Document, parent: NodeId, text: &str) -> Result<NodeId, scribe_ast::AstError> {
    let mut node = Node::text(text);
    node.source = Some(SourcePos::new(None, Some(0), None));
    doc.add_node(node, Some(parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_have_expected_names() {
        assert_eq!(TodoDirective.name(), "todo");
        assert_eq!(TodoListDirective.name(), "todolist");
        let registry = scribe_rst::DirectiveRegistry::new();
        registry.register(TodoDirective);
        registry.register(TodoListDirective);
        assert!(registry.contains("todo"));
        assert!(registry.contains("todolist"));
    }

    #[test]
    fn auto_ids_are_unique_in_a_document() {
        let mut doc = Document::new(None);
        let node = doc.add(NodeKind::Admonition, Some(doc.root)).unwrap();
        doc.get_mut(node).unwrap().attrs.set_list("ids", vec!["id1".to_owned()]);
        assert_eq!(next_auto_id(&doc, "id"), "id2");
    }

    #[test]
    fn description_excludes_the_title() {
        let mut doc = Document::new(None);
        let admonition = doc.add(NodeKind::Admonition, Some(doc.root)).unwrap();
        let title = doc.add(NodeKind::Title, Some(admonition)).unwrap();
        let _ = doc.new_text("Todo", Some(title));
        let paragraph = doc.add(NodeKind::Paragraph, Some(admonition)).unwrap();
        let _ = doc.new_text("todo in foo", Some(paragraph));
        assert_eq!(first_content_text(&doc, admonition), "todo in foo");
    }
}
