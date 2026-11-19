//! MyST directive fenced blocks and nested-content conversion.
//!
//! A MyST fenced directive is a fenced code block whose info string begins
//! with `{name}`:
//!
//! ````markdown
//! ```{note} optional arguments
//! :class: my-class
//!
//! Body text.
//! ```
//! ````
//!
//! `parse_directive_info` splits the info string. `parse_directive_body`
//! separates leading `:option: value` lines from the content and dedents the
//! content, producing exactly the Phase 06 `Pending` payload (§8.5 of
//! `ARCHITECTURE.md`). `myst_content_to_rst` rewrites MyST-only constructs in
//! a directive's nested content into RST syntax so `scribe_rst::resolve_pending`
//! can resolve arbitrarily nested directives through the shared registry.

use std::collections::BTreeMap;

/// Parsed fenced-directive info string (`{name} arguments`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveInfo {
    /// Lower-cased directive name.
    pub name: String,
    /// Raw argument string after the closing brace.
    pub arguments: String,
}

/// Parses a fenced code block info string into directive name and arguments.
///
/// Returns `None` when the info string does not start with `{name}`.
pub fn parse_directive_info(info: &str) -> Option<DirectiveInfo> {
    let trimmed = info.trim_start();
    let open = trimmed.strip_prefix('{')?;
    let close = open.find('}')?;
    let name = open[..close].trim().to_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return None;
    }
    Some(DirectiveInfo {
        name,
        arguments: open[close + 1..].trim().to_owned(),
    })
}

/// Splits a directive body into typed options and dedented content.
///
/// Option lines (`name: value` after a leading colon) are consumed from the
/// start of the body; the remainder is the raw content. The returned content
/// is the dedented body joined with `\n`, exactly the shape Phase 06 expects.
pub fn parse_directive_body(body: &str) -> (BTreeMap<String, String>, String) {
    let mut options = BTreeMap::new();
    let mut content_lines: Vec<&str> = Vec::new();
    let mut in_options = true;
    for line in body.lines() {
        if in_options {
            if let Some((name, value)) = option_line(line) {
                options.insert(name, value);
                continue;
            }
            if line.trim().is_empty() {
                // A blank line ends the option block; do not emit it as
                // leading content (RST directives drop the option separator).
                in_options = false;
                continue;
            }
            in_options = false;
        }
        content_lines.push(line);
    }

    // Dedent the content by the smallest common indent of its non-blank lines.
    let indent = content_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    let content = content_lines
        .iter()
        .map(|line| {
            let cut = indent.min(line.len());
            &line[cut..]
        })
        .collect::<Vec<_>>()
        .join("\n");
    (options, content)
}

/// Parses a single directive option line (`name: value`).
fn option_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start();
    let after = rest.strip_prefix(':')?;
    let (name, value) = after.split_once(':')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    Some((name.to_owned(), value.trim().to_owned()))
}

/// Rewrites MyST-only constructs in directive content into RST syntax.
///
/// This makes nested directives resolvable by the shared RST registry:
/// fenced `{directive}` blocks become `.. directive::` blocks (recursively),
/// `` {role}`payload` `` becomes `` :role:`payload` ``, and `(label)=` becomes
/// `.. _label:`. Content with no MyST-specific syntax is returned unchanged so
/// the Phase 06 `content` payload stays byte-identical to the source.
pub fn myst_content_to_rst(content: &str) -> String {
    if !content.contains('{')
        && !content.contains("$$")
        && !content.contains(")=")
        && !content.contains(":::")
        && !content.contains("```")
    {
        return content.to_owned();
    }
    let lines: Vec<&str> = content.lines().collect();
    convert_lines(&lines)
}

