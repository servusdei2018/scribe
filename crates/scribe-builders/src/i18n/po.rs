//! PO/POT parsing and emission (Phase 26).
//!
//! Upstream reference: `sphinx/templates/gettext/message.pot.jinja` and
//! `sphinx/builders/gettext.py::should_write`. The parser is intentionally
//! defensive: malformed input yields whatever could be recovered and never
//! panics.

use std::collections::BTreeMap;

/// One PO entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    /// Message id (empty for the header entry).
    pub msgid: String,
    /// Message translation.
    pub msgstr: String,
    /// Plural message id, when the entry is pluralizable.
    pub msgid_plural: Option<String>,
    /// Plural translations indexed by `nplurals` slot.
    pub msgstr_plural: BTreeMap<u32, String>,
    /// `#: file:line` locations.
    pub locations: Vec<(String, i64)>,
    /// `#: uuid` locations emitted when `gettext_uuid` is set.
    pub uuids: Vec<String>,
    /// `#.` extracted comments.
    pub extracted_comments: Vec<String>,
    /// `# ` translator comments.
    pub translator_comments: Vec<String>,
    /// Flag names from `#,` (notably `fuzzy`).
    pub flags: Vec<String>,
}

impl Message {
    /// `true` when the entry carries the `fuzzy` flag.
    pub fn is_fuzzy(&self) -> bool {
        self.flags.iter().any(|flag| flag == "fuzzy")
    }

    /// `true` for the metadata entry (`msgid ""`).
    pub fn is_header(&self) -> bool {
        self.msgid.is_empty() && self.msgid_plural.is_none()
    }
}

/// A parsed PO/POT catalog. Entries are kept in first-seen order so emission
/// is deterministic and mirrors Python's insertion-ordered dict.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoCatalog {
    /// Header fields (e.g. `Project-Id-Version` → `Demo 1.0`).
    pub header: BTreeMap<String, String>,
    /// All entries, with the metadata entry first when present.
    pub messages: Vec<Message>,
}

