//! Phase 69 fuzz target for the PO/MO internationalisation pipeline.
//!
//! The MO reader consumes untrusted bytes (a fetched catalog can be corrupt or
//! hostile), so arbitrary byte strings must never panic and a successful parse
//! must stay bounded by the input size. The writer/reader pair must round-trip
//! deterministically, and PO parsing of arbitrary text must be panic-free with
//! a stable re-emission.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use std::collections::BTreeMap;

use proptest::prelude::*;
use scribe_builders::{read_mo, write_mo, PoCatalog};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

/// Strings used as msgid/msgstr: printable ASCII plus multi-byte Unicode and
/// control characters, all of which the MO writer must handle without panics.
fn mo_string() -> impl Strategy<Value = String> {
    prop::string::string_regex("[\\p{L}\\p{N}\\p{P}\\p{S} \\n\\t\\r\\\\]{0,40}").unwrap()
}

fn mo_entries() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((mo_string(), mo_string()), 0..24)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn read_mo_arbitrary_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let first = read_mo(&bytes);
        let second = read_mo(&bytes);
        prop_assert_eq!(
            first.as_ref().ok(),
            second.as_ref().ok(),
            "MO parse is not deterministic"
        );
        if let Ok(map) = first {
            prop_assert!(map.len() <= bytes.len(), "MO item count exceeded input byte length");
        }
    }

    #[test]
    fn mo_roundtrip_is_deterministic(entries in mo_entries()) {
        let bytes = write_mo(&entries).unwrap();
        let decoded = read_mo(&bytes).unwrap();

        // `write_mo` sorts by msgid (stable), and `read_mo` inserts into a
        // BTreeMap, so the last value for a duplicated msgid wins. Building the
        // expectation in original insertion order mirrors that exactly.
        let mut expected = BTreeMap::new();
        for (id, value) in &entries {
            expected.insert(id.clone(), value.clone());
        }
        prop_assert_eq!(decoded, expected, "MO round-trip changed the catalog");

        let again = write_mo(&entries).unwrap();
        prop_assert_eq!(bytes, again, "MO writer is not deterministic");
    }

    #[test]
    fn po_parse_arbitrary_text_never_panics(
        text in prop::string::string_regex("[\\p{L}\\p{N}\\p{P}\\p{S} \\n\\t\"\\\\#:]{0,600}").unwrap(),
    ) {
        let catalog = PoCatalog::parse(&text);
        let first = catalog.to_po();
        let second = catalog.to_po();
        prop_assert_eq!(&first, &second, "PO re-emission is not deterministic");
        prop_assert!(first.len() <= text.len() * 8 + 512, "PO output is not linearly bounded");
    }
}
