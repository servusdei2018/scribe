//! Byte-compatible `objects.inv` reader and writer.
//!
//! Behavior is transcribed from `sphinx/sphinx/util/inventory.py`
//! (`InventoryFile.loads`, `InventoryFile._loads_v1`, `InventoryFile._loads_v2`
//! and `InventoryFile.dump`) of the pinned oracle.  In particular:
//!
//! * the v2 header is four lines and the compressed body uses zlib level 9;
//! * entry lines match `(.+?)\s+(\S+)\s+(-?\d+)\s+?(\S*)\s+(.*)`;
//! * `std:label` / `std:term` names match case-insensitively with ambiguity
//!   detection, all other types match exactly;
//! * a location ending in `$` has the object name appended, then is joined to
//!   the inventory base URI with `posixpath.join` semantics;
//! * writer output is deterministic: domains sorted by name, objects sorted by
//!   the full six-uple, anchors ending in the object name shortened to `$`,
//!   and `display_name == fullname` collapsed to `-`.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::sync::OnceLock;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use regex::Regex;

use crate::types::ObjectEntry;
use super::{IntersphinxError, IntersphinxResult};

const HEADER_PREFIX: &str = "# Sphinx inventory version ";
const HEADER_V1: &str = "# Sphinx inventory version 1";
const HEADER_V2: &str = "# Sphinx inventory version 2";
const CHECK_MARKER: &[u8] = b"zlib";
const PROJECT_PREFIX_LEN: usize = "# Project: ".len();
const VERSION_PREFIX_LEN: usize = "# Version: ".len();

/// One resolved inventory entry (upstream `_InventoryItem`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InventoryItem {
    /// Project name from the inventory header.
    pub project_name: String,
    /// Project version from the inventory header (may be empty).
    pub project_version: String,
    /// Fully joined URI of the object.
    pub uri: String,
    /// Display name (`"-"` means "use the reference's own title").
    pub display_name: String,
}

/// A flattened inventory entry, convenient for export and tests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InventoryObject {
    /// Inventory object type, e.g. `py:func`.
    pub objtype: String,
    /// Object name.
    pub name: String,
    /// The resolved entry.
    pub item: InventoryItem,
}

/// An in-memory inventory: `objtype -> name -> item`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    /// Project name from the header.
    pub project_name: String,
    /// Project version from the header.
    pub project_version: String,
    /// Inventory data keyed by fully-qualified type (`domain:objtype`).
    pub data: BTreeMap<String, BTreeMap<String, InventoryItem>>,
    /// Definitions that were found to have more than one distinct target
    /// during a case-insensitive (`std:label` / `std:term`) merge.
    pub ambiguities: Vec<String>,
}

impl Inventory {
    /// Parses inventory bytes, joining relative locations onto `base_uri`.
    ///
    /// Both v1 (deprecated but still readable) and v2 are supported.  Any
    /// malformed input is reported as an [`IntersphinxError`]; no panic.
    pub fn loads(content: &[u8], base_uri: &str) -> IntersphinxResult<Self> {
        let (format_line, rest) = match content.iter().position(|&byte| byte == b'\n') {
            Some(position) => (&content[..position], &content[position + 1..]),
            None => (content, &content[content.len()..]),
        };
        let format_line = trim_cr_and_spaces(format_line);
        let format = String::from_utf8_lossy(format_line);
        match format.as_ref() {
            HEADER_V2 => Self::loads_v2(rest, base_uri),
            HEADER_V1 => Self::loads_v1(rest, base_uri),
            _ if format.starts_with(HEADER_PREFIX) => Err(IntersphinxError::UnsupportedVersion(
                format[HEADER_PREFIX.len()..].to_owned(),
            )),
            _ => Err(IntersphinxError::InvalidHeader(format.into_owned())),
        }
    }