impl PoCatalog {
    /// Parses PO text. Never panics, even on truncated or malformed input.
    pub fn parse(text: &str) -> Self {
        let mut catalog = Self::default();
        let mut current: Option<Message> = None;
        let mut active_field: Option<Field> = None;

        for raw_line in text.lines() {
            let line = raw_line.trim_end_matches('\r').trim();
            if line.is_empty() {
                if let Some(message) = current.take() {
                    catalog.push_message(message);
                }
                active_field = None;
                continue;
            }

            if let Some(rest) = line.strip_prefix("#.") {
                {
                let message = current.get_or_insert_with(Message::default);
                    message.extracted_comments.push(rest.trim().to_owned());
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("#:") {
                {
                let message = current.get_or_insert_with(Message::default);
                    for token in rest.split_whitespace() {
                        if let Some((path, line_no)) = split_location(token) {
                            message.locations.push((path, line_no));
                        } else {
                            message.uuids.push(token.to_owned());
                        }
                    }
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("#,") {
                {
                let message = current.get_or_insert_with(Message::default);
                    message.flags.extend(rest.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned));
                }
                continue;
            }
            if line.starts_with('#') {
                {
                let message = current.get_or_insert_with(Message::default);
                    message.translator_comments.push(line.trim_start_matches('#').trim().to_owned());
                }
                continue;
            }

            if line.starts_with('"') {
                if let (Some(message), Some(field)) = (current.as_mut(), active_field.clone()) {
                    if let Some(value) = unquote_po(line) {
                        append_field(message, &field, value);
                    }
                }
                continue;
            }

            for (prefix, field) in [
                ("msgid_plural", Field::IdPlural),
                ("msgid", Field::Id),
                ("msgstr", Field::Str),
            ] {
                if let Some(rest) = line.strip_prefix(prefix) {
                    if prefix == "msgstr" && rest.trim_start().starts_with('[') {
                        // `msgstr[0] "..."` — handled below before the generic
                        // `msgstr` branch via an explicit check.
                        continue;
                    }
                    if prefix == "msgstr" && !rest.trim_start().is_empty() && !rest.trim_start().starts_with('"') {
                        continue;
                    }
                    if matches!(field, Field::Id) && current.is_some() && current.as_ref().is_some_and(|m| !m.msgstr.is_empty() || !m.msgstr_plural.is_empty()) {
                        // A new entry starts here without a blank separator.
                        if let Some(message) = current.take() {
                            catalog.push_message(message);
                        }
                    }
                    let message = current.get_or_insert_with(Message::default);
                    let value = unquote_po(rest.trim()).unwrap_or_default();
                    match field {
                        Field::Id => message.msgid = value,
                        Field::IdPlural => message.msgid_plural = Some(value),
                        Field::Str => message.msgstr = value,
                        Field::StrPlural(_) => {}
                    }
                    active_field = Some(field);
                    break;
                }
            }
            if line.starts_with("msgstr[") {
                let index = line
                    .split_once(']')
                    .and_then(|(left, _)| left.trim_start_matches("msgstr[").parse::<u32>().ok());
                let message = current.get_or_insert_with(Message::default);
                if let Some(index) = index {
                    let value = line.split_once(']').and_then(|(_, rest)| unquote_po(rest.trim())).unwrap_or_default();
                    message.msgstr_plural.insert(index, value);
                    active_field = Some(Field::StrPlural(index));
                }
                continue;
            }
        }
        if let Some(message) = current.take() {
            catalog.push_message(message);
        }
        catalog.refresh_header();
        catalog
    }

    fn push_message(&mut self, message: Message) {
        if message.is_header() {
            self.header = parse_header(&message.msgstr);
        }
        self.messages.push(message);
    }

    /// Parses/overwrites the header map from the current header entry text.
    pub fn refresh_header(&mut self) {
        if let Some(header) = self.messages.iter().find(|m| m.is_header()) {
            self.header = parse_header(&header.msgstr);
        }
    }

    /// Value of one header field.
    pub fn header_value(&self, key: &str) -> Option<&str> {
        self.header.get(key).map(String::as_str)
    }

    /// Looks up a usable translation (`msgid` → `msgstr`), ignoring fuzzy and
    /// empty entries.
    pub fn get(&self, msgid: &str) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| !message.is_header() && message.msgid == msgid && !message.is_fuzzy() && !message.msgstr.is_empty())
            .map(|message| message.msgstr.as_str())
    }

    /// Finds an entry by `msgid`, including fuzzy/empty ones.
    pub fn message(&self, msgid: &str) -> Option<&Message> {
        self.messages.iter().find(|message| message.msgid == msgid)
    }

    /// Adds a message, merging locations when the `msgid` already exists.
    pub fn add(&mut self, message: Message) {
        if let Some(existing) = self.messages.iter_mut().find(|m| m.msgid == message.msgid && m.msgid_plural == message.msgid_plural) {
            for location in message.locations {
                if !existing.locations.contains(&location) {
                    existing.locations.push(location);
                }
            }
            for uuid in message.uuids {
                if !existing.uuids.contains(&uuid) {
                    existing.uuids.push(uuid);
                }
            }
            return;
        }
        self.messages.push(message);
    }

    /// Returns the entries used to build a binary MO catalog. The header is
    /// always first; fuzzy/empty translations are dropped unless `use_fuzzy`.
    pub fn mo_entries(&self, use_fuzzy: bool) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        if let Some(header) = self.messages.iter().find(|m| m.is_header()) {
            entries.push((String::new(), header.msgstr.clone()));
        } else {
            entries.push((String::new(), render_header_fields(&self.header)));
        }
        let mut messages: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| {
                !m.is_header()
                    && (use_fuzzy || !m.is_fuzzy())
                    && (!m.msgstr.is_empty() || !m.msgstr_plural.is_empty())
            })
            .collect();
        messages.sort_by(|a, b| a.msgid.cmp(&b.msgid));
        for message in messages {
            if let Some(plural) = &message.msgid_plural {
                let key = format!("{}\0{}", message.msgid, plural);
                let mut form = 0;
                let mut parts = Vec::new();
                while let Some(value) = message.msgstr_plural.get(&form) {
                    parts.push(value.clone());
                    form += 1;
                }
                if parts.is_empty() {
                    parts.push(message.msgstr.clone());
                }
                entries.push((key, parts.join("\0")));
            } else {
                entries.push((message.msgid.clone(), message.msgstr.clone()));
            }
        }
        entries
    }

    /// Emits a POT file using upstream's template and context.
    pub fn to_pot(&self, context: &PotContext) -> String {
        let mut out = String::new();
        out.push_str("# SOME DESCRIPTIVE TITLE.\n");
        out.push_str(&format!("# Copyright (C) {}\n", escape_inline(&context.copyright)));
        out.push_str(&format!("# This file is distributed under the same license as the {} package.\n", escape_inline(&context.project)));
        out.push_str("# FIRST AUTHOR <EMAIL@ADDRESS>, YEAR.\n#\n#, fuzzy\n");
        out.push_str("msgid \"\"\nmsgstr \"\"\n");
        for &key in POT_HEADER_ORDER {
            let rendered = match key {
                "Project-Id-Version" => format!("{} {}", context.project, context.version),
                "Report-Msgid-Bugs-To" => String::new(),
                "POT-Creation-Date" => context.ctime.clone(),
                "PO-Revision-Date" => "YEAR-MO-DA HO:MI+ZONE".to_owned(),
                "Last-Translator" => context.last_translator.clone(),
                "Language-Team" => context.language_team.clone(),
                "MIME-Version" => "1.0".to_owned(),
                "Content-Type" => "text/plain; charset=UTF-8".to_owned(),
                "Content-Transfer-Encoding" => "8bit".to_owned(),
                _ => String::new(),
            };
            out.push_str(&format!("\"{}: {}\\n\"\n", key, escape_inline(&rendered)));
        }

        for message in self.messages.iter().filter(|m| !m.is_header()) {
            out.push('\n');
            for comment in &message.extracted_comments {
                out.push_str(&format!("#. {}\n", comment));
            }
            for comment in &message.translator_comments {
                out.push_str(&format!("# {}\n", comment));
            }
            if context.display_location {
                for (source, line) in &message.locations {
                    out.push_str(&format!("#: {}:{}\n", source, line));
                }
            }
            if context.display_uuid {
                for uuid in &message.uuids {
                    out.push_str(&format!("#: {}\n", uuid));
                }
            }
            out.push_str(&format!("msgid \"{}\"\n", escape_message(&message.msgid)));
            if let Some(plural) = &message.msgid_plural {
                out.push_str(&format!("msgid_plural \"{}\"\n", escape_message(plural)));
                for index in 0..message.msgstr_plural.len() as u32 {
                    let value = message.msgstr_plural.get(&index).map(String::as_str).unwrap_or("");
                    out.push_str(&format!("msgstr[{}] \"{}\"\n", index, escape_message(value)));
                }
            } else {
                out.push_str(&format!("msgstr \"{}\"\n", escape_message(&message.msgstr)));
            }
        }
        out
    }

    /// Emits a `.po` file, preserving existing header fields and translations.
    pub fn to_po(&self) -> String {
        let mut out = String::new();
        for comment in self.messages.iter().find(|m| m.is_header()).map(|m| m.translator_comments.as_slice()).unwrap_or_default() {
            out.push_str(&format!("# {}\n", comment));
        }
        out.push_str("msgid \"\"\nmsgstr \"\"\n");
        let mut fields: Vec<(&String, &String)> = self.header.iter().collect();
        fields.sort_by(|a, b| a.0.cmp(b.0));
        for (key, value) in fields {
            out.push_str(&format!("\"{}: {}\\n\"\n", key, escape_inline(value)));
        }
        for message in self.messages.iter().filter(|m| !m.is_header()) {
            out.push('\n');
            for comment in &message.extracted_comments {
                out.push_str(&format!("#. {}\n", comment));
            }
            for comment in &message.translator_comments {
                out.push_str(&format!("# {}\n", comment));
            }
            if message.is_fuzzy() {
                out.push_str("#, fuzzy\n");
            }
            for (source, line) in &message.locations {
                out.push_str(&format!("#: {}:{}\n", source, line));
            }
            out.push_str(&format!("msgid \"{}\"\n", escape_message(&message.msgid)));
            if let Some(plural) = &message.msgid_plural {
                out.push_str(&format!("msgid_plural \"{}\"\n", escape_message(plural)));
                for index in 0..message.msgstr_plural.len() as u32 {
                    let value = message.msgstr_plural.get(&index).map(String::as_str).unwrap_or("");
                    out.push_str(&format!("msgstr[{}] \"{}\"\n", index, escape_message(value)));
                }
            } else {
                out.push_str(&format!("msgstr \"{}\"\n", escape_message(&message.msgstr)));
            }
        }
        out
    }

    /// Merges translations from `existing` into a freshly extracted catalog,
    /// keeping the new locations and POT-Creation-Date but preserving every
    /// existing `msgstr`, flag, translator comment and header field.
    pub fn merge_translations(&self, existing: &PoCatalog) -> Self {
        let mut merged = self.clone();
        for message in &mut merged.messages {
            if message.is_header() {
                continue;
            }
            if let Some(old) = existing.message(&message.msgid) {
                message.msgstr = old.msgstr.clone();
                message.msgstr_plural = old.msgstr_plural.clone();
                message.flags = old.flags.clone();
                message.translator_comments = old.translator_comments.clone();
                message.extracted_comments = old.extracted_comments.clone();
            }
        }
        let mut header = self.header.clone();
        for (key, value) in &existing.header {
            if key == "POT-Creation-Date" {
                continue;
            }
            header.insert(key.clone(), value.clone());
        }
        merged.header = header;
        if let Some(header_message) = merged.messages.iter_mut().find(|m| m.is_header()) {
            header_message.msgstr = render_header_fields(&merged.header);
        }
        merged
    }
}

