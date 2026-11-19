//! Python `doctest` subprocess protocol (Phase 22).
//!
//! Reimplementing Python's `doctest` output checker in Rust would risk subtle
//! divergence in ellipsis/whitespace/exception normalization. Instead the
//! builder serializes the grouped test codes to JSON, spawns a configurable
//! Python interpreter with the vendored worker script below, and parses a
//! compact JSON result. The worker mirrors upstream
//! `sphinx/ext/doctest.py::DocTestBuilder.test_group` almost line for line, so
//! the captured failure diffs and summaries are byte-identical to the oracle.
//!
//! A missing interpreter is a non-fatal condition: [`find_python`] returns
//! `None` and the builder reports every example as skipped.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::extract::TestGroup;

/// Vendored Python worker. Kept as a literal so the protocol is auditable.
pub const WORKER_SCRIPT: &str = r####"
import doctest
import io
import json
import re
import sys

payload = json.load(sys.stdin)
for root in reversed(payload.get('python_paths', [])):
    if root not in sys.path:
        sys.path.insert(0, root)

flags = int(payload.get('flags', 41))
show = bool(payload.get('show_successes', True))
fail_fast = bool(payload.get('fail_fast', False))
global_setup = payload.get('global_setup') or ''
global_cleanup = payload.get('global_cleanup') or ''

out = io.StringIO()
def emit(text):
    out.write(text)

compile_type = ['single']
def patched_compile(code, name, typ, flg, dont_inherit):
    return compile(code, name, compile_type[0], flg, dont_inherit)
doctest.compile = patched_compile

parser = doctest.DocTestParser()
exception_re = getattr(parser, '_EXCEPTION_RE', None)
if exception_re is None:
    exception_re = re.compile(r'^(?P<msg>\w+.*)$', re.MULTILINE)

def version_ok(spec):
    if not spec:
        return True
    try:
        from packaging.specifiers import SpecifierSet
        version = '.'.join(map(str, sys.version_info[:3]))
        return str(version) in SpecifierSet(spec)
    except Exception:
        return True

def skipped(code):
    # Upstream drops `:skipif:` nodes before grouping; `:pyversion:` instead
    # sets the SKIP flag on the example so it is still counted as a try.
    expr = code.get('skipif')
    if expr:
        ctx = {}
        try:
            if global_setup:
                exec(global_setup, ctx)
            return bool(eval(expr, ctx))
        except Exception:
            return False
    return False

def opts(code):
    return {int(key): bool(value) for key, value in (code.get('options') or {}).items()}

def doctest_opts(code):
    result = opts(code)
    if not version_ok(code.get('pyversion')):
        result[doctest.SKIP] = True
    return result

def make_example(code, want='', exc_msg=None):
    return doctest.Example(
        code['code'], want, exc_msg=exc_msg,
        lineno=int(code.get('lineno') or 0), options=opts(code),
    )

def summarize(runner, verbose):
    buf = io.StringIO()
    old = sys.stdout
    sys.stdout = buf
    try:
        result = runner.summarize(verbose=verbose)
    finally:
        sys.stdout = old
    emit(buf.getvalue())
    return result

def run_setup_cleanup(runner, codes, group_name, what, filename, ns):
    examples = [make_example(code) for code in codes if not skipped(code)]
    if not examples:
        return True
    test = doctest.DocTest(
        examples, {}, '%s (%s code)' % (group_name, what), filename, 0, None,
    )
    test.globs = ns
    old_failures = runner.failures
    compile_type[0] = 'exec'
    runner.run(test, out=emit, clear_globs=False)
    return runner.failures <= old_failures

results = []
total_tries = 0
total_failures = 0
setup_failures = 0
cleanup_failures = 0
stop = False

