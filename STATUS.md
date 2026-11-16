# scribe — Master Status Ledger

**Mission:** Production-grade Rust reimplementation of the Python Sphinx documentation engine.

**Working dir:** `/home/credo/code/scribe`  ·  **Oracle:** `./sphinx` (Python Sphinx checkout, HEAD `035a07c3`, 2026-10-30)

**Decisions locked:**
- Aspirational targets (95% fixture parity, 10–50× speedup, self-hosting `sphinx/doc`) are **tracked metrics**, not hard per-phase gates.
- Hard phase gates: `cargo check --workspace --all-targets` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green; phase-specific functional criterion.
- Differential oracle: locally installed Sphinx from `./sphinx` via `uv`.
- VCS: outer repo initialized; `.gitignore` excludes `sphinx/`; one commit per phase (`phase-NN: <title>`).

## Phase status

| Phase | Title | Status | Gate evidence | Metrics / notes |
|---|---|---|---|---|
| 00 | Setup (git, workspace scaffold, oracle, harness) | DONE | Oracle `sphinx-build 9.3.0+/035a07c32`; harness test green (see below) | Repo initialized; workspace scaffold + harness delivered in Phase 01 |
| 01 | Workspace Architecture, CLI Skeleton & Oracle Harness | DONE | check/clippy/test clean; CLI smoke tests + oracle differential green (see below) | 11 crates, 12 tests, 2 differential tests |
| 02 | Configuration Engine & `conf.py` Bridge | DONE | check/clippy/test clean; CLI loads real `conf.py`; 14 roots + oracle differential green (see below) | Typed registry + `ConfigValue`, `Tags` parser, TOML loader, Python JSON bridge + static fallback; miette `scribe::config::load_failed` |
| 03 | Unified Document AST & Doctree Model | PENDING | — | — |
| 04 | RST Lexer & Indentation/Block Scanner | PENDING | — | — |
| 05 | RST Inline Markup & Role Parser | PENDING | — | — |
| 06 | Directive & Role Registry + Core Directives | PENDING | — | — |
| 07 | MyST Markdown Parser Integration | PENDING | — | — |
| 08 | Build Environment, Discovery & Incremental Cache | PENDING | — | — |
| 09 | Toctree Collector & Section Numbering | PENDING | — | — |
| 10 | AST Transform Pipeline & Post-Transforms | PENDING | — | — |
| 11 | Domain Architecture & Standard Domain | PENDING | — | — |
| 12 | Python Domain (`py`) | PENDING | — | — |
| 13 | C/C++/JS/reST/Math Domains | PENDING | — | — |
| 14 | Cross-Reference Resolution & `intersphinx` | PENDING | — | — |
| 15 | Syntax Highlighting & Pygments Compatibility | PENDING | — | — |
| 16 | Jinja2 Templating & Theme System | PENDING | — | — |
| 17 | HTML5 Writer & Standalone HTML Builder | PENDING | — | — |
| 18 | Client-Side Search Indexer | PENDING | — | — |
| 19 | DirectoryHTML, SingleHTML & Metadata Builders | PENDING | — | — |
| 20 | Built-In Core Extensions Suite | PENDING | — | — |
| 21 | Native Python autodoc + napoleon Engine | PENDING | — | — |
| 22 | autosummary & doctest Builders/Extensions | PENDING | — | — |
| 23 | LaTeX Writer & PDF Builder | PENDING | — | — |
| 24 | EPUB3, Manpage, Texinfo & Plaintext Builders | PENDING | — | — |
| 25 | Linkcheck, XML, Pseudoxml & Changes Builders | PENDING | — | — |
| 26 | Internationalization (gettext & .mo Pipeline) | PENDING | — | — |
| 27 | Third-Party Python & Wasm Extension Bridge | PENDING | — | — |
| 28 | Live-Reload Dev Server & Watch Mode | PENDING | — | — |
| 29 | Upstream Test Suite Conformance & Fuzz Hardening | PENDING | — | — |
| 30 | Performance, Packaging, Docs & Release Readiness | PENDING | — | — |

## Frozen cross-phase contracts

Defined in `ARCHITECTURE.md` §5. Phase 01 intentionally defines **none** of these types, so no
premature shape is baked in.

- **Phase 03:** `scribe-ast::Document` node enum + `u32` arena `NodeId`; `pending_xref` node shape (FROZEN once Phase 03 lands).
- **Phase 08:** `BuildEnvironment` public API.
- **Phase 11:** `Domain` trait (`resolve_xref` / `resolve_any_xref`), shared with Phase 10 transform engine.

## Oracle setup

- [x] Python Sphinx installed from `./sphinx` via `uv` into `.oracle-venv/`
- [x] `sphinx-build --version` verified → `sphinx-build 9.3.0+/035a07c32`
- [x] Differential runner executes a baseline fixture (`test-basic`, `-b pseudoxml`)

## Phase 01 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | All suites green: 9 CLI unit + 1 core unit + 2 differential + doctests; **0 failed** |
| `cargo test -p scribe-cli --test differential -- --nocapture` | 2 passed; oracle invoked on `test-basic`, produced `index.pseudoxml`, markers matched |
| `cargo run -p scribe-cli -- --help` | Lists `build`, `quickstart`, `clean`, `check`, `help`; `-V/--version` present |
| `cargo run -p scribe-cli -- build --help` | Mirrors all sphinx-build flags (see `ARCHITECTURE.md` §2) |
| `cargo run -p scribe-cli -- --version` | `scribe 0.1.0` |
| `cargo run -p scribe-cli -- build --version` | `sphinx-build 9.3.0+/035a07c32 (scribe compatibility layer)` |
| `./target/debug/scribe build -b pseudoxml -j auto -D html_theme=basic <src> <out>` | Structured miette `scribe::unimplemented_builder` diagnostic; exit 1, no panic |
| `./target/debug/scribe build -j 0 src out` | clap rejects: job count must be positive or `auto`; exit 2 |
| `./target/debug/scribe build -D no-equals src out` | clap rejects: invalid `name=value` override; exit 2 |
| `./target/debug/scribe build -a src out index.rst` | miette `scribe::cli::invalid_build_options`; exit 1 |
| `./target/debug/scribe quickstart <path>` | Banner printed; exit 0 |
| `./target/debug/scribe check <path>` | "No source discovered yet"; exit 0 |
| `./target/debug/scribe clean <path>` | Removes `.scribe-cache` when present, otherwise reports absence; exit 0 |

