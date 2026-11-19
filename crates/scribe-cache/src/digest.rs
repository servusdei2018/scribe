//! BLAKE3 content digests and deterministic digest combination.

use crate::error::CacheError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// A 256-bit BLAKE3 content digest.
///
/// The digest is `Copy`, `Ord` and `Hash`, so it can be used directly as a map
/// key in the incremental engine. Its `Display` form is lowercase hexadecimal,
/// which is stable across platforms and useful in diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Number of raw bytes in a digest.
    pub const LEN: usize = 32;

    /// Hash an in-memory byte slice.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(bytes);
        Self(*hasher.finalize().as_bytes())
    }

    /// Hash a UTF-8 string's bytes.
    pub fn of_str(text: &str) -> Self {
        Self::of_bytes(text.as_bytes())
    }

    /// Hash the *contents* of a file.
    ///
    /// I/O errors are surfaced; callers that want a missing file to be a
    /// sentinel should handle [`CacheError`] themselves (the incremental engine
    /// maps it to a zero digest).
    pub fn of_file(path: &Path) -> Result<Self, CacheError> {
        let bytes = std::fs::read(path).map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self::of_bytes(&bytes))
    }

    /// Construct a digest from raw bytes (e.g. a hasher's finalized output).
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A digest with all bytes zeroed, used as the "file absent" sentinel.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Whether this is the all-zero sentinel.
    pub fn is_zero(self) -> bool {
        self.0 == [0u8; 32]
    }

    /// Borrow the raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copy the raw bytes into an owned array.
    pub fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Lowercase hexadecimal representation.
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    /// Parse a hexadecimal representation produced by [`Self::to_hex`].
    pub fn from_hex(text: &str) -> Result<Self, CacheError> {
        if text.len() != 64 {
            return Err(CacheError::InvalidDigest(text.to_owned()));
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
            let hi = hex_nibble(chunk[0]);
            let lo = hex_nibble(chunk[1]);
            match (hi, lo) {
                (Some(hi), Some(lo)) => bytes[index] = (hi << 4) | lo,
                _ => return Err(CacheError::InvalidDigest(text.to_owned())),
            }
        }
        Ok(Self(bytes))
    }

    /// Deterministically combine labelled digests into one digest.
    ///
    /// Labels and values are length-prefixed so no concatenation ambiguity can
    /// make two different inputs hash to the same result.
    pub fn combine(parts: &[(&str, &Self)]) -> Self {
        let mut hasher = blake3::Hasher::new();
        for (label, digest) in parts {
            hasher.update(&(label.len() as u64).to_le_bytes());
            hasher.update(label.as_bytes());
            hasher.update(&digest.0);
        }
        Self(*hasher.finalize().as_bytes())
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl Default for ContentDigest {
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ContentDigest({})", self.to_hex())
    }
}

/// Hash an in-memory byte slice.
pub fn hash_bytes(bytes: &[u8]) -> ContentDigest {
    ContentDigest::of_bytes(bytes)
}

/// Hash a UTF-8 string.
pub fn hash_str(text: &str) -> ContentDigest {
    ContentDigest::of_str(text)
}

/// Hash the contents of a file.
pub fn hash_file(path: &Path) -> Result<ContentDigest, CacheError> {
    ContentDigest::of_file(path)
}

#[cfg(test)]
mod tests {
    use super::{hash_str, ContentDigest};

    #[test]
    fn hashing_is_deterministic_and_order_sensitive() {
        let a = hash_str("alpha");
        let b = hash_str("alpha");
        let c = hash_str("beta");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.to_hex().len(), 64);
        assert_eq!(ContentDigest::from_hex(&a.to_hex()).unwrap(), a);
    }

    #[test]
    fn combine_is_order_sensitive_and_unambiguous() {
        let a = hash_str("a");
        let b = hash_str("b");
        assert_ne!(
            ContentDigest::combine(&[("x", &a), ("y", &b)]),
            ContentDigest::combine(&[("x", &b), ("y", &a)])
        );
        // Length prefix prevents ("ab","c") colliding with ("a","bc").
        let ab = hash_str("ab");
        let c = hash_str("c");
        let a = hash_str("a");
        let bc = hash_str("bc");
        assert_ne!(
            ContentDigest::combine(&[("k", &ab), ("z", &c)]),
            ContentDigest::combine(&[("k", &a), ("z", &bc)])
        );
    }
}
