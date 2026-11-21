//! A versioned, content-verified, atomically written typed cache store.

use crate::digest::ContentDigest;
use crate::error::CacheError;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

/// Magic number at the start of every cache file.
pub const CACHE_MAGIC: [u8; 4] = *b"SCRB";
/// Current cache envelope version. Bump when the on-disk layout changes.
pub const CACHE_FORMAT_VERSION: u32 = 2;
const HEADER_LEN: usize = 4 + 4 + 8 + 32;

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);
static ENCODE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// A typed cache store rooted at a directory.
///
/// Values are bincode-encoded and wrapped in a small fixed header
/// (`magic | version | payload length | BLAKE3 digest | payload`). The header
/// is written with little-endian integers on every platform, so the bytes are
/// deterministic. Writes go through a temporary file and an atomic rename.
///
/// Decoding is intentionally forgiving: [`Self::load`] returns `Ok(None)` for a
/// missing, truncated, version-mismatched, checksum-failing or otherwise
/// corrupt artifact. A corrupt cache is simply a cold cache.
#[derive(Clone, Debug)]
pub struct CacheStore<T> {
    root: PathBuf,
    _marker: std::marker::PhantomData<T>,
}

impl<T> CacheStore<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Open a store rooted at `root`. The directory is created lazily on save.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Directory backing this store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path used for a logical cache key.
    pub fn path_for(&self, key: &str) -> Result<PathBuf, CacheError> {
        Ok(self.root.join(cache_filename(key)?))
    }

    /// Whether an artifact exists for `key`.
    pub fn exists(&self, key: &str) -> bool {
        self.path_for(key).map(|path| path.is_file()).unwrap_or(false)
    }

    /// Encode and atomically store `value`, returning its content digest.
    pub fn save(&self, key: &str, value: &T) -> Result<ContentDigest, CacheError> {
        let filename = cache_filename(key)?;
        let payload = encode(value)?;
        let digest = ContentDigest::of_bytes(&payload);

        // Stream the header and payload straight to the file instead of
        // building a second `HEADER_LEN + payload.len()` buffer. The bytes on
        // disk are identical, but the transient allocation is halved — which
        // matters when persisting the full environment (all doctrees) at the
        // end of a large build.
        std::fs::create_dir_all(&self.root).map_err(|source| CacheError::CreateDir {
            path: self.root.clone(),
            source,
        })?;

        let target = self.root.join(filename);
        let sequence = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = self.root.join(format!(
            ".tmp-{}-{sequence}-{}",
            std::process::id(),
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("artifact")
        ));

        {
            let mut file = File::create(&temporary).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(&CACHE_MAGIC).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(&CACHE_FORMAT_VERSION.to_le_bytes()).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(&(payload.len() as u64).to_le_bytes()).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(digest.as_bytes()).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(&payload).map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| CacheError::Io {
                path: temporary.clone(),
                source,
            })?;
        }

        if std::fs::rename(&temporary, &target).is_err() {
            // On platforms where rename cannot replace an existing file, drop
            // the destination first and retry. Never leave the temp file behind.
            let _ = std::fs::remove_file(&target);
            std::fs::rename(&temporary, &target).map_err(|source| CacheError::Io {
                path: target.clone(),
                source,
            })?;
        }

        Ok(digest)
    }

    /// Load and verify an artifact.
    ///
    /// Returns `Ok(None)` for *any* unusable artifact (missing, truncated,
    /// incompatible version, bad checksum or undecodable payload). Genuine
    /// filesystem errors while reading an existing file are also treated as a
    /// cold cache, because a build must never abort on a bad cache.
    pub fn load(&self, key: &str) -> Result<Option<T>, CacheError> {
        match self.load_checked(key) {
            Ok(value) => Ok(Some(value)),
            Err(_) => Ok(None),
        }
    }

    /// Like [`Self::load`], but reports why an artifact could not be used.
    ///
    /// Used by diagnostics and by tests that deliberately corrupt the cache.
    pub fn load_checked(&self, key: &str) -> Result<T, CacheError> {
        let path = self.root.join(cache_filename(key)?);
        let mut blob = Vec::new();
        File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut blob))
            .map_err(|source| CacheError::Io {
                path: path.clone(),
                source,
            })?;

        if blob.len() < HEADER_LEN {
            return Err(CacheError::Encode("cache artifact is truncated".to_owned()));
        }
        if blob[..4] != CACHE_MAGIC {
            return Err(CacheError::Encode("cache artifact has a bad magic".to_owned()));
        }
        let version = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
        if version != CACHE_FORMAT_VERSION {
            return Err(CacheError::Encode(format!(
                "cache artifact version {version} is incompatible with {CACHE_FORMAT_VERSION}"
            )));
        }
        let payload_len = u64::from_le_bytes([
            blob[8], blob[9], blob[10], blob[11], blob[12], blob[13], blob[14], blob[15],
        ]) as usize;
        let expected_len = HEADER_LEN.saturating_add(payload_len);
        if payload_len != blob.len().saturating_sub(HEADER_LEN) || expected_len != blob.len() {
            return Err(CacheError::Encode(
                "cache artifact length does not match its header".to_owned(),
            ));
        }
        let mut stored_digest = [0u8; 32];
        stored_digest.copy_from_slice(&blob[16..HEADER_LEN]);
        let payload = &blob[HEADER_LEN..];
        if ContentDigest::of_bytes(payload).into_bytes() != stored_digest {
            return Err(CacheError::Encode(
                "cache artifact failed its content checksum".to_owned(),
            ));
        }

        bincode::deserialize(payload)
            .map_err(|error| CacheError::Encode(format!("cache payload is undecodable: {error}")))
    }

    /// Remove one artifact. A missing artifact is not an error.
    pub fn remove(&self, key: &str) -> Result<(), CacheError> {
        let path = self.root.join(cache_filename(key)?);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CacheError::Io { path, source }),
        }
    }

    /// Remove every artifact in the store directory.
    pub fn clear(&self) -> Result<(), CacheError> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CacheError::Io {
                path: self.root.clone(),
                source,
            }),
        }
    }

}

