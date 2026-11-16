//! `conf.py` loading via a subprocess JSON bridge and a static AST fallback.
//!
//! The primary strategy executes `conf.py` with the pinned Python interpreter
//! in a helper that injects `tags` and `__file__`, then serializes the simple
//! values from the resulting namespace to JSON. Failures are reported as
//! warnings while successfully evaluated values are retained. The secondary
//! strategy parses top-level literal assignments in Rust, with no Python
//! dependency at all.

use crate::{ConfigError, ConfigValue, ConfigWarning, ConfigWarningKind};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Which `conf.py` loading strategy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeStrategy {
    /// Try Python first, then fall back to the static parser.
    #[default]
    Auto,
    /// Only execute `conf.py` with Python.
    Python,
    /// Only parse literal assignments in Rust.
    Static,
}

/// Settings for the Python subprocess bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonBridgeOptions {
    /// Interpreter to invoke.
    pub interpreter: PathBuf,
    /// Maximum wall-clock time allowed for `conf.py`.
    pub timeout: Duration,
}

impl Default for PythonBridgeOptions {
    fn default() -> Self {
        Self {
            interpreter: default_python_interpreter(),
            timeout: Duration::from_secs(15),
        }
    }
}

/// Locate a usable Python interpreter.
///
/// Preference order: `SCRIBE_PYTHON`, the workspace-local oracle venv (known at
/// compile time), an `.oracle-venv/bin/python` found by walking up from the
/// current directory, and finally `python3` from `PATH`.
pub fn default_python_interpreter() -> PathBuf {
    if let Ok(candidate) = std::env::var("SCRIBE_PYTHON") {
        if !candidate.trim().is_empty() {
            return PathBuf::from(candidate);
        }
    }

    let compile_time = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.oracle-venv/bin/python");
    if compile_time.is_file() {
        return compile_time;
    }

    if let Ok(current) = std::env::current_dir() {
        let mut directory = Some(current.as_path());
        while let Some(base) = directory {
            let candidate = base.join(".oracle-venv/bin/python");
            if candidate.is_file() {
                return candidate;
            }
            directory = base.parent();
        }
    }

    PathBuf::from("python3")
}

/// Values and diagnostics produced by either bridge strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeOutcome {
    /// Raw values from the `conf.py` namespace.
    pub values: BTreeMap<String, ConfigValue>,
    /// Non-fatal diagnostics collected while loading.
    pub warnings: Vec<ConfigWarning>,
    /// Strategy that actually produced `values`.
    pub strategy_used: BridgeStrategy,
}

impl BridgeOutcome {
    fn new(strategy_used: BridgeStrategy) -> Self {
        Self {
            values: BTreeMap::new(),
            warnings: Vec::new(),
            strategy_used,
        }
    }
}

/// Load values from a `conf.py` using the requested strategy.
pub fn load_conf_values(
    path: &Path,
    tags: &[String],
    options: &PythonBridgeOptions,
    strategy: BridgeStrategy,
) -> Result<BridgeOutcome, ConfigError> {
    match strategy {
        BridgeStrategy::Static => parse_static_conf_py(path),
        BridgeStrategy::Python => evaluate_conf_py(path, tags, options),
        BridgeStrategy::Auto => {
            let primary = evaluate_conf_py(path, tags, options);
            match primary {
                Ok(outcome) => Ok(outcome),
                Err(error) => match parse_static_conf_py(path) {
                    Ok(mut fallback) => {
                        fallback.warnings.insert(
                            0,
                            ConfigWarning::new(
                                ConfigWarningKind::PythonUnavailable,
                                format!("Python bridge failed ({error}); used the static fallback"),
                            )
                            .with_path(path),
                        );
                        Ok(fallback)
                    }
                    Err(_) => Err(error),
                },
            }
        }
    }
}

// ---- Python subprocess bridge --------------------------------------------

/// Helper executed with `python -c`. It deliberately lives in-process with the
/// child: only simple Python values leave the sandboxed namespace as JSON.
const BRIDGE_SCRIPT: &str = r###"import builtins
import io
import json
import os
import sys
import traceback


