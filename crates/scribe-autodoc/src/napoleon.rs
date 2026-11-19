//! Napoleon docstring converter (Phase 21).
//!
//! Converts Google-style and NumPy-style docstrings into reStructuredText that
//! is fed through `scribe_rst::parse` and the Python-domain field rewriting
//! pass. The emitted RST mirrors `sphinx.ext.napoleon.docstring`:
//!
//! * `Args:` / `Parameters:` → `:param name:` + `:type name:` fields,
//!   grouped into a *Parameters* field list by the Python domain;
//! * `Returns:` → `:returns:` + `:rtype:`;
//! * `Yields:` → a `:Yields:` field; `Raises:` → `:raises Type:`;
//! * `Attributes:` → `.. attribute::` blocks (or `:ivar:` when configured);
//! * `Examples:` / `Notes:` / `References:` → `.. rubric::`;
//! * `See Also:` → `.. seealso::`; `Warnings:` / `Warning:` → `.. warning::`.

use crate::parser::dedent_docstring;

/// Toggles and rendering choices for the napoleon converter, mirroring the
/// Sphinx `napoleon_*` configuration values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NapoleonOptions {
    /// Parse Google-style (`Args:`) sections.
    pub google: bool,
    /// Parse NumPy-style (underlined) sections.
    pub numpy: bool,
    /// Emit `:param:` fields for parameters (otherwise use `:ivar:`-style
    /// plain text).
    pub use_param: bool,
    /// Emit separate `:type:` fields.
    pub use_rtype: bool,
    /// Emit `:keyword:` fields for keyword-argument sections.
    pub use_keyword: bool,
    /// Render `Attributes:` as `:ivar:` fields instead of `.. attribute::`.
    pub use_ivar: bool,
    /// Render `Examples:` as `.. admonition::` instead of `.. rubric::`.
    pub use_admonition_for_examples: bool,
    /// Render `Notes:` as `.. note::` instead of `.. rubric::`.
    pub use_admonition_for_notes: bool,
}

impl Default for NapoleonOptions {
    fn default() -> Self {
        Self {
            google: true,
            numpy: true,
            use_param: true,
            use_rtype: true,
            use_keyword: true,
            use_ivar: false,
            use_admonition_for_examples: false,
            use_admonition_for_notes: false,
        }
    }
}

/// Canonical section name used internally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Parameters,
    KeywordArgs,
    OtherParams,
    Returns,
    Yields,
    Receives,
    Raises,
    Warns,
    Attributes,
    Examples,
    Notes,
    SeeAlso,
    Warnings,
    References,
    Methods,
    /// A section not recognized by napoleon (rendered as a rubric).
    Generic,
}

impl Section {
    /// Maps a Google section heading to its canonical kind.
    fn from_google(heading: &str) -> Option<Self> {
        let key = heading.trim().trim_end_matches(':').trim().to_lowercase();
        Some(match key.as_str() {
            "args" | "arguments" | "parameters" | "params" => Self::Parameters,
            "keyword args" | "keyword arguments" | "kwargs" | "keywords" => Self::KeywordArgs,
            "other parameters" | "other params" => Self::OtherParams,
            "returns" | "return" => Self::Returns,
            "yields" | "yield" => Self::Yields,
            "receives" => Self::Receives,
            "raises" | "raise" | "exceptions" => Self::Raises,
            "warns" => Self::Warns,
            "attributes" => Self::Attributes,
            "examples" | "example" => Self::Examples,
            "note" | "notes" => Self::Notes,
            "see also" => Self::SeeAlso,
            "warnings" | "warning" => Self::Warnings,
            "references" => Self::References,
            "methods" => Self::Methods,
            _ => return None,
        })
    }