    /// Reads the deprecated v1 format gracefully.
    fn loads_v1(content: &[u8], base_uri: &str) -> IntersphinxResult<Self> {
        let text = std::str::from_utf8(content)
            .map_err(|error| IntersphinxError::InvalidHeader(error.to_string()))?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 2 {
            return Err(IntersphinxError::InvalidHeader(
                "missing project name or version".to_owned(),
            ));
        }
        let project_name = header_value(lines[0], PROJECT_PREFIX_LEN);
        let project_version = header_value(lines[1], VERSION_PREFIX_LEN);
        let mut inventory = Self {
            project_name,
            project_version,
            data: BTreeMap::new(),
            ambiguities: Vec::new(),
        };
        for line in lines.iter().skip(2) {
            let trimmed = line.trim_end();
            let mut parts = trimmed.splitn(3, char::is_whitespace).filter(|part| !part.is_empty());
            let (Some(name), Some(item_type), Some(location)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let joined = posixpath_join(base_uri, location);
            let owned_type = (item_type != "mod").then(|| format!("py:{item_type}"));
            let objtype = owned_type.as_deref().unwrap_or("py:module");
            let uri = if item_type == "mod" {
                format!("{joined}#module-{name}")
            } else {
                format!("{joined}#{name}")
            };
            inventory.insert(
                objtype,
                name,
                InventoryItem {
                    project_name: inventory.project_name.clone(),
                    project_version: inventory.project_version.clone(),
                    uri,
                    display_name: "-".to_owned(),
                },
            );
        }
        Ok(inventory)
    }

    /// Reads the v2 format (zlib-compressed body).
    fn loads_v2(content: &[u8], base_uri: &str) -> IntersphinxResult<Self> {
        // Split into at most four parts: project, version, check line, body.
        let mut chunks = content.splitn(4, |&byte| byte == b'\n');
        let line_1 = chunks.next().unwrap_or_default();
        let line_2 = chunks.next();
        let check_line = chunks.next();
        let compressed = chunks.next();
        let (Some(line_2), Some(check_line), Some(compressed)) = (line_2, check_line, compressed)
        else {
            return Err(IntersphinxError::InvalidHeader(
                "missing project name or version".to_owned(),
            ));
        };
        if !check_line.windows(CHECK_MARKER.len()).any(|window| window == CHECK_MARKER) {
            return Err(IntersphinxError::InvalidHeader(format!(
                "not compressed: {}",
                String::from_utf8_lossy(check_line)
            )));
        }

        let project_name = String::from_utf8_lossy(header_bytes(line_1, PROJECT_PREFIX_LEN)).into_owned();
        let project_version = String::from_utf8_lossy(header_bytes(line_2, VERSION_PREFIX_LEN)).into_owned();

        let mut decoder = ZlibDecoder::new(compressed);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|error| IntersphinxError::Decompress(error.to_string()))?;
        let body = std::str::from_utf8(&decompressed)
            .map_err(|error| IntersphinxError::Decompress(error.to_string()))?;

        let mut inventory = Self {
            project_name,
            project_version,
            data: BTreeMap::new(),
            ambiguities: Vec::new(),
        };
        // Upstream tracks case-insensitive definitions to detect ambiguous
        // `std:label` / `std:term` entries.  It stores (priority, location,
        // display name) as the comparison payload.
        let mut potential: BTreeMap<String, (String, String, String)> = BTreeMap::new();
        let mut actual: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let pattern = entry_pattern();

        for raw_line in body.lines() {
            let line = raw_line.trim_end();
            let Some(captures) = pattern.captures(line) else {
                continue;
            };
            let name = captures.get(1).map_or("", |m| m.as_str());
            let objtype = captures.get(2).map_or("", |m| m.as_str());
            let priority = captures.get(3).map_or("", |m| m.as_str());
            let mut location = captures.get(4).map_or("", |m| m.as_str()).to_owned();
            let display_name = captures.get(5).map_or("", |m| m.as_str()).to_owned();

            if !objtype.contains(':') {
                // Inventory types must be "{domain}:{objtype}".  Upstream
                // skips malformed types (regex-DoS hardening from issue #8175).
                continue;
            }
            if objtype == "py:module" && inventory.contains(objtype, name) {
                // Upstream 1.1 and below emitted two entries for Python
                // modules; the first one is authoritative.
                continue;
            }

            if objtype == "std:label" || objtype == "std:term" {
                let definition = format!("{objtype}:{name}");
                let content = (priority.to_owned(), location.clone(), display_name.clone());
                let lowercase = definition.to_lowercase();
                if let Some(existing) = potential.get(&lowercase) {
                    if existing != &content {
                        actual.insert(definition.clone());
                    }
                } else {
                    potential.insert(lowercase, content);
                }
            }

            if let Some(stripped) = location.strip_suffix('$') {
                location = format!("{stripped}{name}");
            }
            let uri = posixpath_join(base_uri, &location);
            inventory.insert(
                objtype,
                name,
                InventoryItem {
                    project_name: inventory.project_name.clone(),
                    project_version: inventory.project_version.clone(),
                    uri,
                    display_name,
                },
            );
        }
        inventory.ambiguities.extend(actual);
        Ok(inventory)
    }

