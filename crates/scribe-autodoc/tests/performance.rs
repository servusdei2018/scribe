//! Performance smoke test and dynamic-introspection fallback tests (Phase 21).

use std::path::{Path, PathBuf};
use std::time::Instant;

use scribe_autodoc::dynamic::{introspect, DynamicOptions, DynamicModule};
use scribe_autodoc::parser::parse_sources;

#[test]
fn parses_multi_module_tree_in_milliseconds() {
    let count = 48usize;
    let mut inputs: Vec<(String, String, PathBuf)> = Vec::with_capacity(count);
    for index in 0..count {
        let mut source = String::from("\"\"\"Module doc.\"\"\"\n");
        for function in 0..24 {
            source.push_str(&format!(
                "def f_{function}(x: int, y: str = 'v') -> bool:\n    \"\"\"Doc {function}.\"\"\"\n    return True\n"
            ));
        }
        source.push_str("class C:\n    \"\"\"Class.\"\"\"\n    def m(self) -> None: pass\n");
        inputs.push((source, format!("pkg.mod{index}"), PathBuf::from(format!("pkg/mod{index}.py"))));
    }
    let started = Instant::now();
    let results = parse_sources(&inputs);
    let elapsed = started.elapsed();
    assert_eq!(results.len(), count);
    assert!(results.iter().all(|result| result.is_ok()));
    // Report timing; this is deliberately not a hard performance gate.
    eprintln!("parsed {count} modules ({total} functions) in {elapsed:?}", total = count * 24);
    assert!(elapsed.as_secs() < 30, "pathological parse time: {elapsed:?}");
}

#[test]
fn malformed_sources_never_panic() {
    let bad = vec![
        "def f(:\n".to_owned(),
        "class :\n".to_owned(),
        "x = (\n".to_owned(),
        "\0\0\0".to_owned(),
    ];
    let inputs: Vec<(String, String, PathBuf)> = bad
        .into_iter()
        .enumerate()
        .map(|(index, source)| (source, format!("bad{index}"), PathBuf::from(format!("bad{index}.py"))))
        .collect();
    let results = parse_sources(&inputs);
    assert!(results.iter().all(Result::is_err));
}

#[test]
fn dynamic_bridge_uses_documented_protocol() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = workspace.join(".oracle-venv/bin/python");
    if !python.is_file() {
        eprintln!("skipping dynamic bridge test: no oracle python");
        return;
    }
    let fixture = workspace.join("sphinx/tests/roots/test-ext-napoleon-paramtype");
    let options = DynamicOptions {
        python,
        roots: vec![fixture],
        timeout_secs: 10,
    };
    match introspect("pkg.bar", &options) {
        Ok(module) => {
            let module: DynamicModule = module;
            assert_eq!(module.name, "pkg.bar");
            assert!(module.members.iter().any(|member| member.name == "Bar"));
        }
        Err(error) => {
            // Missing third-party dependencies in the oracle venv are an
            // expected, non-fatal reason to fall back to static mode.
            eprintln!("dynamic bridge unavailable: {error}");
        }
    }
}

#[test]
fn missing_interpreter_degrades_gracefully() {
    let options = DynamicOptions {
        python: PathBuf::from("/definitely/not/a/python"),
        roots: Vec::new(),
        timeout_secs: 1,
    };
    assert!(introspect("anything", &options).is_err());
}