    /// Maps a NumPy section heading to its canonical kind.
    fn from_numpy(heading: &str) -> Option<Self> {
        let key = heading.trim().to_lowercase();
        Some(match key.as_str() {
            "parameters" | "params" => Self::Parameters,
            "other parameters" | "other params" => Self::OtherParams,
            "keyword arguments" | "keywords" => Self::KeywordArgs,
            "returns" => Self::Returns,
            "yields" => Self::Yields,
            "receives" => Self::Receives,
            "raises" => Self::Raises,
            "warns" => Self::Warns,
            "attributes" => Self::Attributes,
            "examples" => Self::Examples,
            "notes" => Self::Notes,
            "see also" => Self::SeeAlso,
            "warnings" => Self::Warnings,
            "references" => Self::References,
            "methods" => Self::Methods,
            _ => return None,
        })
    }
}

/// A parsed docstring section.
#[derive(Debug)]
struct ParsedSection {
    section: Section,
    heading: String,
    body: Vec<String>,
    numpy: bool,
}

/// Converts a dedented docstring to RST.
///
/// When neither style is enabled (or no section is detected) the original
/// text is returned unchanged, so ordinary RST docstrings still work.
#[must_use]
pub fn convert_docstring(docstring: &str, options: &NapoleonOptions) -> String {
    let cleaned = dedent_docstring(docstring);
    let lines: Vec<&str> = cleaned.lines().collect();
    let sections = if options.numpy {
        split_numpy(&lines, options)
    } else {
        Vec::new()
    };
    let sections = if sections.is_empty() && options.google {
        split_google(&lines, options)
    } else {
        sections
    };
    if sections.is_empty() {
        return cleaned;
    }

    let mut out: Vec<String> = Vec::new();
    // Preamble: everything before the first section header.
    let first_line = lines
        .iter()
        .enumerate()
        .position(|(index, _)| is_any_section_start(&lines, index, options))
        .unwrap_or(lines.len());
    for line in lines.iter().take(first_line) {
        out.push((*line).to_owned());
    }
    trim_trailing_blank(&mut out);

    for section in &sections {
        let rendered = render_section(section, options);
        if rendered.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.extend(rendered);
    }
    trim_trailing_blank(&mut out);
    out.join("\n")
}

/// Whether a line starts a section in either style.
fn is_any_section_start(lines: &[&str], index: usize, options: &NapoleonOptions) -> bool {
    let line = lines[index];
    if options.numpy
        && index + 1 < lines.len()
        && is_underline(lines[index + 1])
        && Section::from_numpy(line).is_some()
    {
        return true;
    }
    if options.google {
        if let Some(heading) = google_heading(line) {
            if Section::from_google(heading).is_some() {
                return true;
            }
        }
    }
    false
}

fn is_underline(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && trimmed.chars().all(|ch| ch == '-' || ch == '=' || ch == '~')
        && trimmed.chars().next().is_some_and(|ch| ch == '-')
}

fn google_heading(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.ends_with(':') && trimmed.len() > 1 {
        // A single token such as `Note:` is a header; reject lines that look
        // like a field (`x: something`) by requiring no text after the colon.
        let inner = trimmed.trim_end_matches(':');
        if !inner.is_empty() && !inner.contains('\n') {
            return Some(inner);
        }
    }
    None
}

/// Splits NumPy-style sections using underlined headings.
fn split_numpy(lines: &[&str], options: &NapoleonOptions) -> Vec<ParsedSection> {
    let mut sections = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let next = index + 1;
        if options.numpy
            && next < lines.len()
            && is_underline(lines[next])
            && Section::from_numpy(lines[index]).is_some()
        {
            let section = Section::from_numpy(lines[index]).unwrap_or(Section::Generic);
            let heading = lines[index].trim().to_owned();
            index = next + 1;
            let start = index;
            while index < lines.len() {
                if options.numpy
                    && index + 1 < lines.len()
                    && is_underline(lines[index + 1])
                    && Section::from_numpy(lines[index]).is_some()
                {
                    break;
                }
                if options.google
                    && google_heading(lines[index]).is_some_and(|name| Section::from_google(name).is_some())
                {
                    break;
                }
                index += 1;
            }
            sections.push(ParsedSection {
                section,
                heading,
                body: lines[start..index].iter().map(|line| (*line).to_owned()).collect(),
                numpy: true,
            });
        } else {
            index += 1;
        }
    }
    sections
}