    /// Inserts one item, returning the previous value if the key existed.
    fn insert(&mut self, objtype: &str, name: &str, item: InventoryItem) -> Option<InventoryItem> {
        self.data.entry(objtype.to_owned()).or_default().insert(name.to_owned(), item)
    }

    /// Exact lookup by inventory object type and name.
    pub fn get(&self, objtype: &str, name: &str) -> Option<&InventoryItem> {
        self.data.get(objtype).and_then(|objects| objects.get(name))
    }

    /// Lookup by the three-part `(domain, objtype, name)` key used by
    /// intersphinx resolution.
    pub fn get_by_parts(&self, domain: &str, objtype: &str, name: &str) -> Option<&InventoryItem> {
        self.get(&format!("{domain}:{objtype}"), name)
    }

    /// True when `(objtype, name)` is present.
    pub fn contains(&self, objtype: &str, name: &str) -> bool {
        self.get(objtype, name).is_some()
    }

    /// Case-insensitive-aware lookup for `std:label` / `std:term`.
    ///
    /// Returns the matched (possibly differently-cased) name together with the
    /// item.  Exact matches always win.  Ambiguous case-insensitive matches
    /// resolve to the first key in sorted order and are recorded in
    /// [`Inventory::ambiguities`] at parse time.
    pub fn lookup<'a>(&'a self, objtype: &str, name: &str) -> Option<(&'a str, &'a InventoryItem)> {
        let objects = self.data.get(objtype)?;
        if let Some((key, item)) = objects.get_key_value(name) {
            return Some((key.as_str(), item));
        }
        if objtype != "std:label" && objtype != "std:term" {
            return None;
        }
        let lowered = name.to_lowercase();
        objects
            .iter()
            .find(|(candidate, _)| candidate.to_lowercase() == lowered)
            .map(|(candidate, item)| (candidate.as_str(), item))
    }

    /// All entries in deterministic order.
    pub fn get_objects(&self) -> Vec<InventoryObject> {
        let mut result = Vec::new();
        for (objtype, objects) in &self.data {
            for (name, item) in objects {
                result.push(InventoryObject {
                    objtype: objtype.clone(),
                    name: name.clone(),
                    item: item.clone(),
                });
            }
        }
        result.sort();
        result
    }

    /// Number of entries across all object types.
    pub fn len(&self) -> usize {
        self.data.values().map(BTreeMap::len).sum()
    }

    /// True when the inventory contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Serializes an inventory to the byte-exact Sphinx v2 format.
    ///
    /// `objects` is keyed by domain name so the writer can emit fully
    /// qualified `{domain}:{objtype}` type strings, exactly like upstream's
    /// `for domain in env.domains.sorted()` loop.
    pub fn dump(
        objects: &BTreeMap<String, Vec<ObjectEntry>>,
        config: &DumpConfig<'_>,
        builder_uri: impl Fn(&str) -> String,
    ) -> Vec<u8> {
        let mut output = format!(
            "{HEADER_V2}\n# Project: {}\n# Version: {}\n\
             # The remainder of this file is compressed using zlib.\n",
            collapse_whitespace(config.project),
            collapse_whitespace(config.version),
        )
        .into_bytes();

        let mut body = String::new();
        for (domain_name, domain_objects) in objects {
            let mut sorted = domain_objects.clone();
            sorted.sort();
            for entry in sorted {
                let mut anchor = entry.anchor.clone();
                if !entry.name.is_empty() && anchor.ends_with(&entry.name) {
                    anchor.truncate(anchor.len() - entry.name.len());
                    anchor.push('$');
                }
                let mut uri = builder_uri(&entry.docname);
                if !anchor.is_empty() {
                    uri.push('#');
                    uri.push_str(&anchor);
                }
                let display_name = if entry.display_name == entry.name {
                    "-"
                } else {
                    &entry.display_name
                };
                body.push_str(&format!(
                    "{} {}:{} {} {} {}\n",
                    entry.name, domain_name, entry.objtype, entry.priority, uri, display_name,
                ));
            }
        }

        // Compression into an in-memory Vec is infallible.  The safety of the
        // `expect` below rests on `std::io::Write for Vec<u8>` never returning
        // an error and flate2 only propagating the inner writer's error.
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder
            .write_all(body.as_bytes())
            .expect("writing compressed bytes to an in-memory Vec cannot fail");
        let compressed = encoder
            .finish()
            .expect("finishing a zlib stream into an in-memory Vec cannot fail");
        output.extend_from_slice(&compressed);
        output
    }

    /// Serializes from a flat slice of `(domain, objects)` pairs.
    pub fn dump_pairs<'a>(
        objects: impl IntoIterator<Item = (&'a str, Vec<ObjectEntry>)>,
        config: &DumpConfig<'_>,
        builder_uri: impl Fn(&str) -> String,
    ) -> Vec<u8> {
        let grouped: BTreeMap<String, Vec<ObjectEntry>> = objects
            .into_iter()
            .map(|(domain, entries)| (domain.to_owned(), entries))
            .collect();
        Self::dump(&grouped, config, builder_uri)
    }
}

