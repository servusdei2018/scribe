//! BLAKE3-keyed on-disk cache for downloaded inventories.
//!
//! The cache stores a small deterministic envelope:
//!
//! ```text
//! [magic "SCRI"][version u32 LE][timestamp i64 LE][name length u32 LE][name][raw objects.inv]
//! ```
//!
//! Cache files live under a caller-supplied directory (upstream uses
//! `<doctreedir>/__intersphinx_cache__`).  A blake3 digest of the inventory
//! key (the project target URI) names the file, so lookups are stable across
//! builds.  Expiry mirrors upstream `_fetch_inventory_group`: with a
//! non-negative `cache_limit` (days) an entry is fresh while
//! `timestamp >= now - limit * 86400`; a negative limit never expires.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{IntersphinxError, IntersphinxResult};

const MAGIC: &[u8; 4] = b"SCRI";
const VERSION: u32 = 1;

/// One decoded cache entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEntry {
    /// Project name recorded when the entry was stored.
    pub name: String,
    /// UNIX timestamp (seconds) of the fetch.
    pub timestamp: i64,
    /// Raw `objects.inv` bytes.
    pub bytes: Vec<u8>,
}

/// A directory-backed inventory cache.
#[derive(Clone, Debug)]
pub struct InventoryCache {
    root: PathBuf,
    cache_limit_days: i64,
}

impl InventoryCache {
    /// Creates a cache rooted at `root` with the given lifetime in days.
    pub fn new(root: PathBuf, cache_limit_days: i64) -> Self {
        Self { root, cache_limit_days }
    }

    /// Cache directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Configured lifetime in days.
    pub fn cache_limit_days(&self) -> i64 {
        self.cache_limit_days
    }

    /// Deterministic path for a cache key.
    pub fn path_for(&self, key: &str) -> PathBuf {
        let digest = blake3::hash(key.as_bytes()).to_hex();
        self.root.join(format!("{digest}.inv"))
    }

    /// Reads a fresh entry for `key`, or `None` on miss / expiry / corruption.
    pub fn get(&self, key: &str, now: i64) -> Option<CacheEntry> {
        let entry = self.get_stale(key)?;
        Self::is_fresh(entry.timestamp, now, self.cache_limit_days).then_some(entry)
    }

    /// Reads a cached entry ignoring its TTL (offline fallback).
    ///
    /// Corrupt or unreadable entries are still treated as a miss.  Callers use
    /// this only after a fresh fetch failed, so a stale inventory is always
    /// better than no inventory at all.
    pub fn get_stale(&self, key: &str) -> Option<CacheEntry> {
        let path = self.path_for(key);
        let bytes = fs::read(&path).ok()?;
        decode(&bytes).ok()
    }

    /// Atomically stores an entry.
    pub fn put(&self, key: &str, entry: &CacheEntry) -> IntersphinxResult<()> {
        fs::create_dir_all(&self.root).map_err(|error| IntersphinxError::Io {
            path: self.root.display().to_string(),
            detail: error.to_string(),
        })?;
        let target = self.path_for(key);
        let bytes = encode(entry)?;
        let temporary = target.with_extension("inv.tmp");
        {
            let mut file = fs::File::create(&temporary).map_err(|error| IntersphinxError::Io {
                path: temporary.display().to_string(),
                detail: error.to_string(),
            })?;
            file.write_all(&bytes).map_err(|error| IntersphinxError::Io {
                path: temporary.display().to_string(),
                detail: error.to_string(),
            })?;
            file.sync_all().map_err(|error| IntersphinxError::Io {
                path: temporary.display().to_string(),
                detail: error.to_string(),
            })?;
        }
        fs::rename(&temporary, &target).map_err(|error| IntersphinxError::Io {
            path: target.display().to_string(),
            detail: error.to_string(),
        })?;
        Ok(())
    }