#[derive(Clone, Debug)]
enum Field {
    Id,
    IdPlural,
    Str,
    StrPlural(u32),
}

fn append_field(message: &mut Message, field: &Field, value: String) {
    match field {
        Field::Id => message.msgid.push_str(&value),
        Field::IdPlural => message.msgid_plural.get_or_insert_with(String::new).push_str(&value),
        Field::Str => message.msgstr.push_str(&value),
        Field::StrPlural(index) => message.msgstr_plural.entry(*index).or_default().push_str(&value),
    }
}

/// Context for [`PoCatalog::to_pot`], transcribed from the upstream Jinja
/// template's variables.
#[derive(Clone, Debug)]
pub struct PotContext {
    pub project: String,
    pub version: String,
    pub copyright: String,
    pub ctime: String,
    pub last_translator: String,
    pub language_team: String,
    pub display_location: bool,
    pub display_uuid: bool,
}

impl Default for PotContext {
    fn default() -> Self {
        Self {
            project: "Project name not set".to_owned(),
            version: String::new(),
            copyright: String::new(),
            ctime: "YEAR-MO-DA HO:MI+ZONE".to_owned(),
            last_translator: "FULL NAME <EMAIL@ADDRESS>".to_owned(),
            language_team: "LANGUAGE <LL@li.org>".to_owned(),
            display_location: true,
            display_uuid: false,
        }
    }
}

