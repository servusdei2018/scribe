//! Lightweight, opt-in build-stage profiler (Phase 32).
//!
//! Set `SCRIBE_PROFILE=1` to print one line per named stage boundary to stderr:
//!
//! ```text
//! SCRIBE_PROFILE stage=parse  t=+12.34ms  rss=73420KB  hwm=73420KB  delta_rss=+1200KB
//! ```
//!
//! `rss` is the *current* resident set size (from `/proc/self/status` `VmRSS`)
//! and `hwm` is the process high-water mark (`VmHWM`). The delta is against the
//! previous mark, so a falling `delta_rss` reveals that a stage released memory.
//! The profiler is a no-op in normal builds and never panics: any failure to
//! read the procfs files degrades to a `?` placeholder.

use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();

fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("SCRIBE_PROFILE").is_some_and(|v| v != "0"))
}

/// Sample `VmRSS` and `VmHWM` (in KB) from `/proc/self/status`.
fn sample() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut rss = None;
    let mut hwm = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
        if rss.is_some() && hwm.is_some() {
            break;
        }
    }
    Some((rss?, hwm?))
}

struct State {
    started: Instant,
    last: OnceLock<u64>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// Return free heap pages to the OS after a build phase that allocates and
/// releases many short-lived buffers (parsing, rendering, serialization).
/// glibc's allocator otherwise keeps those pages resident, inflating peak RSS.
/// No-op on non-glibc targets; a failed trim is harmless.
pub fn trim_heap() {
    #[cfg(target_env = "gnu")]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: `malloc_trim(0)` is documented by glibc as always safe to
        // call; it only releases free heap pages and reports success/failure,
        // both of which are ignored here.
        let _ = unsafe { malloc_trim(0) };
    }
}

/// Print a stage boundary mark when `SCRIBE_PROFILE` is set; otherwise a no-op.
pub fn mark(label: &str) {
    if !enabled() {
        return;
    }
    let state = STATE.get_or_init(|| State {
        started: Instant::now(),
        last: OnceLock::new(),
    });
    let (rss, hwm) = sample().unwrap_or((0, 0));
    let elapsed_ms = state.started.elapsed().as_secs_f64() * 1000.0;
    let delta = match state.last.get() {
        Some(prev) => {
            let d = rss as i64 - *prev as i64;
            format!("{d:+}")
        }
        None => "base".to_owned(),
    };
    let _ = state.last.set(rss);
    eprintln!(
        "SCRIBE_PROFILE stage={label} t=+{elapsed_ms:.2}ms rss={rss}KB hwm={hwm}KB delta_rss={delta}"
    );
}