    /// Removes expired entries, returning how many were deleted.
    pub fn purge_stale(&self, now: i64) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "inv") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(decoded) = decode(&bytes) else {
                continue;
            };
            if !Self::is_fresh(decoded.timestamp, now, self.cache_limit_days)
                && fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }

    /// Upstream cache-expiry predicate.
    pub fn is_fresh(timestamp: i64, now: i64, cache_limit_days: i64) -> bool {
        if cache_limit_days >= 0 {
            let lifetime = cache_limit_days.saturating_mul(86_400);
            timestamp >= now.saturating_sub(lifetime)
        } else {
            true
        }
    }

    /// Current UNIX time in seconds (saturating for pre-epoch clocks).
    pub fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    }
}

fn encode(entry: &CacheEntry) -> IntersphinxResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(4 + 4 + 8 + 4 + entry.name.len() + entry.bytes.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&entry.timestamp.to_le_bytes());
    let name_len = u32::try_from(entry.name.len()).map_err(|_| IntersphinxError::InvalidEntry(
        "cache entry name is too long".to_owned(),
    ))?;
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(entry.name.as_bytes());
    bytes.extend_from_slice(&entry.bytes);
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> IntersphinxResult<CacheEntry> {
    let mut offset = 0;
    let take = |offset: &mut usize, len: usize| -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        let slice = bytes.get(*offset..end)?;
        *offset = end;
        Some(slice)
    };
    if take(&mut offset, 4).ok_or_else(|| IntersphinxError::InvalidEntry("short cache magic".into()))?
        != MAGIC
    {
        return Err(IntersphinxError::InvalidEntry("bad cache magic".into()));
    }
    let version = read_u32(take(&mut offset, 4).ok_or_else(|| IntersphinxError::InvalidEntry("short cache version".into()))?);
    if version != VERSION {
        return Err(IntersphinxError::InvalidEntry(format!(
            "unsupported cache version {version}"
        )));
    }
    let timestamp = i64::from_le_bytes(
        take(&mut offset, 8)
            .ok_or_else(|| IntersphinxError::InvalidEntry("short cache timestamp".into()))?
            .try_into()
            .map_err(|_| IntersphinxError::InvalidEntry("bad cache timestamp".into()))?,
    );
    let name_len = read_u32(
        take(&mut offset, 4).ok_or_else(|| IntersphinxError::InvalidEntry("short name length".into()))?,
    ) as usize;
    let name_bytes = take(&mut offset, name_len)
        .ok_or_else(|| IntersphinxError::InvalidEntry("short cache name".into()))?;
    let name = String::from_utf8_lossy(name_bytes).into_owned();
    let payload = bytes.get(offset..).unwrap_or_default().to_vec();
    Ok(CacheEntry { name, timestamp, bytes: payload })
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap_or([0, 0, 0, 0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_and_expiry() {
        let root = std::env::temp_dir().join(format!("scribe-inv-cache-test-{}", std::process::id()));
        let cache = InventoryCache::new(root.clone(), 5);
        let entry = CacheEntry { name: "python".into(), timestamp: 1000, bytes: b"inv".to_vec() };
        cache.put("https://docs.python.org/3/", &entry).expect("write");
        let loaded = cache.get("https://docs.python.org/3/", 1000 + 4 * 86_400).expect("fresh");
        assert_eq!(loaded, entry);
        assert!(cache.get("https://docs.python.org/3/", 1000 + 6 * 86_400).is_none());
        // A negative limit never expires.
        let forever = InventoryCache::new(root.clone(), -1);
        assert!(forever.get("https://docs.python.org/3/", i64::MAX / 2).is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_cache_is_a_miss() {
        let root = std::env::temp_dir().join(format!("scribe-inv-cache-corrupt-{}", std::process::id()));
        let cache = InventoryCache::new(root.clone(), 5);
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(cache.path_for("bad"), b"not a cache").expect("write");
        assert!(cache.get("bad", 1000).is_none());
        let _ = fs::remove_dir_all(root);
    }
}