/// Splits Google-style sections using `Name:` headings.
fn split_google(lines: &[&str], options: &NapoleonOptions) -> Vec<ParsedSection> {
    if !options.google {
        return Vec::new();
    }
    let mut sections = Vec::new();
    let mut current: Option<ParsedSection> = None;
    for line in lines {
        if let Some(heading) = google_heading(line) {
            if let Some(section) = Section::from_google(heading) {
                if let Some(previous) = current.take() {
                    sections.push(previous);
                }
                current = Some(ParsedSection {
                    section,
                    heading: heading.to_owned(),
                    body: Vec::new(),
                    numpy: false,
                });
                continue;
            }
        }
        if let Some(section) = current.as_mut() {
            section.body.push((*line).to_owned());
        }
    }
    if let Some(previous) = current {
        sections.push(previous);
    }
    sections
}

/// Renders one section to RST lines.
fn render_section(section: &ParsedSection, options: &NapoleonOptions) -> Vec<String> {
    let body = trim_blank_lines(&section.body);
    match section.section {
        Section::Parameters | Section::KeywordArgs | Section::OtherParams => {
            render_params(&body, section.section == Section::KeywordArgs, section.numpy, options)
        }
        Section::Returns => render_return(&body, "returns", options.use_rtype),
        Section::Yields => render_field_with_type("Yields", &body),
        Section::Receives => render_receives(&body),
        Section::Raises => render_raises(&body),
        Section::Warns => render_field_with_type("Warns", &body),
        Section::Attributes => render_attributes(&body, section.numpy, options),
        Section::Examples => {
            if options.use_admonition_for_examples {
                directive_block("admonition:: Examples", &body, "   ")
            } else {
                rubric("Examples", &body)
            }
        }
        Section::Notes => render_notes(&body, section.numpy, options),
        Section::SeeAlso => render_see_also(&body, section.numpy),
        Section::Warnings => directive("warning", &body),
        Section::References => rubric("References", &body),
        Section::Methods => render_methods(&body, section.numpy),
        Section::Generic => rubric(&section.heading, &body),
    }
}

/// A parameter entry: name, optional type and description lines.
#[derive(Debug)]
struct ParamEntry {
    name: String,
    type_name: Option<String>,
    description: Vec<String>,
}

/// Parses `name (type): description` / `name : type` style parameter lines.
fn parse_params(body: &[String], numpy: bool) -> Vec<ParamEntry> {
    let mut entries: Vec<ParamEntry> = Vec::new();
    let mut field_indent: Option<usize> = None;
    for line in body {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(entry) = entries.last_mut() {
                entry.description.push(String::new());
            }
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let is_field = field_indent.is_none_or(|level| indent <= level);
        let parsed = if is_field {
            if numpy { parse_numpy_field(trimmed) } else { parse_google_field(trimmed) }
        } else {
            None
        };
        if let Some((name, type_name, rest)) = parsed {
            field_indent = Some(indent);
            let mut description = Vec::new();
            if !rest.is_empty() {
                description.push(rest);
            }
            entries.push(ParamEntry { name, type_name, description });
        } else if let Some(entry) = entries.last_mut() {
            let text = if line.len() > indent { &line[indent.min(line.len())..] } else { trimmed };
            entry.description.push(text.to_owned());
        }
    }
    for entry in &mut entries {
        while entry.description.first().is_some_and(String::is_empty) {
            entry.description.remove(0);
        }
        while entry.description.last().is_some_and(String::is_empty) {
            entry.description.pop();
        }
    }
    entries
}

/// Parses `name (type): description` (Google) into its parts.
fn parse_google_field(line: &str) -> Option<(String, Option<String>, String)> {
    let (lhs, rhs) = line.split_once(':')?;
    let lhs = lhs.trim();
    if lhs.is_empty() {
        return None;
    }
    let (name, type_name) = if let Some(open) = lhs.find('(') {
        if lhs.ends_with(')') {
            (lhs[..open].trim(), Some(lhs[open + 1..lhs.len() - 1].trim().to_owned()))
        } else {
            (lhs, None)
        }
    } else {
        (lhs, None)
    };
    if name.is_empty() {
        return None;
    }
    Some((name.to_owned(), type_name, rhs.trim().to_owned()))
}