def _serialize(value, depth=0):
    if value is None or isinstance(value, (bool, int, float, str)):
        return value, True
    if depth > 12:
        return None, False
    if isinstance(value, (list, tuple, set, frozenset)):
        result = []
        for item in list(value)[:1000]:
            converted, ok = _serialize(item, depth + 1)
            if not ok:
                return None, False
            result.append(converted)
        return result, True
    if isinstance(value, dict):
        result = {}
        for key, item in list(value.items())[:1000]:
            key = key if isinstance(key, str) else str(key)
            converted, ok = _serialize(item, depth + 1)
            if not ok:
                return None, False
            result[key] = converted
        return result, True
    if hasattr(value, '__fspath__'):
        try:
            return os.fspath(value), True
        except Exception:
            pass
    if hasattr(value, 'value') and isinstance(getattr(value, 'value'), (str, int, float, bool, type(None))):
        return value.value, True
    return None, False


def _make_tags(names):
    try:
        from sphinx.util.tags import Tags as SphinxTags
        return SphinxTags(names)
    except Exception:
        pass

    class Tags:
        def __init__(self, tags):
            self._tags = set(tags)

        def __contains__(self, tag):
            return tag in self._tags

        def has(self, tag):
            return tag in self._tags

        def add(self, tag):
            self._tags.add(tag)

        def remove(self, tag):
            self._tags.discard(tag)

        def __iter__(self):
            return iter(self._tags)

        def __str__(self):
            return 'Tags(%s)' % ', '.join(sorted(self._tags))

        def __repr__(self):
            return 'Tags(%r)' % (tuple(sorted(self._tags)),)

        def eval_condition(self, condition):
            condition = condition.strip()
            if condition in ('True', 'true'):
                return True
            if condition in ('False', 'false'):
                return False
            return condition in self._tags

    return Tags(names)


def main():
    if len(sys.argv) < 3:
        sys.stderr.write('usage: bridge CONF_PATH TAGS_JSON\n')
        return 2
    conf_path = os.path.abspath(sys.argv[1])
    try:
        tag_names = json.loads(sys.argv[2])
    except Exception:
        tag_names = []
    conf_dir = os.path.dirname(conf_path)
    namespace = {
        '__file__': conf_path,
        '__builtins__': builtins,
        'tags': _make_tags(tag_names),
    }
    error = None
    tb = None
    captured = io.StringIO()
    original_stdout = sys.stdout
    original_cwd = os.getcwd()
    try:
        os.chdir(conf_dir)
        if conf_dir not in sys.path:
            sys.path.insert(0, conf_dir)
        sys.stdout = captured
        with open(conf_path, 'rb') as handle:
            source = handle.read()
        code = compile(source, conf_path, 'exec')
        exec(code, namespace)
    except BaseException as exc:
        error = '%s: %s' % (type(exc).__name__, exc)
        tb = traceback.format_exc(limit=8)
    finally:
        sys.stdout = original_stdout
        os.chdir(original_cwd)
        printed = captured.getvalue()

    values = {}
    skipped = []
    for key, value in namespace.items():
        if not isinstance(key, str) or key.startswith('__') or key == 'tags':
            continue
        converted, ok = _serialize(value)
        if ok:
            values[key] = converted
        else:
            skipped.append(key)

    payload = {
        'values': values,
        'skipped': sorted(skipped),
        'error': error,
        'traceback': tb,
        'printed': printed[:2000],
    }
    json.dump(payload, sys.stdout, ensure_ascii=False)
    sys.stdout.write('\n')
    return 0


if __name__ == '__main__':
    sys.exit(main())
"###;

#[derive(Debug, Deserialize)]
struct BridgePayload {
    #[serde(default)]
    values: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    skipped: Vec<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    traceback: Option<String>,
}

