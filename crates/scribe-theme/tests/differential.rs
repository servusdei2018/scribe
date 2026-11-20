//! Differential tests: render the same template expressions with the Python
//! Jinja2 oracle and with minijinja, then compare normalized output.
//!
//! The oracle script re-implements the exact helper bodies from
//! `sphinx/jinja2glue.py` and `sphinx/builders/html/__init__.py` so the
//! comparison validates *our Rust implementations* against upstream behavior,
//! not against a second guess.  Normalization is intentionally small:
//! leading/trailing whitespace is trimmed and Python's `True`/`False` are
//! lowercased to minijinja's boolean spelling.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use scribe_theme::{RenderContext, TemplateEnvironment, ThemeManager};

const EXPRESSIONS: &[&str] = &[
    "{{ pathto('other') }}",
    "{{ pathto('index') }}",
    "{{ pathto('_static/x.css', 1) }}",
    "{{ pathto('https://cdn/x.css', resource=True) }}",
    "{{ hasdoc('index') }} / {{ hasdoc('missing') }}",
    "{{ 'yes'|tobool }} / {{ 'off'|tobool }}",
    "{{ '42'|toint + 1 }} / {{ 'nope'|toint }}",
    "{{ 0|todim }} / {{ 12|todim }} / {{ '940px'|todim }}",
    "{{ '<b>hi</b>'|striptags }}",
    "{% set g = idgen() %}{{ g.current() }}{{ g.next() }}{{ g.current() }}{{ g.next() }}",
    "{{ accesskey('a') }}|{{ accesskey('a') }}|{{ accesskey('B') }}",
    "{{ 1 + 2 }} / {{ 'a' ~ 'b' }}",
    "{{ 'a b c d'|replace(' ', '-') }}",
    // Sphinx's Jinja environment has autoescape off: literal MarkupSafe
    // entities pass through verbatim while `|e` still escapes explicitly.
    "{{ '&#8212;' }}|{{ 'a & b'|e }}",
];

const ORACLE_SCRIPT: &str = r#"
import json, re, sys
from urllib.parse import quote
from jinja2 import Environment

def relative_uri(base, to):
    if to.startswith('/'):
        return to
    b2 = base.partition('#')[0].split('/')
    t2 = to.partition('#')[0].split('/')
    for _ in range(min(len(b2) - 1, len(t2))):
        if b2[0] != t2[0]:
            break
        b2.pop(0); t2.pop(0)
    if b2 == t2:
        return ''
    if len(b2) == 1 and t2 == ['']:
        return './'
    return ('../' * (len(b2) - 1)) + '/'.join(t2)

def pathto(otheruri, resource=False, baseuri='index.html'):
    if resource and '://' in otheruri:
        return otheruri
    if not resource:
        otheruri = quote(otheruri, safe='/') + '.html'
    return relative_uri(baseuri, otheruri) or '#'

def tobool(val):
    if isinstance(val, str):
        return val.lower() in {'true', '1', 'yes', 'on'}
    return bool(val)

def toint(val):
    try:
        return int(val)
    except ValueError:
        return 0

def todim(val):
    if val is None:
        return 'initial'
    if str(val).isdigit():
        return '0' if int(val) == 0 else '%spx' % val
    return val

def striptags(s):
    return re.sub(r'(<!--.*?-->|<[^>]*>)', '', str(s))

class IdGen:
    def __init__(self):
        self.id = 0
    def current(self):
        return self.id
    def next(self):
        self.id += 1
        return self.id

expressions = json.load(sys.stdin)
env = Environment()
env.filters.update(tobool=tobool, toint=toint, todim=todim, striptags=striptags)
results = []
for expression in expressions:
    seen = set()
    def accesskey(key, seen=seen):
        if key and key not in seen:
            seen.add(key)
            return 'accesskey="%s"' % key
        return ''
    env.globals.update(
        pathto=pathto,
        hasdoc=lambda name: name in {'index', 'other', 'search', 'genindex'},
        idgen=IdGen,
        accesskey=accesskey,
    )
    results.append(env.from_string(expression).render())
print(json.dumps(results))
"#;

fn oracle_python() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.oracle-venv/bin/python")
}

fn normalize(value: &str) -> String {
    value.trim().replace("True", "true").replace("False", "false")
}

fn oracle_outputs() -> Vec<String> {
    let mut child = Command::new(oracle_python())
        .arg("-c")
        .arg(ORACLE_SCRIPT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("oracle python must be available");
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        let expressions = serde_json::to_vec(EXPRESSIONS).expect("serialize expressions");
        stdin.write_all(&expressions).expect("write expressions");
    }
    let output = child.wait_with_output().expect("oracle must finish");
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Vec<String>>(&output.stdout).expect("oracle JSON")
}

#[test]
fn expression_subset_matches_jinja2_oracle() {
    let expected = oracle_outputs();
    assert_eq!(expected.len(), EXPRESSIONS.len());

    let theme = ThemeManager::new().get_theme("basic").expect("basic");
    let env = TemplateEnvironment::with_search_paths(&theme, Vec::new()).expect("env");
    let ctx = RenderContext::new("index")
        .with_theme("basic")
        .add_doc("index")
        .add_doc("other");

    for (expression, oracle) in EXPRESSIONS.iter().zip(expected.iter()) {
        let actual = env
            .render_string("expr.html", expression, &ctx)
            .unwrap_or_else(|error| panic!("minijinja failed for `{expression}`: {error}"));
        assert_eq!(
            normalize(&actual),
            normalize(oracle),
            "expression: {expression}"
        );
    }
}

#[test]
fn oracle_is_available_and_reports_expected_booleans() {
    let outputs = oracle_outputs();
    assert_eq!(normalize(&outputs[5]), "true / false");
    assert_eq!(normalize(&outputs[9]), "0112");
}