/// Parses `name : type` followed by an indented description (NumPy).
fn parse_numpy_field(line: &str) -> Option<(String, Option<String>, String)> {
    // NumPy entries do not contain a colon on the field line unless they are
    // a description continuation. `name : type` is the field form.
    let (name, type_name) = if let Some((name, type_name)) = line.split_once(" : ") {
        (name.trim(), Some(type_name.trim().to_owned()))
    } else {
        (line, None)
    };
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    Some((name.to_owned(), type_name.filter(|value| !value.is_empty()), String::new()))
}

fn render_params(body: &[String], keyword: bool, numpy: bool, options: &NapoleonOptions) -> Vec<String> {
    let entries = parse_params(body, numpy);
    let mut out = Vec::new();
    for entry in entries {
        let role = if keyword && options.use_keyword { "keyword" } else { "param" };
        let mut field = format!(":{role} {}:", entry.name);
        if !entry.description.is_empty() {
            field.push(' ');
            field.push_str(entry.description.first().map_or("", String::as_str));
        }
        out.push(field);
        for continuation in entry.description.iter().skip(1) {
            if continuation.is_empty() {
                out.push(String::new());
            } else {
                out.push(format!("   {continuation}"));
            }
        }
        if let Some(type_name) = entry.type_name {
            if options.use_rtype || options.use_param {
                out.push(format!(":type {}: {}", entry.name, type_name));
            }
        }
    }
    trim_trailing_blank(&mut out);
    out
}

/// Renders a `Returns:` section as `:returns:` / `:rtype:`.
fn render_return(body: &[String], field: &str, use_rtype: bool) -> Vec<String> {
    let entries = parse_typed_entries(body);
    let mut out = Vec::new();
    if let Some(entry) = entries.first() {
        let mut line = format!(":{field}:");
        if !entry.description.is_empty() {
            line.push(' ');
            line.push_str(&entry.description.join(" "));
        }
        out.push(line);
        if use_rtype {
            if let Some(type_name) = &entry.type_name {
                out.push(format!(":rtype: {type_name}"));
            }
        }
    } else {
        let text = body.iter().map(String::as_str).collect::<Vec<_>>().join(" ");
        if !text.trim().is_empty() {
            out.push(format!(":{field}: {}", text.trim()));
        }
    }
    out
}

/// Renders `:Yields: *type* -- description` style fields.
fn render_field_with_type(name: &str, body: &[String]) -> Vec<String> {
    let entries = parse_typed_entries(body);
    let mut out = Vec::new();
    for entry in entries {
        let mut value = String::new();
        if let Some(type_name) = &entry.type_name {
            value.push_str(&format!("*{type_name}* -- "));
        }
        value.push_str(&entry.description.join(" "));
        out.push(format!(":{name}: {value}"));
    }
    out
}

fn render_receives(body: &[String]) -> Vec<String> {
    let entries = parse_params(body, false);
    let mut out = Vec::new();
    for entry in entries {
        let mut line = format!(":param {}:", entry.name);
        if !entry.description.is_empty() {
            line.push(' ');
            line.push_str(&entry.description.join(" "));
        }
        out.push(line);
    }
    out
}

fn render_raises(body: &[String]) -> Vec<String> {
    let entries = parse_typed_entries(body);
    let mut out = Vec::new();
    for entry in entries {
        let kind = entry.type_name.unwrap_or_else(|| entry.name.clone());
        let description = entry.description.join(" ");
        out.push(format!(":raises {kind}: {description}"));
    }
    out
}