/// Execute the bridge script and return `(stdout, stderr)`.
fn run_python(
    script: &str,
    arguments: &[&str],
    options: &PythonBridgeOptions,
) -> Result<(Vec<u8>, String), ConfigError> {
    let mut child = Command::new(&options.interpreter)
        .arg("-c")
        .arg(script)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ConfigError::Bridge {
            message: format!("cannot spawn {}: {error}", options.interpreter.display()),
            stderr: String::new(),
        })?;

    let stdout_thread = child.stdout.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            buffer
        })
    });
    let stderr_thread = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            buffer
        })
    });

    let deadline = Instant::now() + options.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ConfigError::BridgeTimeout {
                        seconds: options.timeout.as_secs(),
                    });
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(ConfigError::Bridge {
                    message: format!("cannot wait for Python bridge: {error}"),
                    stderr: String::new(),
                });
            }
        }
    }

    let stdout = stdout_thread.and_then(|handle| handle.join().ok()).unwrap_or_default();
    let stderr_bytes = stderr_thread.and_then(|handle| handle.join().ok()).unwrap_or_default();
    Ok((stdout, String::from_utf8_lossy(&stderr_bytes).into_owned()))
}

fn json_to_value(value: &serde_json::Value) -> Option<ConfigValue> {
    match value {
        serde_json::Value::Null => Some(ConfigValue::None),
        serde_json::Value::Bool(flag) => Some(ConfigValue::Bool(*flag)),
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Some(ConfigValue::Int(integer))
            } else {
                number.as_f64().map(ConfigValue::Float)
            }
        }
        serde_json::Value::String(text) => Some(ConfigValue::Str(text.clone())),
        serde_json::Value::Array(items) => {
            let converted = items.iter().map(json_to_value).collect::<Option<Vec<_>>>()?;
            Some(ConfigValue::List(converted))
        }
        serde_json::Value::Object(entries) => {
            let mut converted = BTreeMap::new();
            for (key, value) in entries {
                converted.insert(key.clone(), json_to_value(value)?);
            }
            Some(ConfigValue::Dict(converted))
        }
    }
}

/// Evaluate a `conf.py` by executing it in the Python subprocess bridge.
pub fn evaluate_conf_py(
    path: &Path,
    tags: &[String],
    options: &PythonBridgeOptions,
) -> Result<BridgeOutcome, ConfigError> {
    let tags_json = serde_json::to_string(tags)?;
    let path_string = path.to_string_lossy();
    let (stdout, stderr) = run_python(
        BRIDGE_SCRIPT,
        &[path_string.as_ref(), tags_json.as_str()],
        options,
    )?;

    if stdout.is_empty() {
        return Err(ConfigError::Bridge {
            message: "Python bridge produced no output".to_owned(),
            stderr,
        });
    }

    let payload: BridgePayload = serde_json::from_slice(&stdout)?;
    let mut outcome = BridgeOutcome::new(BridgeStrategy::Python);

    if let Some(error) = payload.error {
        let mut message = format!("conf.py raised {error}");
        if let Some(traceback) = payload.traceback {
            message.push('\n');
            message.push_str(traceback.trim_end());
        }
        outcome.warnings.push(
            ConfigWarning::new(ConfigWarningKind::PythonExecutionFailed, message).with_path(path),
        );
    }
    if !stderr.trim().is_empty() {
        // stderr from imports is informational; keep it bounded and non-fatal.
        let tail: String = stderr.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        outcome
            .warnings
            .push(ConfigWarning::new(ConfigWarningKind::PythonExecutionFailed, format!("Python bridge stderr:\n{tail}")).with_path(path));
    }
    if !payload.skipped.is_empty() {
        outcome.warnings.push(
            ConfigWarning::new(
                ConfigWarningKind::UnserializableValue,
                format!(
                    "skipped {} non-serializable namespace value(s): {}",
                    payload.skipped.len(),
                    payload.skipped.join(", ")
                ),
            )
            .with_path(path),
        );
    }

    for (key, value) in payload.values {
        match json_to_value(&value) {
            Some(converted) => {
                outcome.values.insert(key, converted);
            }
            None => outcome.warnings.push(
                ConfigWarning::new(
                    ConfigWarningKind::UnserializableValue,
                    format!("could not convert value for `{key}`"),
                )
                .with_path(path),
            ),
        }
    }

    Ok(outcome)
}