for doc in payload.get('docs', []):
    docname = doc['docname']
    groups = doc['groups']
    if not groups:
        results.append({'docname': docname, 'success': True, 'tries': 0, 'failures': 0})
        continue
    if show:
        emit('\nDocument: %s\n----------%s\n' % (docname, '-' * len(docname)))

    setup_runner = doctest.DocTestRunner(verbose=False, optionflags=flags)
    test_runner = doctest.DocTestRunner(verbose=False, optionflags=flags)
    cleanup_runner = doctest.DocTestRunner(verbose=False, optionflags=flags)
    test_runner._fakeout = setup_runner._fakeout
    cleanup_runner._fakeout = setup_runner._fakeout

    doc_success = True
    for group in groups:
        name = group['name']
        ns = {}
        if global_setup:
            compile_type[0] = 'exec'
            try:
                exec(compile(global_setup, '<global_setup>', 'exec'), ns)
            except Exception:
                pass
        setup = list(group.get('setup', []))
        cleanup = list(group.get('cleanup', []))
        filename = (setup[0].get('filename') if setup else None) or (docname + '.rst')
        if global_setup:
            setup.insert(0, {
                'code': global_setup, 'options': {}, 'lineno': 0,
                'filename': '<global_setup>', 'skipif': None, 'pyversion': None,
            })
        if global_cleanup:
            cleanup.append({
                'code': global_cleanup, 'options': {}, 'lineno': 0,
                'filename': '<global_cleanup>', 'skipif': None, 'pyversion': None,
            })
        if not run_setup_cleanup(setup_runner, setup, name, 'setup', filename, ns):
            doc_success = False
            if fail_fast:
                stop = True
                break
            continue

        group_success = True
        for test in group.get('tests', []):
            codes = [code for code in test if not skipped(code)]
            if not codes:
                continue
            if len(codes) == 1:
                source = codes[0]
                try:
                    case = parser.get_doctest(
                        source['code'], {}, name,
                        source.get('filename') or filename,
                        int(source.get('lineno') or 0),
                    )
                except Exception:
                    emit('ignoring invalid doctest code: %r\n' % (source['code'],))
                    continue
                if not case.examples:
                    continue
                for example in case.examples:
                    merged = doctest_opts(source)
                    merged.update(example.options)
                    example.options = merged
                compile_type[0] = 'single'
            else:
                source, output_code = codes[0], codes[1]
                output = output_code['code']
                out_opts = opts(output_code)
                out_opts[doctest.DONT_ACCEPT_BLANKLINE] = True
                match = exception_re.match(output.strip())
                exc_msg = None
                if match and 'msg' in match.groupdict():
                    exc_msg = match.group('msg')
                example = doctest.Example(
                    source['code'], output, exc_msg=exc_msg,
                    lineno=int(source.get('lineno') or 0), options=out_opts,
                )
                case = doctest.DocTest(
                    [example], {}, name, source.get('filename') or filename,
                    int(source.get('lineno') or 0), None,
                )
                compile_type[0] = 'exec'
            case.globs = ns
            old_failures = test_runner.failures
            test_runner.run(case, out=emit, clear_globs=False)
            if test_runner.failures > old_failures:
                group_success = False
                if fail_fast:
                    break

        if not run_setup_cleanup(cleanup_runner, cleanup, name, 'cleanup', filename, ns):
            doc_success = False
        if not group_success:
            doc_success = False
        if fail_fast and not group_success:
            stop = True
            break

    failures, tries = summarize(setup_runner, False)
    setup_failures += failures
    if test_runner.tries:
        failures, tries = summarize(test_runner, show)
        total_failures += failures
        total_tries += tries
    if cleanup_runner.tries:
        failures, tries = summarize(cleanup_runner, show)
        cleanup_failures += failures
    results.append({
        'docname': docname,
        'success': doc_success,
        'tries': test_runner.tries,
        'failures': test_runner.failures,
    })
    if stop:
        break

print(json.dumps({
    'output': out.getvalue(),
    'docs': results,
    'total_tries': total_tries,
    'total_failures': total_failures,
    'setup_failures': setup_failures,
    'cleanup_failures': cleanup_failures,
}, ensure_ascii=False))
"####;

/// One document's test groups handed to the worker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctestDocument {
    /// Document name.
    pub docname: String,
    /// Groups in document order.
    pub groups: Vec<TestGroup>,
}

/// Input payload for the worker.
#[derive(Clone, Debug, Serialize)]
struct WorkerRequest<'a> {
    docs: &'a [DoctestDocument],
    flags: u32,
    show_successes: bool,
    fail_fast: bool,
    global_setup: String,
    global_cleanup: String,
    python_paths: Vec<String>,
}

/// Per-document result returned by the worker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctestDocResult {
    /// Document name.
    pub docname: String,
    /// Whether every group in the document passed.
    pub success: bool,
    /// Number of test examples executed.
    pub tries: u32,
    /// Number of failed test examples (setup/cleanup excluded).
    pub failures: u32,
}

/// Aggregated worker result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoctestRunResult {
    /// Captured `doctest` runner output (failure diffs + summaries).
    pub output: String,
    /// Per-document results.
    pub docs: Vec<DoctestDocResult>,
    /// Total executed examples.
    pub total_tries: u32,
    /// Failed examples.
    pub total_failures: u32,
    /// Failed setup examples.
    pub setup_failures: u32,
    /// Failed cleanup examples.
    pub cleanup_failures: u32,
}

