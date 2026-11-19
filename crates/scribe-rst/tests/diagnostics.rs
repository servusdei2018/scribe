//! Recovery tests: malformed input must yield miette diagnostics with source
//! spans and a best-effort document, never a panic.

mod common;

use scribe_rst::{named_report, parse, RstDiagnostic, RstError, Severity};

fn diagnostics_for(source: &str) -> Vec<RstDiagnostic> {
    parse(source).diagnostics
}

#[test]
fn inconsistent_indentation_is_diagnosed_with_help_and_span() {
    let source = "    first line of quote\n  dedented continuation\n";
    let diagnostics = diagnostics_for(source);
    let diag = diagnostics
        .iter()
        .find(|d| d.code() == "scribe::rst::indentation")
        .expect("indentation diagnostic");
    assert!(diag.to_string().contains("inconsistent indentation"), "{diag}");
    assert!(diag.to_string().contains("column 4"), "{diag}");
    assert_eq!(diag.severity(), Severity::Error);
    // Source line 2 starts at byte 23.
    let span = diag.span().expect("span");
    assert_eq!(span.offset(), 24);
    assert!(!span.is_empty());
    let report = named_report(Some("bad.rst".into()), source.into(), RstError::Indentation {
        message: "inconsistent indentation".into(),
        span,
    });
    let rendered = format!("{report:?}");
    assert!(rendered.contains("bad.rst"), "{rendered}");
    assert!(rendered.contains("check indentation"), "{rendered}");
    assert!(rendered.contains("bad.rst:2:"), "{rendered}"); // labelled at line 2
}

#[test]
fn still_returns_a_best_effort_document() {
    let output = parse("Title\n=====\n\n    quote\n  bad\n\nAfter.\n");
    assert!(output.has_diagnostics());
    let pseudo = output.document.to_pseudoxml().expect("emission");
    assert!(pseudo.contains("<section"), "{pseudo}");
    assert!(pseudo.contains("quote"), "{pseudo}");
    assert!(pseudo.contains("After."), "{pseudo}");
}

#[test]
fn malformed_table_is_diagnosed() {
    let source = "+---+---+\n| a | b |\n+---+--+\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == "scribe::rst::table"),
        "{diagnostics:?}"
    );
    let diag = diagnostics
        .iter()
        .find(|d| d.code() == "scribe::rst::table")
        .unwrap();
    assert!(diag.to_string().contains("malformed table"), "{diag}");
    assert!(diag.to_string().contains("border"), "{diag}");
}

#[test]
fn bad_adornment_is_diagnosed() {
    let source = "A very long title that needs a longer underline\n---\n\nText.\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == "scribe::rst::adornment"),
        "{diagnostics:?}"
    );
    let diag = diagnostics
        .iter()
        .find(|d| d.code() == "scribe::rst::adornment")
        .unwrap();
    assert!(diag.to_string().contains("too short"), "{diag}");
}

#[test]
fn stray_adornment_line_is_diagnosed_but_recovered() {
    let source = "Para one.\n\n-----\n\nPara two.\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.code().ends_with("panic")),
        "{diagnostics:?}"
    );
    let output = parse(source);
    let pseudo = output.document.to_pseudoxml().unwrap();
    assert!(pseudo.contains("<transition>"), "{pseudo}");
}

#[test]
fn missing_literal_body_is_diagnosed() {
    let source = "Paragraph ending with::\n\nNot indented.\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == "scribe::rst::missing_body"),
        "{diagnostics:?}"
    );
}

#[test]
fn unterminated_substitution_is_diagnosed() {
    let source = ".. |missing replace:: text\n";
    let diagnostics = diagnostics_for(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == "scribe::rst::explicit_markup"),
        "{diagnostics:?}"
    );
}

#[test]
fn diagnostics_carry_stable_codes_and_help() {
    let source = "+---+\n| a |\n+--+\n";
    for diag in diagnostics_for(source) {
        assert!(diag.code().starts_with("scribe::rst::"), "{diag}");
        assert!(diag.span().is_some(), "{diag}");
        assert!(!diag.to_string().is_empty());
    }
}

#[test]
fn recovery_never_panics_on_fuzz_like_inputs() {
    let inputs = [
        ".. \n",
        "::\n",
        "----\n",
        "1. \n",
        ":a:\n",
        "| \n",
        ">>> \n",
        "+---+\n",
        "==== ===\n",
        "Title\n==\n",
        "    \n",
        ".. _:\n",
        ".. |a| replace::\n",
        "term :\n    def\n",
    ];
    for input in inputs {
        let output = parse(input);
        let _ = output.document.to_pseudoxml().expect("emission");
        for diag in &output.diagnostics {
            let _ = diag.clone().to_report();
        }
    }
}
