//! Phase 69 fuzz target for the deterministic incremental cache.
//!
//! Two invariants are asserted for every generated case:
//!   * a `save` → `load` round-trip is byte-for-byte deterministic (equal
//!     values produce equal content digests) and never panics for arbitrary
//!     logical keys / payloads;
//!   * any single-byte corruption of a previously valid artifact is treated
//!     as a cold cache (`load` returns `None`) and never panics — a build must
//!     never abort because an on-disk artifact was damaged.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use scribe_cache::CacheStore;

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

/// Logical keys are restricted to the sanitised alphabet so `cache_filename`
/// accepts them; arbitrary Unicode keys are exercised by the unit tests.
fn key() -> impl Strategy<Value = String> {
    // Starting with an alphanumeric excludes the reserved `"."` / `".."` keys
    // that `cache_filename` rejects.
    "[A-Za-z0-9][A-Za-z0-9._-]{0,23}"
}

fn payload() -> impl Strategy<Value = Vec<(String, u64)>> {
    prop::collection::vec(("[A-Za-z0-9_]{1,16}", any::<u64>()), 0..24)
}

static STORE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn store_for() -> CacheStore<Vec<(String, u64)>> {
    // A globally unique root per store keeps the two parallel property tests
    // (and regenerated cases) from ever sharing an artifact path.
    let sequence = STORE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "scribe-cache-fuzz-{}-{sequence}-store",
        std::process::id()
    ));
    CacheStore::new(root)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn save_load_roundtrip_is_deterministic(
        case_id in any::<u64>(),
        logical_key in key(),
        value in payload(),
    ) {
        let _ = case_id;
        let store = store_for();
        // A persisted historical seed used the now-excluded key `"."`; skip it
        // gracefully so replaying the seed never panics.
        if store.path_for(&logical_key).is_err() {
            return Ok(());
        }
        let first = store.save(&logical_key, &value).unwrap();
        prop_assert!(!first.is_zero(), "digest must be non-zero for a saved artifact");
        prop_assert_eq!(store.load(&logical_key).unwrap(), Some(value.clone()));

        // Saving the same value under a different key must produce identical
        // bytes and an identical digest, regardless of the logical key.
        let other_key = format!("{logical_key}_copy");
        let second = store.save(&other_key, &value).unwrap();
        prop_assert_eq!(first, second, "digest is not content-addressed");
        let a = std::fs::read(store.path_for(&logical_key).unwrap()).unwrap();
        let b = std::fs::read(store.path_for(&other_key).unwrap()).unwrap();
        prop_assert_eq!(a, b, "artifact bytes differ for identical values");

        store.clear().unwrap();
        prop_assert!(!store.exists(&logical_key));
    }

    #[test]
    fn single_byte_corruption_is_always_cold(
        case_id in any::<u64>(),
        logical_key in key(),
        value in prop::collection::vec(("[A-Za-z0-9_]{1,16}", any::<u64>()), 1..24),
        flip in any::<usize>(),
    ) {
        let _ = case_id;
        let store = store_for();
        // A persisted historical seed used the now-excluded key `"."`; skip it
        // gracefully so replaying the seed never panics.
        if store.path_for(&logical_key).is_err() {
            return Ok(());
        }
        store.save(&logical_key, &value).unwrap();
        let path = store.path_for(&logical_key).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        prop_assert!(bytes.len() > 48, "artifact unexpectedly short");

        // Flip one byte anywhere in magic / version / length / digest / payload.
        let index = flip % bytes.len();
        bytes[index] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();

        prop_assert!(store.load(&logical_key).unwrap().is_none(), "corrupt artifact was not cold");
        prop_assert!(store.load_checked(&logical_key).is_err(), "corrupt artifact passed verification");
        store.clear().unwrap();
    }
}