**Oracle path:** `/home/credo/code/scribe/.oracle-venv/bin/sphinx-build`
**Baseline fixture:** `/home/credo/code/scribe/sphinx/tests/roots/test-basic`

## Phase 02 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **36 passed, 0 failed** (15 scribe-config unit, 5 conf.py-root integration, 3 config differential, 11 CLI/core/differential) |
| `cargo test -p scribe-config` | 23 passed (15 unit + 5 roots + 3 differential); tags differential compares 33 expressions against oracle |
| `./target/debug/scribe build -c sphinx/tests/roots/test-config sphinx/tests/roots/test-config /tmp/scribe-out` | Prints `scribe: loaded configuration from .../test-config/conf.py` plus `project = "Sphinx <Tests>"`, `release = "0.6alpha1"`, `html_theme = "alabaster"`, `html_title = "Sphinx <Tests> 0.6alpha1 documentation"`; then the expected miette `scribe::unimplemented_builder` (exit 1), **no panic** |
| `./target/debug/scribe build -c sphinx/tests/roots/test-root ...` | Loads via Python bridge despite `sys`/`docutils`/`sphinx` imports; 8 unserializable names reported as warnings, `jsmath_path` reported as unknown key; values retained |
| `./target/debug/scribe build -c /tmp/scribe-bad-conf ...` (conf.py raises `RuntimeError`) | Partial namespace value (`project`) retained, `warning [python-execution-failed]` with traceback printed, **no panic** |
| `./target/debug/scribe build ... -D html_theme_options.sidebar_width=300px` | Dict-member override applied with no warning |
| `./target/debug/scribe build ... -D project=CLI -A answer=42 -n -t draft` | Unit test asserts `project`, `html_context.answer = 42` and `nitpicky = true` |

**Real `conf.py` roots validated (14 via static fallback, 2 also via Python bridge):**

`test-basic`, `test-config`, `test-copyright-multiline`, `test-directive-only`, `test-ext-todo`,
`test-ext-autodoc`, `test-theming`, `test-build-html-theme-having-multiple-stylesheets`,
`test-domain-py`, `test-directive-code`, `test-ext-intersphinx-role`,
`test-ext-extlinks-hardcoded-urls-multiple-replacements`, `test-root`, `test-smartquotes`.

Differential comparison additionally runs the oracle's `Sphinx(...)` on `test-basic`, `test-config`,
`test-ext-autodoc`, `test-root` and `test-theming` and asserts parity for 27 resolved config keys
(including `source_suffix`, computed `html_title`/`epub_title`/`latex_documents`/`man_pages`/
`texinfo_documents`/`htmlhelp_basename`). The `Tags` differential covers 33 expressions including
parentheses, conditional expressions, constants and malformed inputs.

## Risks / open items

- **Tags constant fidelity (refinement divergence):** the Phase 02 refinement expected `true`/`True`/
  `false`/`False`/`none`/`None` to evaluate with Python truthiness. Direct inspection of the pinned
  `sphinx/sphinx/util/tags.py` (`_eval_node`, lines 86-101) shows **no `Const` branch**, and the
  oracle raises `TypeError: invalid node, check parsing` for `True`, `not None`, etc. We follow the
  source/oracle: constants and omitted-else branches return `TagsError::InvalidNode`. The
  differential test asserts this against the oracle and passes.
- `conf.py` files that import optional third-party modules: the subprocess bridge records a
  `python-execution-failed` warning but keeps all values assigned before the failure; values from
  later statements are unavailable. The static fallback skips those statements and reports a
  `static-parse-issue` warning. No fixture in the validated set fails this way.
- Python interpreter discovery uses `SCRIBE_PYTHON`, then the compile-time workspace path
  `.oracle-venv/bin/python`, then ancestor search and `python3` on `PATH`. If none is available,
  `BridgeStrategy::Auto` transparently uses the Rust static parser.
- TeX escaping in computed `latex_documents` covers the common ASCII specials; full Unicode LaTeX
  escapes belong to the LaTeX writer phase (Phase 23).
- No Python subprocess output is trusted blindly: conf.py `stdout` is captured into `StringIO`, the
  child has a 15s timeout, and only JSON-serializable values cross the boundary.
- Python 3.15 + Sphinx HEAD compatibility: **resolved** — oracle installed and runs fixtures successfully.
- Domain ↔ transform circular dependency mitigated by freezing `pending_xref` + `Domain` contracts early (owned by Phases 03/11).
- Upstream `--keep-going` is `argparse.SUPPRESS`ed, so `sphinx-build --help` does not list it; mirrored exactly (accepted, hidden).
- `-M/--mode` and `-r/--release` are make-mode/quickstart flags, not `sphinx-build` parser flags; intentionally deferred until a compatibility shim is scoped.
- Dependency slots for later phases (`rayon`, `dashmap`, `blake3`, etc.) are declared but unused until their owning phases; no MSRV issues observed with `rust-version = 1.85` on toolchain 1.99.0.
- No git commits created — orchestrator will audit gates and commit.
