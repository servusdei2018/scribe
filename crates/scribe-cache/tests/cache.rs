//! Integration tests for the deterministic typed cache store.

use scribe_cache::{hash_bytes, hash_file, hash_str, CacheError, CacheStore, ContentDigest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "scribe-cache-it-{tag}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Payload {
    title: String,
    entries: BTreeMap<String, u64>,
    digest: ContentDigest,
}

fn payload() -> Payload {
    Payload {
        title: "cache fixture".to_owned(),
        entries: BTreeMap::from([("alpha".to_owned(), 1), ("beta".to_owned(), 2)]),
        digest: hash_str("content"),
    }
}

#[test]
fn typed_round_trip_and_deterministic_bytes() {
    let dir = TempDir::new("roundtrip");
    let store = CacheStore::<Payload>::new(dir.join("store"));
    let first = store.save("doctree__index", &payload()).expect("save");
    assert!(!first.is_zero());
    let again = store.save("doctree__index_again", &payload()).expect("save again");
    assert_eq!(first, again);
    assert_eq!(store.load("doctree__index").expect("load"), Some(payload()));
    assert_eq!(
        std::fs::read(store.path_for("doctree__index").unwrap()).unwrap(),
        std::fs::read(store.path_for("doctree__index_again").unwrap()).unwrap()
    );
}

#[test]
fn missing_corrupt_and_version_mismatched_artifacts_are_cold() {
    let dir = TempDir::new("cold");
    let store = CacheStore::<Payload>::new(dir.join("store"));
    assert_eq!(store.load("absent").expect("absent"), None);

    std::fs::create_dir_all(dir.join("store")).unwrap();
    std::fs::write(store.path_for("garbage").unwrap(), b"\x00\x01\x02").unwrap();
    assert_eq!(store.load("garbage").expect("garbage"), None);
    assert!(matches!(
        store.load_checked("garbage"),
        Err(CacheError::Encode(_))
    ));

    store.save("good", &payload()).expect("save good");
    let path = store.path_for("good").unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // Version field is bytes 4..8; make it incompatible.
    bytes[4] = 0xff;
    bytes[5] = 0xff;
    bytes[6] = 0xff;
    bytes[7] = 0xff;
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(store.load("good").expect("version mismatch"), None);
}

#[test]
fn content_hashes_are_stable_and_file_aware() {
    let dir = TempDir::new("digest");
    let path = dir.join("source.rst");
    std::fs::write(&path, "Hello, world!\n").unwrap();
    let digest = hash_file(&path).expect("hash file");
    assert_eq!(digest, hash_bytes(b"Hello, world!\n"));
    assert_eq!(digest, hash_str("Hello, world!\n"));
    assert_eq!(digest.to_hex(), ContentDigest::from_hex(&digest.to_hex()).unwrap().to_hex());
    assert!(digest > ContentDigest::ZERO);
    std::fs::write(&path, "changed\n").unwrap();
    assert_ne!(hash_file(&path).unwrap(), digest);
}

#[test]
fn keys_are_sanitized_and_remove_works() {
    let dir = TempDir::new("keys");
    let store = CacheStore::<Payload>::new(dir.join("store"));
    store.save("nested/doctree index", &payload()).expect("save");
    assert!(store.exists("nested/doctree index"));
    store.remove("nested/doctree index").expect("remove");
    assert!(!store.exists("nested/doctree index"));
    assert!(matches!(
        store.save("", &payload()),
        Err(CacheError::InvalidKey(_))
    ));
}
