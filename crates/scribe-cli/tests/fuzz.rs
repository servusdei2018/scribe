//! Phase 69 fuzz target for the CLI option model.
//!
//! `-D name=value` overrides come straight from the command line. Parsing an
//! arbitrary string must never panic, must be deterministic and — on success —
//! must split at exactly the first `=` into a non-empty key.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use proptest::prelude::*;
use scribe_cli::ConfigOverride;

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn config_override_parse_never_panics_and_is_deterministic(
        raw in prop::string::string_regex("[\\p{L}\\p{N}\\p{P}\\p{S} =\\n\\t\\\\]{0,120}").unwrap(),
    ) {
        let first = ConfigOverride::parse(&raw);
        let second = ConfigOverride::parse(&raw);
        match (first, second) {
            (Ok(a), Ok(b)) => {
                prop_assert_eq!(&a.key, &b.key);
                prop_assert_eq!(&a.value, &b.value);
                prop_assert!(!a.key.is_empty(), "empty key accepted");
                // The key must be exactly the part before the first '='.
                prop_assert_eq!(&a.key, raw.split_once('=').unwrap().0);
                let joined = format!("{}={}", a.key, a.value);
                prop_assert_eq!(joined, raw, "override did not round-trip");
            }
            (Err(_), Err(_)) => {}
            _ => prop_assert!(false, "override parse was not deterministic"),
        }
    }
}