/// Failure modes of the worker protocol (all non-panicking).
#[derive(Clone, Debug, thiserror::Error)]
pub enum DoctestRunnerError {
    /// No Python interpreter could be located.
    #[error("no Python interpreter available for doctest execution")]
    InterpreterMissing,
    /// The interpreter could not be spawned.
    #[error("could not spawn Python interpreter {path}: {detail}")]
    Spawn {
        /// Interpreter path.
        path: String,
        /// OS detail.
        detail: String,
    },
    /// The worker did not finish within the configured timeout.
    #[error("doctest worker timed out after {secs}s")]
    Timeout {
        /// Timeout in seconds.
        secs: u64,
    },
    /// The worker output was not valid JSON / did not match the protocol.
    #[error("doctest worker protocol error: {detail}")]
    Protocol {
        /// Detail.
        detail: String,
    },
}

/// Options controlling the worker invocation.
#[derive(Clone, Debug)]
pub struct RunnerOptions {
    /// Interpreter to spawn.
    pub python: PathBuf,
    /// Extra `sys.path` entries.
    pub python_paths: Vec<String>,
    /// Comparison flag bitmask.
    pub flags: u32,
    /// Whether successful groups are listed in the output.
    pub show_successes: bool,
    /// Stop after the first failure.
    pub fail_fast: bool,
    /// Python code prepended to every group.
    pub global_setup: String,
    /// Python code appended to every group.
    pub global_cleanup: String,
    /// Kill the worker after this many seconds (guards against hanging code).
    pub timeout_secs: u64,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python3"),
            python_paths: Vec::new(),
            flags: super::normalize::flags::DEFAULT,
            show_successes: true,
            fail_fast: false,
            global_setup: String::new(),
            global_cleanup: String::new(),
            timeout_secs: 60,
        }
    }
}

/// Locates a usable Python interpreter.
///
/// Order: `SCRIBE_PYTHON`, `SCRIBE_DOCTEST_PYTHON`, the workspace's pinned
/// `.oracle-venv/bin/python`, then `python3`/`python` on `PATH`.
#[must_use]
pub fn find_python() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = [
        std::env::var_os("SCRIBE_DOCTEST_PYTHON").map(PathBuf::from),
        std::env::var_os("SCRIBE_PYTHON").map(PathBuf::from),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.oracle-venv/bin/python")),
        which("python3"),
        which("python"),
    ]
    .into_iter()
    .flatten()
    .collect();
    candidates.into_iter().find(|path| path.is_file())
}

fn which(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Runs the worker and returns its structured result.
pub fn run(
    docs: &[DoctestDocument],
    options: &RunnerOptions,
) -> Result<DoctestRunResult, DoctestRunnerError> {
    let python = &options.python;
    if !python.is_file() && which(&python.to_string_lossy()).is_none() {
        return Err(DoctestRunnerError::InterpreterMissing);
    }
    let request = WorkerRequest {
        docs,
        flags: options.flags,
        show_successes: options.show_successes,
        fail_fast: options.fail_fast,
        global_setup: options.global_setup.clone(),
        global_cleanup: options.global_cleanup.clone(),
        python_paths: options.python_paths.clone(),
    };
    let payload = serde_json::to_string(&request)
        .map_err(|error| DoctestRunnerError::Protocol { detail: error.to_string() })?;
    let mut child = Command::new(python)
        .arg("-c")
        .arg(WORKER_SCRIPT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| DoctestRunnerError::Spawn {
            path: python.display().to_string(),
            detail: error.to_string(),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= Duration::from_secs(options.timeout_secs) => {
                let _ = child.kill();
                return Err(DoctestRunnerError::Timeout { secs: options.timeout_secs });
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return Err(DoctestRunnerError::Spawn {
                    path: python.display().to_string(),
                    detail: error.to_string(),
                });
            }
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|error| DoctestRunnerError::Spawn {
            path: python.display().to_string(),
            detail: error.to_string(),
        })?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DoctestRunnerError::Protocol {
            detail: format!("worker exited with {status}: {}", stderr.trim()),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).map_err(|error| DoctestRunnerError::Protocol {
        detail: format!("invalid worker JSON: {error}"),
    })
}

#[must_use]
pub fn worker_source_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.oracle-venv/bin/python")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_script_is_self_contained() {
        assert!(WORKER_SCRIPT.contains("doctest.DocTestRunner"));
        assert!(WORKER_SCRIPT.contains("IGNORE_EXCEPTION_DETAIL") || WORKER_SCRIPT.contains("optionflags=flags"));
        assert!(WORKER_SCRIPT.contains("json.dumps"));
    }

    #[test]
    fn missing_interpreter_is_reported_not_panicked() {
        let options = RunnerOptions {
            python: PathBuf::from("/definitely/not/a/python"),
            ..RunnerOptions::default()
        };
        assert!(run(&[], &options).is_err());
    }
}