/// Config inputs for [`Inventory::dump`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DumpConfig<'a> {
    /// Project name written into the header.
    pub project: &'a str,
    /// Project version written into the header.
    pub version: &'a str,
}

/// Python's `posixpath.join` (no path normalization, just concatenation).
pub fn posixpath_join(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_owned();
    }
    if path.starts_with('/') {
        return path.to_owned();
    }
    if base.is_empty() || base.ends_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Collapses runs of whitespace to a single space, like `re.sub(r'\s+', ' ', s)`
/// (leading/trailing runs become a single space rather than being trimmed).
fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_whitespace = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_whitespace {
                result.push(' ');
            }
            in_whitespace = true;
        } else {
            result.push(ch);
            in_whitespace = false;
        }
    }
    result
}

/// Returns the bytes after the fixed-width header prefix, with trailing CR or
/// spaces removed (Python's `.rstrip()[11:]`).
fn header_bytes(line: &[u8], prefix_len: usize) -> &[u8] {
    let trimmed = trim_cr_and_spaces(line);
    trimmed.get(prefix_len..).unwrap_or_default()
}

fn header_value(line: &str, prefix_len: usize) -> String {
    line.trim_end_matches(['\r', ' ']).get(prefix_len..).unwrap_or_default().to_owned()
}

fn trim_cr_and_spaces(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b'\r' | b' ') {
        end -= 1;
    }
    &bytes[..end]
}

fn entry_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(.+?)\s+(\S+)\s+(-?\d+)\s+?(\S*)\s+(.*)")
            .expect("the inventory entry regex is a compile-time constant")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posixpath_join_matches_upstream() {
        assert_eq!(posixpath_join("/util", "foo.html"), "/util/foo.html");
        assert_eq!(posixpath_join("/util/", "foo.html"), "/util/foo.html");
        assert_eq!(posixpath_join("/util", "/abs"), "/abs");
        assert_eq!(posixpath_join("", "foo"), "foo");
        assert_eq!(posixpath_join("/util", ""), "/util");
    }

    #[test]
    fn collapse_whitespace_preserves_edges() {
        assert_eq!(collapse_whitespace("foo"), "foo");
        assert_eq!(collapse_whitespace("  a\t b\n c  "), " a b c ");
    }
}