const POT_HEADER_ORDER: &[&str] = &[
    "Project-Id-Version",
    "Report-Msgid-Bugs-To",
    "POT-Creation-Date",
    "PO-Revision-Date",
    "Last-Translator",
    "Language-Team",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
];

fn render_header_fields(fields: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    for key in keys {
        let value = fields.get(key).map(String::as_str).unwrap_or("");
        out.push_str(&format!("{}: {}\n", key, value));
    }
    out
}

fn parse_header(text: &str) -> BTreeMap<String, String> {
    let mut header = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            header.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    header
}

/// Escapes a single-line header value exactly like upstream's `e` filter
/// (without the newline continuation form).
pub fn escape_inline(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escapes a message with upstream's multi-line continuation convention:
/// `\n` becomes a literal `\n` escape followed by a new quoted line.
pub fn escape_message(value: &str) -> String {
    escape_inline(value).replace('\n', "\\n\"\n\"")
}

/// Decodes one quoted PO token (`"..."`), including continuation escapes.
pub fn unquote_po(token: &str) -> Option<String> {
    let token = token.trim();
    if token.len() < 2 || !token.starts_with('"') || !token.ends_with('"') {
        return None;
    }
    let body = &token[1..token.len() - 1];
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}

fn split_location(token: &str) -> Option<(String, i64)> {
    let (path, line) = token.rsplit_once(':')?;
    let line = line.parse::<i64>().ok()?;
    if path.is_empty() {
        return None;
    }
    Some((path.to_owned(), line))
}

/// Mirrors `sphinx.builders.gettext.should_write`: ignore the region between
/// the `POT-Creation-Date` header and the `PO-Revision-Date` header so an
/// unchanged body does not churn the creation timestamp.
pub fn should_write(old_content: &str, new_content: &str) -> bool {
    let Some(old_header) = old_content.find("\"POT-Creation-Date:") else { return true };
    let Some(new_header) = new_content.find("\"POT-Creation-Date:") else { return true };
    let Some(old_body) = old_content.find("\"PO-Revision-Date:") else { return true };
    let Some(new_body) = new_content.find("\"PO-Revision-Date:") else { return true };
    old_content[..old_header] != new_content[..new_header]
        || new_content[new_body..] != old_content[old_body..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_and_unquote_round_trip() {
        let original = "line \"one\" \\ path\nnext\tend";
        let escaped = escape_message(original);
        let quoted = format!("\"{escaped}\"");
        // A multi-line message is emitted as adjacent quoted tokens separated
        // by a newline; decode each token and concatenate them.
        let decoded: String = quoted
            .lines()
            .filter_map(unquote_po)
            .collect();
        assert_eq!(decoded, original);
    }

    #[test]
    fn parses_header_and_entries() {
        let text = "# translator\n#. extracted\n#: foo.rst:3\n#, fuzzy\nmsgid \"Hello\"\nmsgstr \"Hallo\"\n\nmsgid \"Bye\"\nmsgstr \"\"\n\"Tschuss\"\n";
        let catalog = PoCatalog::parse(text);
        assert_eq!(catalog.messages.len(), 2);
        let hello = catalog.message("Hello").unwrap();
        assert_eq!(hello.msgstr, "Hallo");
        assert!(hello.is_fuzzy());
        assert_eq!(hello.locations, vec![("foo.rst".to_owned(), 3)]);
        assert_eq!(hello.extracted_comments, vec!["extracted"]);
        assert_eq!(hello.translator_comments, vec!["translator"]);
        assert_eq!(catalog.get("Bye"), Some("Tschuss"));
        assert_eq!(catalog.get("Hello"), None); // fuzzy is ignored
    }

    #[test]
    fn parses_plural_entry() {
        let text = "msgid \"one item\"\nmsgid_plural \"many items\"\nmsgstr[0] \"ein\"\nmsgstr[1] \"viele\"\n";
        let catalog = PoCatalog::parse(text);
        let message = catalog.message("one item").unwrap();
        assert_eq!(message.msgid_plural.as_deref(), Some("many items"));
        assert_eq!(message.msgstr_plural.get(&0).map(String::as_str), Some("ein"));
        assert_eq!(message.msgstr_plural.get(&1).map(String::as_str), Some("viele"));
        let mo = catalog.mo_entries(false);
        assert_eq!(mo[1], ("one item\0many items".to_owned(), "ein\0viele".to_owned()));
    }

    #[test]
    fn should_write_ignores_creation_date() {
        let old = "#, fuzzy\nmsgid \"\"\nmsgstr \"\"\n\"POT-Creation-Date: 2000\\n\"\n\"PO-Revision-Date: X\\n\"\n\nmsgid \"a\"\nmsgstr \"\"\n";
        let new = old.replace("2000", "2026");
        assert!(!should_write(old, &new));
        assert!(should_write(old, &new.replace("msgid \"a\"", "msgid \"b\"")));
    }

    #[test]
    fn merge_preserves_translations_and_header() {
        let extracted = PoCatalog::parse("msgid \"Hi\"\nmsgstr \"\"\n");
        let existing = PoCatalog::parse("msgid \"\"\nmsgstr \"\"\n\"Language: xx\\n\"\n\"POT-Creation-Date: OLD\\n\"\n\nmsgid \"Hi\"\nmsgstr \"Hallo\"\n");
        let merged = extracted.merge_translations(&existing);
        assert_eq!(merged.get("Hi"), Some("Hallo"));
        assert_eq!(merged.header_value("Language"), Some("xx"));
    }
}
