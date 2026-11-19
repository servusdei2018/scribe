//! Native `sphinx.ext.doctest` builder (Phase 22).
//!
//! Five directives emit ordinary `LiteralBlock` / `Comment` nodes carrying the
//! upstream `testnodetype` marker; the builder extracts `>>>` blocks and those
//! directives, groups setup/test/cleanup snippets, executes them with Python's
//! real `doctest` engine through a small subprocess protocol and writes an
//! upstream-shaped `output.txt`. See `ARCHITECTURE.md` §28.

pub mod builder;
pub mod directives;
pub mod error;
pub mod extract;
pub mod normalize;
pub mod runner;

pub use builder::{format_summary, register_config_keys, DoctestBuilder, DoctestResult};
pub use directives::{register_directives, DoctestDirectiveKind, TestDirective};
pub use error::{DoctestError, Result};
pub use extract::{doctest_block_group, extract_codes, group_codes, TestCode, TestGroup};
pub use normalize::{
    ellipsis_match, failure_diff, normalize_whitespace, option_flag, outputs_match,
    parse_option_string, FailureDiff,
};
pub use runner::{
    find_python, run as run_worker, DoctestDocResult, DoctestDocument, DoctestRunResult,
    DoctestRunnerError, RunnerOptions, WORKER_SCRIPT,
};
