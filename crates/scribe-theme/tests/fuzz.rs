//! Phase 69 fuzz target for the theme helper layer.
//!
//! `relative_uri` and `urlquote` are fed arbitrary docnames and URIs from the
//! parsed project. They must never panic and must be deterministic and bounded
//! for hostile (including multi-byte and path-traversal) inputs.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use proptest::prelude::*;
use scribe_theme::{relative_uri, urlquote};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

fn uri_text() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Za-z0-9._/#?&%=:\\-\\p{L} ]{0,80}").unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn relative_uri_never_panics_and_is_deterministic(
        base in uri_text(),
        target in uri_text(),
    ) {
        let result = relative_uri(&base, &target);
        prop_assert_eq!(&result, &relative_uri(&base, &target), "relative_uri is not deterministic");
        prop_assert!(
            result.len() <= base.len() + target.len() + 8,
            "relative_uri grew beyond its inputs"
        );
    }

    #[test]
    fn urlquote_never_panics_and_is_deterministic(value in uri_text()) {
        let quoted = urlquote(&value);
        prop_assert_eq!(&quoted, &urlquote(&value), "urlquote is not deterministic");
        prop_assert!(quoted.len() <= value.len() * 12 + 4, "urlquote output is not bounded");
    }
}