fn convert_lines(lines: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if let Some(open) = fence_opener(line) {
            // Find the matching closing fence (same character, >= length).
            let mut j = i + 1;
            while j < lines.len() && !is_closing_fence(lines[j], open.ch, open.len) {
                j += 1;
            }
            let inner = &lines[i + 1..j.min(lines.len())];
            let (options, body) = parse_directive_body(&inner.join("\n"));
            let converted_body = convert_lines(&body.lines().collect::<Vec<_>>());
            out.push(format!(".. {}:: {}", open.name, open.arguments));
            for (name, value) in options {
                out.push(format!("   :{name}: {value}"));
            }
            if !converted_body.trim().is_empty() {
                out.push(String::new());
                for body_line in converted_body.lines() {
                    if body_line.is_empty() {
                        out.push(String::new());
                    } else {
                        out.push(format!("   {body_line}"));
                    }
                }
            }
            i = j.saturating_add(1);
            continue;
        }
        if let Some(label) = crate::lower::target_label(line.trim()) {
            out.push(format!(".. _{label}:"));
            i += 1;
            continue;
        }
        out.push(convert_inline(line));
        i += 1;
    }
    out.join("\n")
}

struct FenceOpener {
    ch: u8,
    len: usize,
    name: String,
    arguments: String,
}

fn fence_opener(line: &str) -> Option<FenceOpener> {
    let trimmed = line.trim_start();
    let ch = *trimmed.as_bytes().first()?;
    if ch != b'`' && ch != b':' {
        return None;
    }
    let len = trimmed.bytes().take_while(|&b| b == ch).count();
    if len < 3 {
        return None;
    }
    let info = &trimmed[len..];
    let parsed = parse_directive_info(info)?;
    Some(FenceOpener {
        ch,
        len,
        name: parsed.name,
        arguments: parsed.arguments,
    })
}

fn is_closing_fence(line: &str, ch: u8, min_len: usize) -> bool {
    let trimmed = line.trim();
    let count = trimmed.bytes().take_while(|&b| b == ch).count();
    count >= min_len && trimmed[count..].trim().is_empty()
}

/// Converts inline MyST roles and math into RST equivalents (best effort).
fn convert_inline(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // `{role}`payload`` → `:role:`payload``
        if bytes[i] == b'{' {
            if let Some(close) = line[i + 1..].find('}') {
                let role_end = i + 1 + close;
                let role = &line[i + 1..role_end];
                if !role.is_empty()
                    && role
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '+'))
                    && line[role_end + 1..].starts_with('`')
                {
                    out.push(':');
                    out.push_str(role);
                    out.push(':');
                    i = role_end + 1;
                    continue;
                }
            }
        }
        // `$expr$` → `:math:`expr`` (leave `$$…$$` for a later phase).
        if bytes[i] == b'$' && (i == 0 || bytes[i - 1] != b'\\') {
            let rest = &line[i + 1..];
            if !rest.starts_with('$') {
                if let Some(end) = rest.find('$') {
                    if end > 0 {
                        out.push_str(":math:`");
                        out.push_str(&rest[..end]);
                        out.push('`');
                        i = i + 1 + end + 1;
                        continue;
                    }
                }
            }
        }
        let ch = line[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_info_strings() {
        let info = parse_directive_info("{note} some argument").expect("info");
        assert_eq!(info.name, "note");
        assert_eq!(info.arguments, "some argument");
        assert!(parse_directive_info("rust").is_none());
    }

    #[test]
    fn splits_options_and_content() {
        let (options, content) = parse_directive_body(":class: foo\n:name: bar\n\nHello.\n");
        assert_eq!(options.get("class").map(String::as_str), Some("foo"));
        assert_eq!(options.get("name").map(String::as_str), Some("bar"));
        assert_eq!(content, "Hello.");
    }

    #[test]
    fn plain_content_is_unchanged() {
        let body = "just text\nmore text\n";
        assert_eq!(myst_content_to_rst(body), body);
    }

    #[test]
    fn converts_nested_fences_and_roles() {
        let body = "Intro {ref}`x`.\n\n```{note}\nnested {math}`y`\n```\n";
        let rst = myst_content_to_rst(body);
        assert!(rst.contains(".. note::"), "{rst}");
        assert!(rst.contains("nested :math:`y`"), "{rst}");
        assert!(rst.contains("Intro :ref:`x`."), "{rst}");
    }
}