// ---- Static literal parser ------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum PyKind {
    Name(String),
    Str(String),
    Int(i64),
    Float(f64),
    Op(char),
    Newline,
    Unsupported(String),
}

#[derive(Debug, Clone)]
struct PyToken {
    kind: PyKind,
    line: usize,
}

fn lex_python(source: &str) -> Result<Vec<PyToken>, ConfigError> {
    let characters: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1usize;
    let mut depth = 0i32;

    let string_prefixes = ["r", "u", "b", "f", "br", "rb", "fr", "rf"];

    while index < characters.len() {
        let ch = characters[index];
        if ch == '\n' {
            line += 1;
            if depth == 0 {
                tokens.push(PyToken { kind: PyKind::Newline, line });
            }
            index += 1;
            continue;
        }
        if ch == '\r' {
            if index + 1 < characters.len() && characters[index + 1] == '\n' {
                index += 1;
            }
            continue;
        }
        if ch == ' ' || ch == '\t' || ch == '\x0c' {
            index += 1;
            continue;
        }
        if ch == '#' {
            while index < characters.len() && characters[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if ch == '\\' && index + 1 < characters.len() && characters[index + 1] == '\n' {
            index += 2;
            line += 1;
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            index += 1;
            while index < characters.len()
                && (characters[index].is_ascii_alphanumeric() || characters[index] == '_')
            {
                index += 1;
            }
            let name: String = characters[start..index].iter().collect();
            if index < characters.len() && (characters[index] == '\'' || characters[index] == '"')
                && string_prefixes.contains(&name.to_lowercase().as_str())
            {
                let (text, unsupported, consumed, newlines) = lex_string(&characters, index, &name)?;
                index = consumed;
                line += newlines;
                let kind = if unsupported {
                    PyKind::Unsupported(name)
                } else {
                    PyKind::Str(text)
                };
                tokens.push(PyToken { kind, line });
            } else {
                tokens.push(PyToken { kind: PyKind::Name(name), line });
            }
            continue;
        }

        if ch == '\'' || ch == '"' {
            let (text, unsupported, consumed, newlines) = lex_string(&characters, index, "")?;
            index = consumed;
            line += newlines;
            let kind = if unsupported {
                PyKind::Unsupported("f-string".to_owned())
            } else {
                PyKind::Str(text)
            };
            tokens.push(PyToken { kind, line });
            continue;
        }

        if ch.is_ascii_digit() {
            let start = index;
            if ch == '0' && index + 1 < characters.len() && matches!(characters[index + 1], 'x' | 'X') {
                index += 2;
                while index < characters.len() && characters[index].is_ascii_hexdigit() {
                    index += 1;
                }
            } else {
                while index < characters.len() && characters[index].is_ascii_digit() {
                    index += 1;
                }
                if index + 1 < characters.len()
                    && characters[index] == '.'
                    && characters[index + 1].is_ascii_digit()
                {
                    index += 1;
                    while index < characters.len() && characters[index].is_ascii_digit() {
                        index += 1;
                    }
                }
                if index < characters.len() && matches!(characters[index], 'e' | 'E') {
                    let exponent_start = index;
                    index += 1;
                    if index < characters.len() && matches!(characters[index], '+' | '-') {
                        index += 1;
                    }
                    if index < characters.len() && characters[index].is_ascii_digit() {
                        while index < characters.len() && characters[index].is_ascii_digit() {
                            index += 1;
                        }
                    } else {
                        index = exponent_start;
                    }
                }
            }
            let raw: String = characters[start..index].iter().collect();
            let kind = if raw.contains(['.', 'e', 'E'])
                && raw.parse::<f64>().is_ok()
            {
                PyKind::Float(raw.parse::<f64>().unwrap_or(0.0))
            } else if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
                match i64::from_str_radix(hex, 16) {
                    Ok(number) => PyKind::Int(number),
                    Err(_) => PyKind::Unsupported(raw),
                }
            } else {
                match raw.parse::<i64>() {
                    Ok(number) => PyKind::Int(number),
                    Err(_) => PyKind::Unsupported(raw),
                }
            };
            tokens.push(PyToken { kind, line });
            continue;
        }

        if matches!(ch, '(' | '[' | '{') {
            depth += 1;
        } else if matches!(ch, ')' | ']' | '}') {
            depth = (depth - 1).max(0);
        }
        tokens.push(PyToken { kind: PyKind::Op(ch), line });
        index += 1;
    }

    tokens.push(PyToken { kind: PyKind::Newline, line });
    Ok(tokens)
}

/// Lex a Python string literal starting at `start` (the quote character).
/// Returns `(text, unsupported, new_index, newlines)`.
fn lex_string(
    characters: &[char],
    start: usize,
    prefix: &str,
) -> Result<(String, bool, usize, usize), ConfigError> {
    let lowercase = prefix.to_lowercase();
    let raw = lowercase.contains('r');
    let unsupported = lowercase.contains('f');
    let quote = characters[start];
    let triple = start + 2 < characters.len()
        && characters[start + 1] == quote
        && characters[start + 2] == quote;
    let mut index = start + if triple { 3 } else { 1 };
    let mut text = String::new();
    let mut newlines = 0usize;

    loop {
        if index >= characters.len() {
            return Err(ConfigError::StaticParse("unterminated string literal".to_owned()));
        }
        let ch = characters[index];
        if triple
            && index + 2 < characters.len()
            && characters[index] == quote
            && characters[index + 1] == quote
            && characters[index + 2] == quote
        {
            index += 3;
            break;
        }
        if !triple && ch == quote {
            index += 1;
            break;
        }
        if !triple && ch == '\n' {
            return Err(ConfigError::StaticParse("unterminated single-line string".to_owned()));
        }
        if ch == '\n' {
            newlines += 1;
        }
        if ch == '\\' && !raw && index + 1 < characters.len() {
            let escaped = characters[index + 1];
            index += 2;
            match escaped {
                'n' => text.push('\n'),
                't' => text.push('\t'),
                'r' => text.push('\r'),
                'a' => text.push('\x07'),
                'b' => text.push('\x08'),
                'f' => text.push('\x0c'),
                'v' => text.push('\x0b'),
                '0' => text.push('\0'),
                '\\' => text.push('\\'),
                '\'' => text.push('\''),
                '"' => text.push('"'),
                'x' => {
                    let value = read_hex(characters, index, 2);
                    index += 2;
                    if let Some(code) = value {
                        text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                }
                'u' => {
                    let value = read_hex(characters, index, 4);
                    index += 4;
                    if let Some(code) = value {
                        text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                }
                'U' => {
                    let value = read_hex(characters, index, 8);
                    index += 8;
                    if let Some(code) = value {
                        text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                }
                other => {
                    text.push('\\');
                    text.push(other);
                }
            }
            continue;
        }
        if ch == '\\' && raw && index + 1 < characters.len() {
            // Raw strings keep the backslash but still protect the quote.
            text.push('\\');
            text.push(characters[index + 1]);
            index += 2;
            continue;
        }
        text.push(ch);
        index += 1;
    }

    Ok((text, unsupported, index, newlines))
}

fn read_hex(characters: &[char], start: usize, count: usize) -> Option<u32> {
    if start + count > characters.len() {
        return None;
    }
    let text: String = characters[start..start + count].iter().collect();
    u32::from_str_radix(&text, 16).ok()
}

struct StaticParser {
    tokens: Vec<PyToken>,
    position: usize,
    skipped: Vec<String>,
    statements_skipped: usize,
    first_skipped_line: Option<usize>,
}

impl StaticParser {
    fn new(tokens: Vec<PyToken>) -> Self {
        Self {
            tokens,
            position: 0,
            skipped: Vec::new(),
            statements_skipped: 0,
            first_skipped_line: None,
        }
    }

    fn current(&self) -> &PyToken {
        &self.tokens[self.position.min(self.tokens.len() - 1)]
    }

    fn is_op(&self, operator: char) -> bool {
        matches!(self.current().kind, PyKind::Op(found) if found == operator)
    }

    fn is_newline(&self) -> bool {
        matches!(self.current().kind, PyKind::Newline)
    }

    fn is_end(&self) -> bool {
        self.position >= self.tokens.len() - 1 && self.is_newline()
    }

    fn advance(&mut self) -> PyToken {
        let token = self.tokens[self.position].clone();
        if self.position + 1 < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn skip_newlines(&mut self) {
        while self.is_newline() && !self.is_end() {
            self.advance();
        }
    }

    fn skip_statement(&mut self, name: Option<String>) {
        self.statements_skipped += 1;
        if self.first_skipped_line.is_none() {
            self.first_skipped_line = Some(self.current().line);
        }
        if let Some(name) = name {
            self.skipped.push(name);
        }
        while !self.is_newline() && !self.is_end() {
            self.advance();
        }
        if self.is_newline() && !self.is_end() {
            self.advance();
        }
    }

    fn parse(&mut self) -> BTreeMap<String, ConfigValue> {
        let mut values = BTreeMap::new();
        loop {
            self.skip_newlines();
            if self.is_end() {
                break;
            }

            let is_assignment = matches!(self.current().kind, PyKind::Name(_))
                && self.position + 1 < self.tokens.len()
                && matches!(self.tokens[self.position + 1].kind, PyKind::Op('='));
            if !is_assignment {
                self.skip_statement(None);
                continue;
            }

            let name = match self.advance().kind {
                PyKind::Name(name) => name,
                _ => {
                    self.skip_statement(None);
                    continue;
                }
            };
            self.advance(); // '='
            let value = self.parse_value();
            if value.is_some() && (self.is_newline() || self.is_end()) {
                if let Some(value) = value {
                    values.insert(name, value);
                }
                if self.is_newline() && !self.is_end() {
                    self.advance();
                }
            } else {
                self.skip_statement(Some(name));
            }
        }
        values
    }

    fn parse_value(&mut self) -> Option<ConfigValue> {
        if self.is_op('+') || self.is_op('-') {
            let negative = self.is_op('-');
            self.advance();
            let value = self.parse_value()?;
            return match value {
                ConfigValue::Int(number) => {
                    Some(ConfigValue::Int(if negative { -number } else { number }))
                }
                ConfigValue::Float(number) => {
                    Some(ConfigValue::Float(if negative { -number } else { number }))
                }
                _ => None,
            };
        }

        match self.current().kind.clone() {
            PyKind::Str(text) => {
                self.advance();
                let mut combined = text;
                while let PyKind::Str(next) = self.current().kind.clone() {
                    combined.push_str(&next);
                    self.advance();
                }
                Some(ConfigValue::Str(combined))
            }
            PyKind::Int(number) => {
                self.advance();
                Some(ConfigValue::Int(number))
            }
            PyKind::Float(number) => {
                self.advance();
                Some(ConfigValue::Float(number))
            }
            PyKind::Name(name) => match name.as_str() {
                "True" => {
                    self.advance();
                    Some(ConfigValue::Bool(true))
                }
                "False" => {
                    self.advance();
                    Some(ConfigValue::Bool(false))
                }
                "None" => {
                    self.advance();
                    Some(ConfigValue::None)
                }
                _ => None,
            },
            PyKind::Op('[') => {
                self.advance();
                let (items, _) = self.parse_sequence(']')?;
                Some(ConfigValue::List(items))
            }
            PyKind::Op('(') => {
                self.advance();
                let (items, had_comma) = self.parse_sequence(')')?;
                if !had_comma && items.len() == 1 {
                    items.into_iter().next()
                } else {
                    Some(ConfigValue::List(items))
                }
            }
            PyKind::Op('{') => {
                self.advance();
                self.parse_dict()
            }
            _ => None,
        }
    }

    fn parse_sequence(&mut self, closing: char) -> Option<(Vec<ConfigValue>, bool)> {
        let mut items = Vec::new();
        let mut had_comma = false;
        loop {
            while self.is_newline() && !self.is_end() {
                self.advance();
            }
            if self.is_op(closing) {
                self.advance();
                break;
            }
            let item = self.parse_value()?;
            items.push(item);
            if self.is_op(',') {
                self.advance();
                had_comma = true;
                continue;
            }
            if self.is_op(closing) {
                self.advance();
                break;
            }
            return None;
        }
        Some((items, had_comma))
    }

    fn parse_dict(&mut self) -> Option<ConfigValue> {
        let mut entries = BTreeMap::new();
        loop {
            while self.is_newline() && !self.is_end() {
                self.advance();
            }
            if self.is_op('}') {
                self.advance();
                break;
            }
            let key = self.parse_key()?;
            if !self.is_op(':') {
                return None;
            }
            self.advance();
            let value = self.parse_value()?;
            entries.insert(key, value);
            if self.is_op(',') {
                self.advance();
                continue;
            }
            if self.is_op('}') {
                self.advance();
                break;
            }
            return None;
        }
        Some(ConfigValue::Dict(entries))
    }

    fn parse_key(&mut self) -> Option<String> {
        match self.current().kind.clone() {
            PyKind::Str(text) => {
                self.advance();
                Some(text)
            }
            PyKind::Int(number) => {
                self.advance();
                Some(number.to_string())
            }
            PyKind::Name(name) => {
                self.advance();
                Some(name)
            }
            PyKind::Op('-') => {
                self.advance();
                match self.current().kind.clone() {
                    PyKind::Int(number) => {
                        self.advance();
                        Some((-number).to_string())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Parse a pure-data `conf.py` without executing any Python code.
pub fn parse_static_conf_py(path: &Path) -> Result<BridgeOutcome, ConfigError> {
    let source = std::fs::read_to_string(path)?;
    let tokens = lex_python(&source)?;
    let mut parser = StaticParser::new(tokens);
    let values = parser.parse();
    let mut outcome = BridgeOutcome::new(BridgeStrategy::Static);
    if parser.statements_skipped > 0 {
        let detail = if parser.skipped.is_empty() {
            String::new()
        } else {
            format!(" ({})", parser.skipped.join(", "))
        };
        let mut warning = ConfigWarning::new(
            ConfigWarningKind::StaticParseIssue,
            format!(
                "static fallback skipped {} non-literal top-level statement(s){detail}",
                parser.statements_skipped
            ),
        )
        .with_path(path);
        if let Some(line) = parser.first_skipped_line {
            warning = warning.with_line(line);
        }
        outcome.warnings.push(warning);
    }
    outcome.values = values;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{parse_static_conf_py, ConfigWarningKind};
    use std::path::PathBuf;

    #[test]
    fn static_parser_handles_literals_and_containers() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../sphinx/tests/roots/test-basic/conf.py");
        let outcome = parse_static_conf_py(&path).expect("fixture conf.py must parse");
        assert_eq!(outcome.values.get("html_theme").and_then(|v| v.get_str()), Some("basic"));
        assert!(outcome.values.contains_key("latex_documents"));
    }

    #[test]
    fn static_parser_reports_skipped_dynamic_statements() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../sphinx/tests/roots/test-ext-autodoc/conf.py");
        let outcome = parse_static_conf_py(&path).expect("fixture conf.py must parse");
        assert_eq!(outcome.values.get("nitpicky").and_then(|v| v.get_bool()), Some(true));
        assert!(outcome.warnings.iter().any(|w| w.kind == ConfigWarningKind::StaticParseIssue));
    }
}
