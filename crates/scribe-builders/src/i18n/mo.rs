//! GNU binary MO format reader/writer (Phase 26).
//!
//! Implemented directly so there is no runtime dependency on Babel or the
//! Python `gettext` module. The layout matches Babel's `write_mo` (no hash
//! table, NUL-terminated strings whose NUL is not counted in the length) and
//! Python's `gettext.GNUTranslations` reader.

use std::collections::BTreeMap;

use thiserror::Error;

const LE_MAGIC: u32 = 0x9504_12de;
const BE_MAGIC: u32 = 0xde12_0495;

/// Errors raised while reading or writing binary catalogs.
#[derive(Debug, Error)]
pub enum MoError {
    /// The magic number is not a GNU MO magic value.
    #[error("invalid MO magic: 0x{0:08x}")]
    InvalidMagic(u32),
    /// The revision is unsupported.
    #[error("unsupported MO revision: {0}")]
    UnsupportedRevision(u32),
    /// The file is truncated or a table offset is out of bounds.
    #[error("truncated or malformed MO catalog: {0}")]
    Malformed(&'static str),
    /// A string is not valid UTF-8.
    #[error("invalid UTF-8 in MO catalog: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    /// Writing a catalog failed.
    #[error("I/O error while writing MO catalog: {0}")]
    Io(#[from] std::io::Error),
}

fn read_u32(bytes: &[u8], offset: usize, little: bool) -> Result<u32, MoError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(MoError::Malformed("u32 outside file"))?;
    let array: [u8; 4] = slice.try_into().map_err(|_| MoError::Malformed("u32 slice"))?;
    Ok(if little { u32::from_le_bytes(array) } else { u32::from_be_bytes(array) })
}

/// Writes a binary MO catalog from `(msgid, msgstr)` entries.
///
/// Entries are sorted by `msgid` for deterministic output. Numeric overflow
/// and len/offset inconsistencies are reported rather than panicking.
pub fn write_mo(entries: &[(String, String)]) -> Result<Vec<u8>, MoError> {
    let mut sorted: Vec<&(String, String)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let count = u32::try_from(sorted.len()).map_err(|_| MoError::Malformed("too many entries"))?;

    let mut ids = Vec::new();
    let mut values = Vec::new();
    let mut offsets: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(sorted.len());
    for entry in &sorted {
        let (msgid, msgstr) = *entry;
        let id = msgid.as_bytes();
        let value = msgstr.as_bytes();
        let id_len = u32::try_from(id.len()).map_err(|_| MoError::Malformed("msgid too long"))?;
        let value_len = u32::try_from(value.len()).map_err(|_| MoError::Malformed("msgstr too long"))?;
        let id_offset = u32::try_from(ids.len()).map_err(|_| MoError::Malformed("catalog too large"))?;
        let value_offset = u32::try_from(values.len()).map_err(|_| MoError::Malformed("catalog too large"))?;
        offsets.push((id_offset, id_len, value_offset, value_len));
        ids.extend_from_slice(id);
        ids.push(0);
        values.extend_from_slice(value);
        values.push(0);
    }

    let header_size = 28usize;
    let index_size = 16usize.saturating_mul(sorted.len());
    let key_start = header_size
        .checked_add(index_size)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or(MoError::Malformed("key table offset overflow"))?;
    let value_start = key_start
        .checked_add(u32::try_from(ids.len()).map_err(|_| MoError::Malformed("ids too long"))?)
        .ok_or(MoError::Malformed("value table offset overflow"))?;

    let mut out = Vec::with_capacity(header_size + index_size + ids.len() + values.len());
    for word in [LE_MAGIC, 0, count, 28, 28 + count * 8, 0, 0] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for (id_offset, id_len, _, _) in &offsets {
        out.extend_from_slice(&id_len.to_le_bytes());
        out.extend_from_slice(&(*id_offset + key_start).to_le_bytes());
    }
    for (_, _, value_offset, value_len) in &offsets {
        out.extend_from_slice(&value_len.to_le_bytes());
        out.extend_from_slice(&(*value_offset + value_start).to_le_bytes());
    }
    out.extend_from_slice(&ids);
    out.extend_from_slice(&values);
    Ok(out)
}

/// Reads a binary MO catalog into a `msgid → msgstr` map.
///
/// Returns a structured [`MoError`] for every malformed input; never panics.
pub fn read_mo(bytes: &[u8]) -> Result<BTreeMap<String, String>, MoError> {
    if bytes.len() < 28 {
        return Err(MoError::Malformed("header shorter than 28 bytes"));
    }
    let native = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let (magic, little) = if native == LE_MAGIC {
        (LE_MAGIC, true)
    } else if native == BE_MAGIC {
        (BE_MAGIC, false)
    } else {
        return Err(MoError::InvalidMagic(native));
    };
    let _ = magic;
    let revision = read_u32(bytes, 4, little)?;
    if revision != 0 {
        return Err(MoError::UnsupportedRevision(revision));
    }
    let count = read_u32(bytes, 8, little)? as usize;
    let key_offset = read_u32(bytes, 12, little)? as usize;
    let value_offset = read_u32(bytes, 16, little)? as usize;

    // Each table entry is two u32s; check the whole table fits before reading.
    let table_bytes = count.checked_mul(8).ok_or(MoError::Malformed("count overflow"))?;
    key_offset
        .checked_add(table_bytes)
        .filter(|end| *end <= bytes.len())
        .ok_or(MoError::Malformed("key table outside file"))?;
    value_offset
        .checked_add(table_bytes)
        .filter(|end| *end <= bytes.len())
        .ok_or(MoError::Malformed("value table outside file"))?;

    let mut entries = BTreeMap::new();
    for index in 0..count {
        let key_len = read_u32(bytes, key_offset + index * 8, little)? as usize;
        let key_at = read_u32(bytes, key_offset + index * 8 + 4, little)? as usize;
        let value_len = read_u32(bytes, value_offset + index * 8, little)? as usize;
        let value_at = read_u32(bytes, value_offset + index * 8 + 4, little)? as usize;
        let key = slice_string(bytes, key_at, key_len)?;
        let value = slice_string(bytes, value_at, value_len)?;
        entries.insert(key, value);
    }
    Ok(entries)
}

fn slice_string(bytes: &[u8], offset: usize, length: usize) -> Result<String, MoError> {
    let end = offset.checked_add(length).ok_or(MoError::Malformed("string offset overflow"))?;
    let slice = bytes.get(offset..end).ok_or(MoError::Malformed("string outside file"))?;
    Ok(std::str::from_utf8(slice)?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trip() {
        let entries = vec![
            (String::new(), "Content-Type: text/plain; charset=UTF-8\n".to_owned()),
            ("Hello".to_owned(), "Hallo".to_owned()),
            ("one\0many".to_owned(), "ein\0viele".to_owned()),
        ];
        let bytes = write_mo(&entries).expect("writing a valid catalog cannot fail");
        assert_eq!(&bytes[0..4], &LE_MAGIC.to_le_bytes());
        let read = read_mo(&bytes).expect("the catalog we just wrote must be readable");
        assert_eq!(read.len(), 3);
        assert_eq!(read.get("Hello").map(String::as_str), Some("Hallo"));
        assert_eq!(read.get("one\0many").map(String::as_str), Some("ein\0viele"));
        assert_eq!(read.get("").map(String::as_str), Some("Content-Type: text/plain; charset=UTF-8\n"));
    }

    #[test]
    fn deterministic_bytes() {
        let entries = vec![("b".to_owned(), "B".to_owned()), ("a".to_owned(), "A".to_owned())];
        let first = write_mo(&entries).expect("valid");
        let second = write_mo(&entries).expect("valid");
        assert_eq!(first, second);
        let reversed = write_mo(&entries.into_iter().rev().collect::<Vec<_>>()).expect("valid");
        assert_eq!(first, reversed);
    }

    #[test]
    fn malformed_input_never_panics() {
        assert!(read_mo(&[]).is_err());
        assert!(read_mo(&[0u8; 28]).is_err());
        let mut bytes = write_mo(&[("a".into(), "A".into())]).expect("valid");
        bytes.truncate(20);
        assert!(read_mo(&bytes).is_err());
        bytes = write_mo(&[("a".into(), "A".into())]).expect("valid");
        bytes[0] = 0;
        assert!(read_mo(&bytes).is_err());
    }

    #[test]
    fn reads_babel_generated_catalog() {
        // Generated by Babel's write_mo in the oracle venv; checks that our
        // reader accepts the reference implementation's output.
        let bytes = include_bytes!("tests/fixtures/oracle.mo");
        let entries = read_mo(bytes).expect("oracle MO must be readable");
        assert_eq!(entries.get("Hello world.").map(String::as_str), Some("HELLO WORLD."));
    }
}