fn cache_filename(key: &str) -> Result<String, CacheError> {
    let sanitized: String = key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(CacheError::InvalidKey(key.to_owned()));
    }
    Ok(format!("cache-{sanitized}.bin"))
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CacheError> {
    // Serialization is global-locked only to make its deterministic byte
    // output independent of any concurrent callers that share nested state.
    let _guard = ENCODE_LOCK.lock();
    bincode::serialize(value).map_err(|error| CacheError::Encode(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::CacheStore;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        entries: BTreeMap<String, u64>,
    }

    fn sample() -> Sample {
        let mut entries = BTreeMap::new();
        entries.insert("b".to_owned(), 2);
        entries.insert("a".to_owned(), 1);
        Sample {
            name: "demo".to_owned(),
            entries,
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("scribe-cache-test-{tag}-{}", std::process::id()))
    }

    #[test]
    fn save_load_round_trip() {
        let root = temp_root("roundtrip");
        let store = CacheStore::<Sample>::new(&root);
        let digest = store.save("environment", &sample()).unwrap();
        assert!(!digest.is_zero());
        assert_eq!(store.load("environment").unwrap(), Some(sample()));
        store.clear().unwrap();
    }

    #[test]
    fn bytes_are_deterministic_across_saves() {
        let root = temp_root("deterministic");
        let store = CacheStore::<Sample>::new(&root);
        let first = store.save("a", &sample()).unwrap();
        let second = store.save("b", &sample()).unwrap();
        assert_eq!(first, second);
        let a = std::fs::read(store.path_for("a").unwrap()).unwrap();
        let b = std::fs::read(store.path_for("b").unwrap()).unwrap();
        assert_eq!(a, b);
        store.clear().unwrap();
    }

    #[test]
    fn corrupt_and_missing_artifacts_are_cold() {
        let root = temp_root("corrupt");
        let store = CacheStore::<Sample>::new(&root);
        assert_eq!(store.load("missing").unwrap(), None);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(store.path_for("garbage").unwrap(), b"not a cache").unwrap();
        assert_eq!(store.load("garbage").unwrap(), None);
        assert!(store.load_checked("garbage").is_err());

        store.save("good", &sample()).unwrap();
        let path = store.path_for("good").unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(store.load("good").unwrap(), None);
        store.clear().unwrap();
    }
}