fn render_attributes(body: &[String], numpy: bool, options: &NapoleonOptions) -> Vec<String> {
    let entries = parse_params(body, numpy);
    let mut out = Vec::new();
    for entry in entries {
        if options.use_ivar {
            let mut line = format!(":ivar {}:", entry.name);
            if !entry.description.is_empty() {
                line.push(' ');
                line.push_str(&entry.description.join(" "));
            }
            out.push(line);
            if let Some(type_name) = entry.type_name {
                out.push(format!(":vartype {}: {}", entry.name, type_name));
            }
        } else {
            out.push(format!(".. attribute:: {}", entry.name));
            out.push(String::new());
            for line in &entry.description {
                out.push(format!("   {line}"));
            }
            out.push(String::new());
        }
    }
    trim_trailing_blank(&mut out);
    out
}

fn render_methods(body: &[String], numpy: bool) -> Vec<String> {
    let entries = parse_params(body, numpy);
    let mut out = Vec::new();
    for entry in entries {
        out.push(format!(".. method:: {}", entry.name));
        out.push(String::new());
        for line in &entry.description {
            out.push(format!("   {line}"));
        }
        out.push(String::new());
    }
    trim_trailing_blank(&mut out);
    out
}

fn render_notes(body: &[String], numpy: bool, options: &NapoleonOptions) -> Vec<String> {
    // Google's `Note:` is an admonition regardless of the
    // `napoleon_use_admonition_for_notes` toggle; NumPy's `Notes` section is a
    // rubric unless that toggle is set.
    if !numpy || options.use_admonition_for_notes {
        directive("note", &dedent_body(body))
    } else {
        rubric("Notes", &dedent_body(body))
    }
}

fn render_see_also(body: &[String], numpy: bool) -> Vec<String> {
    let text: Vec<&str> = body.iter().map(String::as_str).filter(|line| !line.trim().is_empty()).collect();
    if text.is_empty() {
        return Vec::new();
    }
    if numpy {
        let mut out = vec![".. seealso::".to_owned(), String::new()];
        for line in &text {
            if let Some((name, description)) = line.split_once(':') {
                let name = name.trim();
                let description = description.trim();
                out.push(format!("   :py:obj:`{name}`"));
                if !description.is_empty() {
                    out.push(format!("       {description}"));
                }
            } else {
                out.push(format!("   {}", line.trim()));
            }
        }
        return out;
    }
    if text.len() == 1 && !text[0].contains(':') {
        return vec![format!(".. seealso:: {}", text[0].trim())];
    }
    let mut out = vec![".. seealso::".to_owned(), String::new()];
    for line in text {
        out.push(format!("   {}", line.trim()));
    }
    out
}

fn dedent_body(body: &[String]) -> Vec<String> {
    let min = body
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    body.iter()
        .map(|line| if line.len() >= min { line[min..].to_owned() } else { line.trim().to_owned() })
        .collect()
}

fn directive(name: &str, body: &[String]) -> Vec<String> {
    let text: Vec<&str> = body.iter().map(String::as_str).filter(|line| !line.trim().is_empty()).collect();
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(".. {name}:: {}", text[0].trim())];
    for line in text.iter().skip(1) {
        out.push(format!("   {}", line.trim()));
    }
    out
}

fn directive_block(header: &str, body: &[String], indent: &str) -> Vec<String> {
    let mut out = vec![format!(".. {header}")];
    for line in body {
        if line.trim().is_empty() {
            out.push(String::new());
        } else {
            out.push(format!("{indent}{}", line.trim_start()));
        }
    }
    trim_trailing_blank(&mut out);
    out
}

fn rubric(title: &str, body: &[String]) -> Vec<String> {
    let body = dedent_body(body);
    let mut out = vec![format!(".. rubric:: {title}"), String::new()];
    for line in &body {
        out.push(line.clone());
    }
    trim_trailing_blank(&mut out);
    out
}

/// A typed entry: first line carries `name`/`type`, rest is description.
#[derive(Debug)]
struct TypedEntry {
    name: String,
    type_name: Option<String>,
    description: Vec<String>,
}

