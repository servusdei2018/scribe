//! Phase 69 fuzz target for native autodoc.
//!
//! Python source and docstrings arrive from third-party packages as untrusted
//! text. Parsing, dedenting and Napoleon conversion must never panic and must
//! be deterministic for identical input.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use std::path::Path;

use proptest::prelude::*;
use scribe_autodoc::{convert_docstring, dedent_docstring, parse_source, NapoleonOptions};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

fn python_text() -> impl Strategy<Value = String> {
    prop::string::string_regex(
        "[\\p{L}\\p{N}\\p{P}\\p{S} \\n\\t\\r\\\\':\"()\\[\\]{}=,.*]{0,500}",
    )
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn python_source_and_docstrings_never_panic(
        source in python_text(),
        docstring in python_text(),
        google in prop::bool::ANY,
        numpy in prop::bool::ANY,
        use_param in prop::bool::ANY,
        use_rtype in prop::bool::ANY,
        use_keyword in prop::bool::ANY,
        use_ivar in prop::bool::ANY,
        use_admonition_for_examples in prop::bool::ANY,
        use_admonition_for_notes in prop::bool::ANY,
    ) {
        let options = NapoleonOptions {
            google,
            numpy,
            use_param,
            use_rtype,
            use_keyword,
            use_ivar,
            use_admonition_for_examples,
            use_admonition_for_notes,
        };

        let dedented = dedent_docstring(&docstring);
        prop_assert_eq!(dedented, dedent_docstring(&docstring), "dedent is not deterministic");

        let converted = convert_docstring(&docstring, &options);
        prop_assert_eq!(&converted, &convert_docstring(&docstring, &options), "napoleon is not deterministic");
        prop_assert!(converted.len() <= docstring.len() * 16 + 512, "napoleon output is not bounded");

        let first = parse_source(&source, "fuzz.module", Path::new("fuzz_module.py"));
        let second = parse_source(&source, "fuzz.module", Path::new("fuzz_module.py"));
        match (first, second) {
            (Ok(a), Ok(b)) => {
                prop_assert_eq!(&a.source_hash, &b.source_hash, "source hash is not deterministic");
                prop_assert_eq!(a.members.len(), b.members.len(), "member count is not deterministic");
            }
            (Err(_), Err(_)) => {}
            _ => prop_assert!(false, "parse_source result was not deterministic"),
        }
    }
}