fn parse_typed_entries(body: &[String]) -> Vec<TypedEntry> {
    let mut entries: Vec<TypedEntry> = Vec::new();
    let mut field_indent: Option<usize> = None;
    for line in body {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(entry) = entries.last_mut() {
                entry.description.push(String::new());
            }
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let is_field = field_indent.is_none_or(|level| indent <= level);
        let candidate = if is_field { parse_entry_heading(trimmed) } else { None };
        if let Some((name, type_name, rest)) = candidate {
            field_indent = Some(indent);
            let mut description = Vec::new();
            if !rest.is_empty() {
                description.push(rest);
            }
            entries.push(TypedEntry { name, type_name, description });
        } else if let Some(entry) = entries.last_mut() {
            let text = if line.len() > indent { &line[indent.min(line.len())..] } else { trimmed };
            entry.description.push(text.trim().to_owned());
        }
    }
    for entry in &mut entries {
        while entry.description.first().is_some_and(String::is_empty) {
            entry.description.remove(0);
        }
        while entry.description.last().is_some_and(String::is_empty) {
            entry.description.pop();
        }
    }
    entries
}

/// Parses a Google `Type: description` heading; NumPy's first line is just the
/// type with the description indented below.
fn parse_entry_heading(line: &str) -> Option<(String, Option<String>, String)> {
    if let Some((name, rest)) = line.split_once(':') {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let rest = rest.trim();
        let (type_name, description) = if let Some(value) = rest.strip_prefix('(').and_then(|value| value.strip_suffix(')')) {
            (Some(value.to_owned()), String::new())
        } else if rest.is_empty() {
            (None, String::new())
        } else {
            (Some(name.to_owned()), rest.to_owned())
        };
        return Some((name.to_owned(), type_name, description));
    }
    // NumPy style: the whole line is the type name.
    if !line.contains(' ') {
        return Some((line.to_owned(), Some(line.to_owned()), String::new()));
    }
    None
}

fn trim_blank_lines(lines: &[String]) -> Vec<String> {
    let mut out = lines.to_vec();
    while out.first().is_some_and(String::is_empty) {
        out.remove(0);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

fn trim_trailing_blank(lines: &mut Vec<String>) {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_args_and_returns() {
        let doc = "Summary.\n\nArgs:\n    x (int): the x\n    y: the y\n\nReturns:\n    int: the result\n";
        let rst = convert_docstring(doc, &NapoleonOptions::default());
        assert!(rst.contains(":param x: the x"), "{rst}");
        assert!(rst.contains(":type x: int"), "{rst}");
        assert!(rst.contains(":param y: the y"), "{rst}");
        assert!(rst.contains(":returns: the result"), "{rst}");
        assert!(rst.contains(":rtype: int"), "{rst}");
    }

    #[test]
    fn numpy_parameters_and_returns() {
        let doc = "Summary.\n\nParameters\n----------\nx : int\n    the x\n\nReturns\n-------\nint\n    the result\n";
        let rst = convert_docstring(doc, &NapoleonOptions::default());
        assert!(rst.contains(":param x: the x"), "{rst}");
        assert!(rst.contains(":type x: int"), "{rst}");
        assert!(rst.contains(":returns: the result"), "{rst}");
    }

    #[test]
    fn google_raises_and_admonitions() {
        let doc = "Summary.\n\nRaises:\n    ValueError: bad input\n\nExamples:\n    >>> f()\n\nSee Also:\n    other\n\nWarning:\n    careful\n";
        let rst = convert_docstring(doc, &NapoleonOptions::default());
        assert!(rst.contains(":raises ValueError: bad input"), "{rst}");
        assert!(rst.contains(".. rubric:: Examples"), "{rst}");
        assert!(rst.contains(".. seealso:: other"), "{rst}");
        assert!(rst.contains(".. warning:: careful"), "{rst}");
    }

    #[test]
    fn plain_rst_is_unchanged() {
        let doc = "Summary.\n\n:param x: plain\n:returns: value\n";
        assert_eq!(convert_docstring(doc, &NapoleonOptions::default()), doc.trim_end());
    }
}
