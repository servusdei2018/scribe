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
| 03 | Unified Document AST & Doctree Model | DONE | check/clippy/test clean; oracle pseudo-XML differential green (see below) | 29 scribe-ast tests (23 unit + 3 pseudo-XML + 3 round-trip) + 1 doctest; frozen `Document`/`NodeKind`/`NodeId`/`pending_xref` contract documented in `ARCHITECTURE.md` §7 |
| 04 | RST Lexer & Indentation/Block Scanner | DONE | check/clippy/test clean; oracle pseudoxml differential green (see below) | 29 scribe-rst unit + 27 block + 9 diagnostics + 5 differential + 1 doctest; frozen §7 AST untouched; seams + `Pending` contract documented in `ARCHITECTURE.md` §8 |
| 05 | RST Inline Markup & Role Parser | DONE | check/clippy/test clean; oracle pseudoxml differential + 5,120 proptest cases green (see below) | 9 inline unit + 22 focused inline + 6 inline differential + 5 fuzz properties; `pending_xref` + `XrefMeta` role contract documented in `ARCHITECTURE.md` §9 |
| 06 | Directive & Role Registry + Core Directives | DONE | check/clippy/test clean; 2 byte-exact oracle differential fixtures green (see below) | 55 built-in directives + 70 roles; typed option specs; fixpoint `resolve_pending`; 216 workspace tests |
| 07 | MyST Markdown Parser Integration | DONE | check/clippy/test clean; 16 RST↔MyST equivalence + 2 mixed-project + 2,560 fuzz cases green (see below) | `pulldown-cmark` 0.13 streaming lowerer; 71 new tests (workspace total 287); frozen §7 AST untouched |
| 08 | Build Environment, Discovery & Incremental Cache | DONE | check/clippy/test clean; oracle differential green; 3 consecutive full test runs (see below) | Frozen `BuildEnvironment` + `scribe-cache` typed store; 42 new tests (workspace total 332 incl. 3 doctests); `ARCHITECTURE.md` §12 |
| 09 | Toctree Collector & Section Numbering | DONE | check/clippy clean; 364 tests green × 3 consecutive runs; 4 real-fixture oracle differentials (see below) | `toctree` directive + glob/cycle/hierarchy/numbering/nav; 26 new tests; `ARCHITECTURE.md` §13; frozen AST + env shapes untouched |
| 10 | AST Transform Pipeline & Post-Transforms | DONE | check/clippy clean; 391 tests green × 3 consecutive runs; 4 oracle differentials (see below) | `Transform`/`PostTransform` traits, registry + deterministic `(priority, name)` pipeline, 27 built-in slots; `ARCHITECTURE.md` §14; frozen AST + env shapes untouched |
| 11 | Domain Architecture & Standard Domain | DONE | check/clippy clean; **432 tests green × 3 consecutive runs**; 5 oracle differentials (see below) | Frozen `Domain` trait + `DomainRegistry` + `ResolvedReference` + `DomainEnv`; full `std` domain; additive env side table; `ARCHITECTURE.md` §15; no commits |
| 12 | Python Domain (`py`) | DONE | check/clippy clean; **462 tests green × 3 consecutive runs**; 4 real-fixture oracle differentials (see below) | Full `py` domain + hand-written signature/annotation parser + module index; frozen AST/BuildEnvironment/Domain shapes untouched; no commits |
| 13 | C/C++/JS/reST/Math Domains | DONE | check/clippy clean; **517 tests green × 3 consecutive runs**; 6 real-fixture oracle differentials (see below) | 8 new domains + shared signature emitter; C 100% / C++ 89.5% / JS 100% signature parity; frozen AST/BuildEnvironment/Domain shapes untouched; no commits |
| 14 | Cross-Reference Resolution & `intersphinx` | DONE | check/clippy clean; **551 tests green × 3 consecutive runs**; byte-exact oracle inventory + 3 intersphinx differentials (see below) | `xref.rs` parallel engine + `XrefResolutionReport`; `intersphinx/{inventory,load,cache,resolve}.rs`; byte-compatible objects.inv v2; additive registry seam; frozen AST/BuildEnvironment/Domain shapes untouched; no commits |
| 15 | Syntax Highlighting & Pygments Compatibility | DONE | check/clippy clean; **605 tests green × 3 consecutive runs**; 5 real-fixture oracle differentials (see below) | `syntect` highlighter + Pygments-compatible token stream/CSS; `code-block`/`sourcecode`/`literalinclude`/`highlight` directives; 54 new tests (601 lib/integration + 4 doctests); `ARCHITECTURE.md` §19; frozen AST/BuildEnvironment/Domain shapes untouched; no commits |
| 16 | Jinja2 Templating & Theme System | DONE | check/clippy clean; **640 tests green × 3 consecutive runs**; 2 Jinja2-oracle differentials + 5 structural renders (see below) | `scribe-theme`: bundled `basic`/`classic`/`nature`/`alabaster`, TOML+legacy INI metadata, inheritance/cycle diagnostics, sandboxed minijinja with Sphinx globals/filters, `RenderContext` template API; `ARCHITECTURE.md` §20; frozen AST/Env/Domain shapes untouched; no commits |
| 17 | HTML5 Writer & Standalone HTML Builder | DONE | check/clippy clean; **665 tests green × 3 consecutive runs**; 2 real-fixture oracle differentials (see below) | `HTML5Translator` (frozen `Visitor`) + `StandaloneHtmlBuilder` + asset/index/inventory generation; `scribe build -b html` wired; `ARCHITECTURE.md` §21; frozen AST/Env/Domain shapes untouched; no commits |
| 18 | Client-Side Search Indexer | DONE | check/clippy clean; **680 tests green × 3 consecutive runs**; 2 real-fixture search-index differentials + query simulation (see below) | `SearchIndex` + Snowball stemmer/stop words/tokenizer + deterministic `Search.setIndex({...})` emitter; `searchindex.js`, `language_data.js`, stemmer assets and functional `search.html`; vendored English stop list + minified stemmer JS with attribution; `ARCHITECTURE.md` §22; frozen AST/Env/Domain shapes untouched; no commits |
| 19 | DirectoryHTML, SingleHTML & Metadata Builders | DONE | check/clippy clean; **698 tests green × 3 consecutive runs**; 6 oracle differentials (see ARCHITECTURE §25) | 14 new tests (workspace total 698); `ARCHITECTURE.md` §25; frozen AST/Env/Domain shapes untouched; no commits |
| 20 | Built-In Core Extensions Suite | DONE | check/clippy clean; **750 tests green × 3 consecutive runs**; 8 oracle differentials (see below) | 52 new tests (workspace total 750); native registry + 7 extensions; frozen AST/Env/Domain shapes untouched; no commits |
| 21 | Native Python autodoc + napoleon Engine | DONE | check/clippy clean; **790 tests green × 3 consecutive runs**; 5 real-fixture oracle differentials (see below) | 40 new tests (workspace total 790); static `rustpython-parser` primary path + documented dynamic worker fallback; Google/NumPy napoleon; frozen AST/Env/Domain shapes untouched; no commits |
| 22 | autosummary & doctest Builders/Extensions | DONE | check/clippy clean; **830 tests green × 3 consecutive runs**; stub + doctest oracle differentials green (see below) | 40 new tests (790 → 830); byte-exact stub/doctest report parity; frozen AST/Env/Domain shapes untouched; no commits |
| 23 | LaTeX Writer & PDF Builder | DONE | check/clippy clean; **861 tests green × 3 consecutive runs**; 6 oracle differentials + real `latexmk` PDF compile (see below) | 31 new tests (830 → 861); `LaTeXTranslator` (frozen `Visitor`) + `LatexBuilder` + escape layer; self-contained compilable preamble + vendored Sphinx texinputs; CLI `-b latex`/`-b pdf`; `ARCHITECTURE.md` §29; frozen AST/Env/Domain shapes untouched; no commits |
| 24 | EPUB3, Manpage, Texinfo & Plaintext Builders | DONE | check/clippy clean; **895 tests green × 3 consecutive runs**; groff/makeinfo/zip/roxmltree gates (see below) | 34 new tests (861 → 895); CLI `-b epub/epub3/man/texinfo/text`; ARCHITECTURE §30; frozen AST/Env/Domain shapes untouched; no commits |
| 25 | Linkcheck, XML, Pseudoxml & Changes Builders | DONE | check/clippy clean; **920 tests green × 3 consecutive runs**; byte-exact oracle differentials (see below) | 25 new tests (895 → 920); CLI `-b linkcheck/xml/pseudoxml/changes`; ARCHITECTURE §31; frozen AST/Env/Domain shapes untouched; no commits |
| 26 | Internationalization (gettext & .mo Pipeline) | DONE | check/clippy clean; **963 tests green × 3 consecutive runs**; byte-exact oracle POT differential + Babel→scribe localized-HTML round trip (see below) | 43 new tests (920 → 963); `gettext` builder CLI dispatch; PO/POT emitter + GNU MO reader/writer (no Babel at runtime); i18n seams activated; `ARCHITECTURE.md` §32; frozen AST/BuildEnvironment/Domain shapes untouched; no commits |
| 27 | Third-Party Python & Wasm Extension Bridge | DONE | check/clippy clean (default + `wasm-plugins`); **998 tests green × 3 consecutive runs**; real `scribe build` runs a Python fixture (see below) | 35 new tests (963 → 998); event bus + typed payloads + priority/dispatch/containment, Wasm ABI + feature-gated host, JSON-RPC Python shim, `InteropBridge` wired into HTML + shared output pipeline; frozen AST/BuildEnvironment/Domain shapes untouched; `ARCHITECTURE.md` §33; no commits |
| 28 | Live-Reload Dev Server & Watch Mode | DONE | check/clippy clean; **1032 tests green × 3 consecutive runs**; 10 real-subprocess HTTP/SSE/rebuild integration tests + differential (see below) | `notify` watcher + debounce, Phase 08 `BuildEnvironment::update` rebuild-set gating, embedded tokio HTTP server (MIME/index/404/health/SSE), HTML-only live-reload injection, `scribe serve`/`watch` alias, `--no-reload`, graceful Ctrl-C; `ARCHITECTURE.md` §34; frozen AST/Env/Domain shapes untouched; no commits |
| 29 | Upstream Test Suite Conformance & Fuzz Hardening | DONE | check/clippy clean; **1055 tests green × 3 consecutive runs**; conformance harness + regression guard green (see below) | **65.6 % semantic parity** over 34 applicable fixtures (3 pass / 25 partial / 6 fail / 5 skip); 39-fixture selection; **250,000 fuzz cases** (25 properties × 10,000); 1 real bug found & fixed (C++ empty-head alias panic); `CONFORMANCE.md` + committed baseline; `ARCHITECTURE.md` §35; no commits |
| 30 | Performance, Packaging, Docs & Release Readiness | DONE | check/clippy clean; test ×3 + release green; self-host test + bench harness (see below) | Self-host: 155 pages, exit 0 with `-D html_theme=basic` (exit 1 on unmodified `sphinx13` theme); measured 5.25× wall speedup / 1.86× less RSS @40 pages, 6.38× / 1.26× more RSS @150 pages (see `BENCHMARKS.md`); 1056 tests; 65.6% parity; 250k fuzz cases; final ledger below |
| 31 | Production `quickstart` / `clean` / `check` subcommands | DONE | check/clippy clean; `cargo test --workspace` **1073 passed × 3 consecutive**; 9 new real-binary CLI e2e tests (see below) | Replaced all three Phase 01 placeholders: `clean` with path-safety/idempotency/`--dry-run`, `check` reusing config/discovery/toctree/xref APIs with `-W` semantics, `quickstart` with interactive + `--batch` scaffolding; 148 files remain uncommitted, protected by `stash@{0}: pre-phase-31-snapshot`; **no commits created** per user instruction; frozen AST/Env/Domain contracts untouched |
| 32 | Streaming/arena-scoped build lifecycle; linear peak RSS | DONE | check/clippy clean; `cargo test --workspace` **1073 passed × 3 consecutive** + release 1073; self-host + conformance + regression guard green (see below) | Profiled with new `SCRIBE_PROFILE=1` RSS sampler; removed O(pages²) env/config clones and duplicate AST retention in the HTML writer; @150 pages peak RSS **116,292 → 76,656 KB (−34%)** and wall speedup **6.23× → 7.23×**; both exit gates PASS; HTML counts 44/43 @40p, 154/153 @150p; self-host 0.93 s / 193,716 KB; parity unchanged at 65.6% |
| 33–34 | Warning-code taxonomy, xref & theme conformance improvements | DONE | check/clippy clean; test ×3 + release green; conformance + self-host + regression guard green | Semantic parity **65.6% → 66.7%** over 34 fixtures (3 pass / 26 partial / 5 fail / 5 skip); extended warning-code taxonomy + std-domain xref formatting + bundled theme parity; 1,106 tests |
| 35 | External HTML theme resolution (unmodified `sphinx/doc` self-host) | DONE | check/clippy clean; test ×3 + release green; unmodified self-host build exit 0 | `sphinx13` external theme resolved via `html_theme_path` + site-packages discovery; both unmodified and `-D html_theme=basic` builds exit 0 with 158 HTML pages |
| 36 | Final conformance push, 250k fuzz sweep & verified docs | PARTIAL | check/clippy clean; test ×3 + release green (1,108); 270,000 fuzz cases; self-host + conformance + regression guard + benchmarks green | Parity **66.7% → 70.8%** (4 pass / 26 partial / 4 fail / 5 skip) — **below the ≥80% gate**; see §Phase 36 below for the honest blocker report. @40p 5.42× / 35,296 KB; @150p 7.00× / 76,824 KB (< 92,496 KB Python). **No commits created** |

## Frozen cross-phase contracts

Defined in `ARCHITECTURE.md` §5 (full Phase 03 shapes in §7). Phase 01 intentionally defined
**none** of these types, so no premature shape was baked in.

- **Phase 03: FROZEN.** `scribe-ast::Document` node enum (`NodeKind`) + `u32` arena `NodeId`;
  `pending_xref` typed shape (`PendingXref`/`PendingXrefMut`). Documented exhaustively in §7.
- **Phase 08: FROZEN.** `BuildEnvironment` public API + `UpdateOptions` / `UpdateReport` +
  `scribe-cache` format. Documented exhaustively in `ARCHITECTURE.md` §12.
- **Phase 11: FROZEN.** `Domain` trait (`resolve_xref` / `resolve_any_xref`),
  `DomainRegistry`, owned `ResolvedReference` / `ObjectEntry` / `DomainData`,
  `DomainEnv` adapter and the additive env domain-data side table. Documented
  exhaustively in `ARCHITECTURE.md` §15.

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

## Phase 03 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **66 passed, 0 failed** (29 scribe-ast: 23 unit + 3 pseudo-XML + 3 round-trip; 36 from Phases 01–02; 1 doctest) |
| `cargo test -p scribe-ast --test pseudoxml -- --nocapture` | 3 passed: hardcoded snapshot, `Python pformat` differential, real `sphinx-build -b pseudoxml` differential |
| `.oracle-venv/bin/python crates/scribe-ast/tests/fixtures/gen_pseudoxml.py` | Builds canonical tree with `docutils.nodes` + `sphinx.addnodes`; prints `document.pformat()` identical to scribe `to_pseudoxml()` |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q <tiny-src> <out>` | Produced `index.pseudoxml` (section, emphasis/literal paragraph, bullet list, note, plus an injected unresolved `pending_xref`); parsed → AST → re-emitted byte-for-byte equal (incl. `xmlns:*`, `translation_progress`, `names="a\ section"` escaping) |

**Files changed:** `Cargo.toml` (workspace `scribe-ast` dep slot), `crates/scribe-ast/Cargo.toml`
(thiserror/serde/serde_json), `crates/scribe-ast/src/{lib,error,node_id,node,document,visitor,mutator,serialize}.rs`,
`crates/scribe-ast/tests/{pseudoxml,roundtrip}.rs`, `crates/scribe-ast/tests/common/mod.rs`,
`crates/scribe-ast/tests/fixtures/gen_pseudoxml.py`, `ARCHITECTURE.md` (§1, §5, new §7), `STATUS.md`.

**Frozen contract declared:** `ARCHITECTURE.md` §7 documents the exact public shapes of
`Document`, `NodeId`, `Node`/`NodeKind`, `Attributes`/`AttributeValue`, `SourcePos`/`Span`,
`Visitor`/`Mutator`, and the `pending_xref` typed views (`PendingXref`/`PendingXrefMut`).

## Phase 04 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **137 passed, 0 failed** (66 from Phases 01–03 + 29 scribe-rst unit + 27 block + 9 diagnostics + 5 differential + 1 doc-test) |
| `cargo test -p scribe-rst --test blocks` | 27 passed: section hierarchy/slugs, overline sections, duplicate ids, nested lists + tightness, enumerated formats/start, definition lists + classifiers, field lists, option lists, line blocks, block quotes + attribution, literal blocks, doctests, transitions, grid tables (head + spans), simple tables, comments/targets/substitutions, `Pending` directive attributes, CRLF/tabs, prolog/epilog injection, spans, empty doc |
| `cargo test -p scribe-rst --test diagnostics` | 9 passed: indentation/adornment/table/explicit-markup/missing-body diagnostics with byte spans, miette help labels and rendered line:column; best-effort document recovery; 14 fuzz-like inputs without panic |
| `cargo test -p scribe-rst --test differential -- --nocapture` | 5 passed: four real `sphinx-build -b pseudoxml` parity fixtures (sections, lists, quotes/literals/doctests, grid+simple tables) plus harness self-check; all byte-equal after documented normalization |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q <tiny-src> <out>` | Confirmed normalized block structure parity for all four fixtures; normalized-away set: `document.source`, `translation_progress`, `xmlns:*`, `literal_block.force/language/linenos`, `doctest_block.classes="doctest"` |

**Files changed:** `Cargo.toml` (workspace `scribe-rst` dep slot), `crates/scribe-rst/Cargo.toml`
(thiserror, miette, serde, serde_json, once_cell, regex, scribe-ast),
`crates/scribe-rst/src/{lib,error,lexer,names,parser,lists,tables,explicit,pending,inline_seam}.rs`,
`crates/scribe-rst/tests/{common/mod,blocks,diagnostics,differential}.rs`,
`ARCHITECTURE.md` (new §8), `STATUS.md`.

**Frozen contract untouched:** `scribe-ast` was not modified. Phase 04 consumes the
§7 `Document`/`NodeKind`/`NodeId` API read-only apart from normal node construction.
The inline seam (`parse_inline_placeholder`/`InlineContext`) and the `Pending`
attribute contract (`directive_name`, `arguments`, `options` as JSON, `content`,
`directive_line`) are documented in `ARCHITECTURE.md` §8.4/§8.5 for Phases 05/06.

## Phase 05 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **177 passed, 0 failed** (137 from Phases 01–04 + 9 scribe-rst inline unit + 22 inline integration + 6 inline differential + 5 proptest properties + doctests) |
| `cargo test -p scribe-rst --test inline` | 22 passed: emphasis/strong boundary + quoted-start rules, inline literals (embedded backticks, backslash restore), escapes, exact local spans, direct/xref roles, `~`/`.` modifiers, explicit-title + side-table contract, unknown/suffix roles, named/anonymous/phrase/embedded-link/standalone references, substitutions, footnotes/citations, inline in titles/terms/fields/lines/table cells, prolog/epilog injection, in-bounds spans |
| `cargo test -p scribe-rst --test inline_differential -- --nocapture` | 6 passed: 4 byte-exact oracle fixtures (emphasis/literals/escapes; Docutils roles; hyperlink refs; substitutions), 1 footnote/citation reference-profile parity, 1 Sphinx xref target/title parity |
| `cargo test -p scribe-rst --test fuzz -- --nocapture` | 5 passed, `ProptestConfig::with_cases(1024)` each = **5,120 generated cases**; no panics, all spans in bounds/ordered; also exercises full `parse()` + pseudo-XML emission |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q <fixture> <out>` | Byte-exact normalized parity for emphasis, strong, literals, escapes, Docutils roles, standalone URI/email, named/anonymous/phrase references with targets and substitution expansion; normalized-away set inherited from §8.6 plus frozen-AST `title_reference` → `Inline(classes="title-reference")` mapping |

**Files changed:** `Cargo.toml` (workspace `proptest` dep), `crates/scribe-rst/Cargo.toml`
(proptest dev-dep), `crates/scribe-rst/src/{inline.rs (new), inline_seam.rs, lib.rs, parser.rs,
lists.rs, explicit.rs}`, `crates/scribe-rst/tests/{inline.rs (new), inline_differential.rs (new),
fuzz.rs (new)}`, `ARCHITECTURE.md` (new §9, revised §8.4), `STATUS.md`.

**Frozen contract untouched:** `scribe-ast` was not modified. Phase 05 consumes
the §7 `Document`/`NodeKind`/`NodeId` API read-only apart from normal node construction.
`pending_xref` is populated via the frozen typed views; the xref payload additions live
in a side table on `ParseOutput` so no pseudo-XML attribute was invented.

**Fuzz findings fixed:** the adversarial run exposed and fixed a pre-existing char-boundary
panic in Phase 04's `parse_option_string` (non-ASCII short option), an inline-slice boundary
issue on multi-line substitutions, and tab-expanded line spans extending past the source;
all are now covered by the 5,120-case regression suite.

## Phase 06 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **216 passed, 0 failed** (177 from Phases 01–05 + 17 scribe-rst lib unit + 22 directives + 5 roles + 2 directive differential + 2 additional differential) |
| `cargo test -p scribe-rst --test directives` | 22 passed: registry coverage, admonitions/version-modified/body/parts, include line clipping + injectable resolver, `only` tags, nested directives, unknown-directive span diagnostics, option errors, substitutions, footnotes/citations, csv/list tables, math/figure/raw/code placeholders |
| `cargo test -p scribe-rst --test roles` | 5 passed: registry classification, semantic roles, PEP/RFC external refs, `pending_xref` + `XrefMeta` side-table contract, `math`/`eq` |
| `cargo test -p scribe-rst --test directive_differential -- --nocapture` | 2 passed: 18 directives + 4 roles and 4 build-state directives byte-exact after documented normalization |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q -t draft <fixture> <out>` | Confirmed parity for admonitions, changeset, pseudo-sections, containers, images/figures, csv/list tables, glossary, hlist, productionlist, include/raw/only and the abbr/kbd/math/regexp roles |

**Files changed:** `crates/scribe-rst/Cargo.toml` (`scribe-config` dep),
`crates/scribe-rst/src/{lib,error,explicit,parser,inline,names}.rs` (additive side
tables, blank-prefix directive bodies and role classification through the new
registry), new `crates/scribe-rst/src/{option_spec,directive_registry,directives,
role_registry,resolve}.rs`, new `crates/scribe-rst/tests/{directives,roles,
directive_differential}.rs`, `ARCHITECTURE.md` (new §10), `STATUS.md`.
Frozen `scribe-ast` was not modified.

**Directive coverage (55 registered):** `admonition`, `note`, `warning`, `tip`,
`important`, `caution`, `danger`, `error`, `hint`, `attention`, `seealso`,
`versionadded`, `versionchanged`, `deprecated`, `versionremoved`, `rubric`,
`epigraph`, `highlights`, `pull-quote`, `compound`, `container`, `topic`,
`sidebar`, `contents`, `sectnum`, `include`, `raw`, `replace`, `unicode`, `date`,
`class`, `rst-class`, `cssclass`, `role`, `default-role`, `title`, `meta`,
`math`, `csv-table`, `list-table`, `image`, `figure`, `only`, `tabularcolumns`,
`centered`, `acks`, `hlist`, `glossary`, `productionlist`, `footnote`,
`citation`, `code`, `code-block`, `sourcecode`, `literalinclude`.

**Role coverage (70 registered):** all Phase 06 generic roles
(`ref`, `doc`, `download`, `numref`, `envvar`, `option`, `token`, `keyword`,
`term`, `abbr`, `command`, `dfn`, `file`, `guilabel`, `kbd`, `mailheader`,
`makevar`, `manpage`, `menuselection`, `mimetype`, `newsgroup`, `program`,
`regexp`, `samp`, `pep`, `rfc`, `math`, `eq`) plus Docutils direct roles and
fully-qualified Std/Py/C/C++/JS/RST xref aliases. Domain-qualified roles are
generic xref emitters until Phases 11–14.

## Phase 07 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` | **287 passed, 0 failed** (216 prior + 71 new MyST tests) |
| `cargo test -p scribe-myst` | 13 lib unit + 15 CommonMark + 17 MyST-extension + 16 RST↔MyST equivalence + 2 mixed-project + 7 adversarial (5 `proptest` properties × 512 cases = 2,560 generated) + 1 doctest |
| `cargo test -p scribe-myst --test mixed_project` | 2 passed: `.rst` `:ref:\`myst-target\`` ⇄ `.md` `{ref}\`rst-target\`` + `(myst-target)=` — cross-file `reftarget`s match the other file's labels/targets at AST level |
| `cargo test -p scribe-myst --test equivalence` | 16 passed: headings, sections, paragraphs/inline, lists, deflists, transitions, quotes, math/literal/xref/domain roles, directives before & after shared-registry resolution |

**Files changed:** `Cargo.toml` (`pulldown-cmark`, `serde_yaml` in
`[workspace.dependencies]`), `Cargo.lock`, `crates/scribe-myst/Cargo.toml`,
new `crates/scribe-myst/src/{error,names,frontmatter,preprocessor,directives,
roles,lower}.rs` and rewritten `lib.rs`, new
`crates/scribe-myst/tests/{common,commonmark,myst,equivalence,mixed_project,
adversarial}.rs`, `ARCHITECTURE.md` (new §11), `STATUS.md`.
Frozen `scribe-ast` was not modified. No commits created.

**Approach:** streaming `pulldown-cmark` events lowered straight into the
frozen AST with exact byte spans; MyST colon fences rewritten to backticks in a
byte-length-preserving pass; fenced directives emit the Phase 06 `Pending`
contract and resolve through the shared RST registry (including nested
directives, via an RST-content conversion of the payload); roles emit the
Phase 05 concrete/`PendingXref` + `XrefMeta` shape. MyST-Parser is absent from
the oracle, so equivalence is proven against RST-equivalent sources (documented
in §11.1). Final cross-file URI/domain resolution remains Phase 08/11–14.

## Phase 05–07 audit remediation — inline synthesised-span flake

**Failure found during orchestrator audit:** `cargo test -p scribe-rst --test fuzz`
failed intermittently in `pure_recogniser_never_panics_and_spans_are_valid`:

```
child span Span { start: 0, end: 0 } outside parent Span { start: 3, end: 7 }
minimal failing input: text = "||[````]"
```

**Root cause:** in `crates/scribe-rst/src/inline.rs`, `span_of(src, tokens, a, b)`
handles an empty range (`a >= b`, e.g. a synthesised text child) by falling back
to `tokens.first().start` — the start of the *whole* token slice, i.e. global
byte 0 for a top-level `InlineSource::single(&text, 0)`. For `"||[````]"` the
inline literal spans `[3, 7)` (the four backticks) but its closing `` begins at
`body`, leaving `body == end`; the empty text child was therefore assigned the
default `Span { 0, 0 }`, which lies outside the parent and breaks the
child-containment invariant. It only surfaced under proptest's random seeds
because the property generated such overlapping-delimiter strings rarely.

**Fix (durable, not seed-specific):** `span_of` now anchors an empty range at the
construct's own token offset — `tokens[a].start` when `a` is in bounds, else the
last token's end — and only falls back to byte 0 for a genuinely empty token
slice. The resulting zero-width span is always a valid position inside the
parent. No other phase logic changed; frozen `scribe-ast` was untouched; no
`.unwrap()`/`.expect()` was added to production code.

**Regression tests added (property not weakened; cases kept at 1024, plus a
nonzero-base re-parse of every generated case):**
- `crates/scribe-rst/tests/fuzz.rs::regression_exact_failing_input_child_span_is_contained`
  — pins `"||[````]"`, asserts the literal is `Span { 3, 7 }` and its sole child
  is `Span { 5, 5 }` inside the parent.
- `crates/scribe-rst/tests/fuzz.rs::regression_empty_synthesised_child_spans_are_contained`
  — 24 adversarial cases (empty/whitespace payloads, unbalanced/adjacent
  backticks and brackets, empty footnote labels, empty/short role payloads,
  phrase refs with empty title/target) checked at base 0 and base 97.
- `crates/scribe-rst/tests/fuzz.rs::pure_recogniser_never_panics_and_spans_are_valid`
  — strengthened to re-parse every generated string at base 97 and re-check
  containment with `source_len = 97 + len`, so a leaked global-zero span cannot
  hide.
- `crates/scribe-rst/src/inline.rs::empty_literal_child_span_is_clamped_to_parent`
  — unit test at base 100 asserting the child is clamped inside the literal.
- `ARCHITECTURE.md` §9.3 now documents the synthesised-empty-span invariant;
  §9.5 records the new regressions.

**Repeated gate evidence (from `/home/credo/code/scribe`, all 0 failed):**

| Command | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 |
|---|---|---|---|---|---|
| `cargo test --workspace` | 290 passed | 290 passed | 290 passed | 290 passed | 290 passed |
| `cargo test -p scribe-rst --test fuzz` | 7 passed | 7 passed | 7 passed | 7 passed | 7 passed |

(Previous workspace total was 287; the three new regression tests bring it to 290. The fuzz suite went from 5 to 7 tests.)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `PROPTEST_CASES=4096 cargo test -p scribe-rst --test fuzz` | **7 passed, 0 failed** (4× cases, 14.6s) |

No commits created — changes left uncommitted per user request.

## Phase 05 span-audit remediation #2 — Unicode byte/token confusion & non-monotonic offsets

**Failures observed (second span bug):**
1. `child Span { start: 1, end: 15 } outside parent Span { start: 1, end: 14 }` for the
   minimal input ``text = "`———<>`_}"`` at high case counts. The em-dashes are
   3 bytes each but one token each.
2. The new materialisation invariant caught
   `inline child span Span { start: 19, end: 19 } outside parent Span { start: 18, end: 18 }`
   on a tab-expanded prolog/epilog input (`_||||||||*\t`!\n`\t******`).

**Root cause 1 — byte length used as a token index.** In `emit_phrase_reference`
the explicit-title branch computed the title child's exclusive end as
`body + raw[..lt].trim_end().len()`. `raw` is the Drop-mode *rendered* string of
`tokens[body..close_tick]`: a multi-byte character contributes several bytes but
exactly one token, and escaped characters lose their backslash. The byte length
therefore overshot the token index; `span_of` clamped the overshoot to the last
token of the construct, yielding a child span past the `Reference` parent. ASCII
titles masked the bug (bytes == tokens), so a seed with em-dashes was required
to expose it. The dead `raw.as_bytes()[lt-1] == b'\\'` escape check also never
fired in Drop-mode.

**Root cause 2 — non-monotonic absolute-offset table.** `InlineSource::from_lines`
maps each line's bytes at its original `abs_start`. A tab-expanded line can be
longer than its original source slice, so the following line's offsets can start
*lower* than the previous line's end; the table jumps backwards. `abs_span` read
only the two endpoint offsets and collapsed such a range to a point, and parent
and child token ranges collapsed to *different* points, breaking containment.

**Systematic fix (all in `crates/scribe-rst/src/inline.rs`):**
- `emit_phrase_reference` now locates `<`/`>` delimiters at the **token** level
  (final unescaped `>`, last preceding unescaped `<` — matching docutils'
  `escape2null`), builds title/target strings from token ranges, and derives the
  title span's exclusive end from **tokens only** (trailing whitespace trimmed
  at token level). No byte length is ever used as a token index.
- `InlineSource::abs_span` now takes the **min/max over the whole covered table
  range** instead of just the endpoints. Identical for monotonic tables; for
  non-monotonic (tab-expanded) tables this makes containment structural: if
  `[a,b) ⊆ [c,d)` as token ranges then `abs_span(a,b) ⊆ abs_span(c,d)`.
- New `checked_child_span(parent, child)` debug-only invariant invoked for every
  child in `materialize_one`, so any future span regression trips at the source
  in debug builds rather than only in property tests. No `.unwrap()`/`.expect()`
  added to production; release builds unaffected.
- Audited every `span_of` / `span_from_local` call site: all remaining callers
  pass token indices or clamped byte-local offsets and are containment-safe.

**Regression tests added (`crates/scribe-rst/tests/fuzz.rs`):**
- `regression_unicode_title_byte_length_is_not_a_token_index` — pins the exact
  failing input at bases 0/97/1000 and asserts reference `Span(base+1, base+14)`
  with title child `Span(base+1, base+10)`.
- `regression_multibyte_explicit_title_corpus` — em-dash, CJK, accented Latin,
  combining marks, emoji titles with explicit `<target>` links and xref roles
  (`:ref:`, `:py:func:` with `~` modifiers) at bases 0/97/1000/65000.
- `regression_non_monotonic_offset_table_spans_are_contained` — a hand-built
  non-monotonic offset table (as produced by tab expansion).
- `regression_prolog_epilog_tab_expansion_spans_in_bounds` — full-document parse
  of the input that tripped the materialisation invariant.
- Two new proptest properties: `unicode_phrase_references_have_contained_spans`
  and `unicode_xref_roles_have_contained_spans` (each checked at bases 0/97/1000).
- `markup_string` alphabet extended with em-dash variants, CJK (中/日), combining
  acute, emoji and fullwidth punctuation so multi-byte cases are generated often.
- Property assertion unchanged; cases kept at 1024; persisted seed file
  `crates/scribe-rst/tests/fuzz.proptest-regressions` retained (now 5 seeds).

**Stress evidence (from `/home/credo/code/scribe`, all green):**

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | Finished, 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` × 3 consecutive | 338 passed, 0 failed each run |
| `cargo test -p scribe-rst --test fuzz` × 5 consecutive | 13 passed, 0 failed each run (7 → 13 tests) |
| `PROPTEST_CASES=8192 cargo test -p scribe-rst --test fuzz` | 13 passed, 0 failed (27.5 s) |
| `PROPTEST_CASES=16384 cargo test -p scribe-rst --test fuzz` | 13 passed, 0 failed (55.2 s) |
| `PROPTEST_CASES=8192 cargo test --workspace` | 338 passed, 0 failed |

Frozen `scribe-ast` untouched. No commits created — changes left uncommitted per
user request.

## Phase 08 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **332 passed, 0 failed** on runs 1, 2 and 3 (329 library/integration + 3 doctests) |
| `cargo test -p scribe-cache` | 5 unit + 4 integration: deterministic bytes, corrupt/version-mismatch → cold, atomic save/load, key sanitization |
| `cargo test -p scribe-core --test discovery` | 8 passed: `test-basic`, `test-toctree` (7 docs, sorted), `test-root` (`.txt`/`.foo`, excludes, `_templates`), glob semantics, `GlobSpec` API |
| `cargo test -p scribe-core --test incremental` | 15 passed: no-change skip, single edit, included-file invalidation, config relevance, add/remove, toctree edges, transitive closure, `-E`/`-a`, corrupt cache, save/load parity, doctree round-trip, extension seam, `reread_always`, missing doctree |
| `cargo test -p scribe-core --test differential` | 2 passed: real oracle `-E` full read → unchanged empty → edit `other.rst` reports `{"other"}` on both sides → settled empty |
| `./target/debug/scribe build <test-basic>/src <out>` | 1st: `1 added`; 2nd: `no documents need rebuilding`; `-E`/`-a`: `1 added`; then Phase 01 `scribe::unimplemented_builder`, exit 1, no panic |

**New files:** `crates/scribe-cache/src/{error,digest,store}.rs`, `crates/scribe-cache/tests/cache.rs`,
`crates/scribe-core/src/{glob,discovery,env}.rs`,
`crates/scribe-core/tests/{common,discovery,incremental,differential}.rs`.
**Modified:** workspace `Cargo.toml` (new `scribe-cache`/`scribe-core` workspace deps),
`crates/scribe-{cache,core,cli}/Cargo.toml`, `crates/scribe-core/src/lib.rs`,
`crates/scribe-cli/src/lib.rs`, `ARCHITECTURE.md` (§12 + frozen-contract update), `STATUS.md`.
Frozen `scribe-ast` untouched. No commits created.

**Stability remediation (audit):** while running the 3× gate, the pre-existing
`scribe-rst` fuzz property `prolog_epilog_augmentation_never_panics` hit an
`index out of bounds` at `crates/scribe-rst/src/inline.rs:320` (a recogniser can pass an
upper index past the token slice). Fixed by clamping both indices in `span_of` and `text_of`
(no change for in-bounds calls, so all parser differentials stay byte-identical); 5 repeated
`cargo test -p scribe-rst --test fuzz` runs then passed. The 3 consecutive full-suite runs
above were performed after this fix.

**Risks / deferred:** Phase 09 populated the toctree edge maps (§13); Phases 11–14 must write
xref/anchor metadata via `record_document()`; Phase 27 must feed `UpdateOptions::extra_outdated`;
Phase 30 can derive `DEFAULT_PARSER_VERSION` from crate versions. `Config` is not serde, so
only the `RelevantConfig` subset is persisted (unrelated settings intentionally do not
invalidate doctrees, matching upstream `CONFIG_OK`). MyST discovery works but has no oracle
(sphinx checkout lacks MyST-Parser), so the differential test uses RST only.

## Phase 09 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **364 passed, 0 failed** on runs 1, 2 and 3 (361 library/integration + 3 doctests) |
| `cargo test -p scribe-rst --test toctree` | 1 passed: all options, entry kinds, exact byte span/line, external/self `reference` children |
| `cargo test -p scribe-core --test toctree` | 15 passed: per-option decoding, glob + excludes, reversed-after-expand, empty glob, cycles with miette diagnostic, missing/duplicate entries, unreachable docs, local ToC, hierarchical numbering, sectnum placeholders, numfig, global ToC hidden handling, invalidation |
| `cargo test -p scribe-core --test toctree_differential` | 4 passed: `test-toctree`, `test-toctree-glob`, `test-toctree-maxdepth`, `test-toctree-multiple-parents` — exact oracle equality of `env.toctree_includes`, `env.toc_secnumbers`, `env.collect_relations()` and HTML prev/next links |
| `cargo test -p scribe-rst --test directives` | 22 passed (Phase 06 directive regressions, including new zero-argument body recovery) |

**New files:** `crates/scribe-rst/src/toctree.rs`,
`crates/scribe-rst/tests/toctree.rs`, `crates/scribe-core/src/toctree.rs`,
`crates/scribe-core/tests/toctree.rs`, `crates/scribe-core/tests/toctree_differential.rs`.
**Modified:** `crates/scribe-rst/src/{lib.rs,directives.rs,explicit.rs,pending.rs,resolve.rs,parser.rs}`
(additive `body_lines` seam for zero-argument directives, 3-char overline
sections, `sectnum` marker), `crates/scribe-core/src/{lib.rs,env.rs}`
(`toctree` module exports; parser generation bumped to `directives=56`),
`crates/scribe-core/Cargo.toml` (miette + scribe-rst dev-dep),
`ARCHITECTURE.md` (§13 + §12.7 update), `STATUS.md`.
Frozen `scribe-ast` and frozen `BuildEnvironment` public shapes untouched. No commits created.

**Differential approach:** real `test-toctree*` fixtures are copied to temp
dirs; `sphinx-build -b html -E -q` supplies prev/next links (relative hrefs
normalized with Sphinx's `docname_join`), and a pinned Sphinx-API query supplies
the authoritative `toctree_includes`, `toc_secnumbers` and `collect_relations()`
structures for exact comparison. Normalization is documented in
`ARCHITECTURE.md` §13.6. Four distinct fixtures are exercised, including
multi-parent, glob and numbered/maxdepth trees.

**Risks / deferred:** `:numbered:` depth follows the full hierarchy rather than
the maxdepth-pruned render tree (upstream numbering behavior); `numfig` covers
figure/table/captioned code blocks only; extension enumerable node types are a
Phase 27 seam; `only` is still eager at resolve time (differential tests enable
the `html` tag); section-anchor toctree entries and `titlesonly` pruning belong
to the writer phases.

## Phase 10 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **391 passed, 0 failed** on runs 1, 2 and 3 (388 library/integration + 3 doctests) |
| `cargo test -p scribe-core --test transforms` | 17 passed: priority ordering (scrambled registration + name tie-break), enable/disable/report, canonical priorities, all read transforms, nested `only` filtering, footnote/citation numbering + backlinks, smart quotes (language + excluded nodes), glossary NFD sort, target propagation, malformed-document no-panic |
| `cargo test -p scribe-core --test transform_differential` | 4 passed: real `sphinx-build -b pseudoxml` byte-exact normalization for smart quotes, default substitutions and footnote numbering; direct `process_only_nodes` oracle comparison for the `only` post-transform |
| `cargo test -p scribe-core --lib` | 6 new unit tests (4 smart-quote education, 1 canonical priorities, 1 symbol-index math) alongside 11 prior core unit tests |

**New files:** `crates/scribe-core/src/{transforms.rs, post_transforms.rs, smartquotes.rs,
transforms/footnotes.rs}`, `crates/scribe-core/tests/{transforms.rs,
transform_differential.rs}`.
**Modified:** `crates/scribe-core/src/lib.rs` (module + API re-exports),
`ARCHITECTURE.md` (status line, crate responsibility, new §14), `STATUS.md`.
Frozen `scribe-ast` and frozen `BuildEnvironment` public shapes untouched. No
commits created.

**Differential approach:** real oracle builds for three transform families
(smart quotes, default substitutions, footnotes) plus a direct Python-API call
to `process_only_nodes` for the post-transform. Normalization (documented in
`ARCHITECTURE.md` §14.7): strip the `<document>` attribute list, Sphinx-only
`docname`, `xml:space`, and rename generated `idN` values by first appearance
so cross-links stay comparable. Node kinds, nesting, all other attributes and
text are compared verbatim.

**Risks / deferred:** citation *references* keep the frozen
`citation_reference` node (oracle uses `reference > inline`), so citation
label/backlink behavior is unit-tested but not byte-compared; `|translation
progress|` is a Phase 26 placeholder; the `SphinxDomains`/`ReferencesResolver`
slots are no-ops until Phase 14; i18n slots (except `ExtraTranslatableNodes`)
are no-ops until Phase 26; `sig_element_fallback` is off by default to match
Sphinx's own translator; `today` without explicit config uses a deterministic
UTC formatter rather than full locale-aware `format_date`.

## Phase 11 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **432 passed, 0 failed** on runs 1, 2 and 3 (429 library/integration + 3 doctests) |
| `cargo test -p scribe-domains --lib` | 9 unit tests (owned types, diagnostics, path helpers, std id/numref formatting, registry) |
| `cargo test -p scribe-domains --test std` | 16 tests: every std object type/role, duplicate labels, lowercase `ref`, program-option context/aliases/subcommands, numref `{number}`/`{name}` and `%s`, case-insensitive terms, `:any:` dispatch, `get_objects` ordering, `clear_doc`/`merge_domaindata`, malformed-xref no-panic, read-phase pipeline |
| `cargo test -p scribe-domains --test nitpicky` | 10 tests: exact message/code/span/help, anonlabel message, dangling messages per type, `nitpick_ignore` (domain and short type, regex, Python tuples), `suppress_warnings`, silent fallback, Phase 10 sink routing |
| `cargo test -p scribe-domains --test differential` | 5 passed: 4 generated tiny-project oracle builds (`-b pseudoxml -E -n -q`) + the oracle's own `test-domain-py-xref-warning` root; resolution facts and exact warning messages compared |
| `cargo test -p scribe-domains --test persistence` | 1 test: domain inventories survive the additive env side table + snapshot round-trip; stale versions rejected |

**New files:** `crates/scribe-domains/src/{lib,error,types,diagnostics,traits,std,registry,resolve}.rs`,
`crates/scribe-domains/tests/{common/mod,std,nitpicky,differential,persistence}.rs`.
**Modified (additive):** `crates/scribe-domains/Cargo.toml`, workspace `Cargo.toml`
(`scribe-domains` dep), `crates/scribe-core/{Cargo.toml,src/env.rs,
src/transforms.rs,src/post_transforms.rs}` (domain-data side table, `DomainEnv`
impl, `TransformContext` domain sink + registry, wired seams),
`crates/scribe-rst/src/{directives.rs,role_registry.rs}` (six std object
directives, `confval`/`label` roles), `ARCHITECTURE.md` (§5, §14, new §15) and
`STATUS.md`. Frozen §7 AST and §12 `BuildEnvironment` public field shapes are
untouched; `ENV_FORMAT_VERSION` bumped `1 → 2` for the additive snapshot field.
No commits created.

**Differential approach:** real oracle `pseudoxml` builds normalized to
resolution facts (reference tags + `refid`/`refuri`/`title`/`classes` + first
inner text line) and exact warning messages; generated ids, document attributes
and namespaces are excluded. Full normalization rules are in §15.7.

**Risks / deferred:** `:download:` uses synthesized `_downloads/<basename>`
URIs; `productionlist` does not emit internal backticked token xrefs; explicit
labels propagated onto `Section` nodes cannot be distinguished from implicit
titles in the frozen AST (use `.. label::` for those); intersphinx is deferred
to Phase 14 with the seam documented in §15.8.

## Phase 12 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **462 passed, 0 failed** on runs 1, 2 and 3 (459 library/integration + 3 doctests) |
| `cargo test -p scribe-domains --lib` | 15 unit tests (owned types, diagnostics, path helpers, std formatting, registry, Python metadata/roles, signature RE, reftarget modifiers, Optional/Union rewriting, arglist separators) |
| `cargo test -p scribe-domains --test python` | 20 tests: every signature form (positional-only `/`, keyword-only `*`, `*args`/`**kwargs`, defaults, annotations, `async`, `[T]`, decorators, qualified names), `:noindex:`/`:final:`/`:abstract:` options, typed field xrefs, `~`/`.`/`fix_parens` xref modifiers, aliasing, `:any:`, duplicate warnings, module context/nesting, `clear_doc`/`merge_domaindata`, module index, persistence, adversarial corpus |
| `cargo test -p scribe-domains --test python_differential` | 4 passed: real oracle builds of `test-domain-py`, `test-domain-py-python_use_unqualified_type_names`, `test-domain-py-xref-type-alias`, `test-domain-py-xref-warning`; signature pseudoxml structure, resolved references and exact warnings compared |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -n -q <fixture> <out>` | Byte-level doctrees captured for the four roots; normalization and reductions documented in `ARCHITECTURE.md` §16.6 |

**New files:** `crates/scribe-domains/src/python.rs`, `crates/scribe-domains/src/python/signature.rs`,
`crates/scribe-rst/src/python_directives.rs`, `crates/scribe-domains/tests/{python,python_differential}.rs`.
**Modified (additive):** `crates/scribe-domains/src/{lib,error,registry,resolve,std,traits}.rs`,
`crates/scribe-config/src/registry.rs` (four Python config keys),
`crates/scribe-rst/src/{lib,directives,explicit,role_registry}.rs` (`py:*` directives, `py:type` role,
Docutils-correct option-block/blank-line handling, domain-qualified directive names),
`crates/scribe-core/src/transforms.rs` (`prepare_document` call in the read-phase domain seam),
`ARCHITECTURE.md` (new §16) and `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment` and
§15 `Domain` trait public shapes are untouched. No commits created.

**Differential approach:** real oracle pseudoxml trees are reduced to
`desc_signature` structure (tag/text sequence plus kept attributes) and
reference resolution facts (`refid`/`refuri`/`reftitle`/`classes`/inner text);
warnings are compared verbatim. Normalized away: internal `py:*` skeleton
attributes, `_toc_*` hints, generated ids, `xml:space`, `multi_line*` flags,
`.pseudoxml` URI suffix, document attributes, and the toctree-only `index.rst`
(§16.6).

**Risks / deferred:** signature nodes carry the directive marker's span rather
than per-token byte spans (valid but coarse); multi-line signature wrapping and
`python_display_short_literal_types` are parsed but always render single-line;
field-list rendering covers the upstream `param`/`type`/`rtype`/`raises`
groupings and preserves existing role references, but exotic Docutils field
customizations are out of scope; `DomainRegistry::builtin()` now includes `py`
while `with_standard()` stays std-only for Phase 11 regressions.

## Phase 14 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **551 passed, 0 failed** on runs 1, 2 and 3 (548 library/integration + 3 doctests) |
| `cargo test -p scribe-domains --test inventory` | 6 passed: v2/v1 reader, `$` expansion, embedded spaces, case-insensitive ambiguity, corrupt input, **byte-exact writer** |
| `cargo test -p scribe-domains --test resolution` | 17 passed: local/any/intersphinx resolution, fuzzy suggestions, nitpicky stability, `:external:`, disabled reftypes, cache expiry, no-panic, rayon multi-doc, async wrapper, registry seam |
| `cargo test -p scribe-domains --test intersphinx_differential` | 3 passed: real oracle `-b pseudoxml -E -n -q` resolution facts + warnings with an oracle-generated local inventory; writer/reader fixture check |
| `.oracle-venv/bin/python crates/scribe-domains/tests/fixtures/gen_inventory.py <out>` | Reproduces upstream `InventoryFile.dump` byte-for-byte with the pinned zlib-ng; asserted equal to `Inventory::dump` |

**New files:** `crates/scribe-domains/src/intersphinx/{mod,inventory,load,cache,resolve}.rs`,
`crates/scribe-domains/src/xref.rs`,
`crates/scribe-domains/tests/{inventory,resolution,intersphinx_differential}.rs`,
`crates/scribe-domains/tests/fixtures/{gen_inventory,gen_intersphinx_v2}.py`.
**Modified (additive):** workspace `Cargo.toml` (`flate2` zlib backend, `reqwest`,
`tokio`, `url`, `strsim`), `crates/scribe-domains/Cargo.toml`, `src/{lib,resolve,registry,diagnostics}.rs`
(registry-attached intersphinx hook, report breakdown fields, `INTERSPHINX` code),
`crates/scribe-config/src/registry.rs` (`intersphinx_timeout`,
`intersphinx_resolve_self`), `ARCHITECTURE.md` (new §18), `STATUS.md`.
Frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` / `ResolvedReference`
public shapes are untouched. No commits created.

**Inventory byte-compat evidence:** our zlib-backed writer output equals the
oracle generator's bytes exactly, including the four-line escaped header,
level-9 deflate stream, `$` anchor shortening, `-` display collapse and
deterministic domain/object ordering. `flate2` must use the `zlib` backend
(system libz-ng 1.3.1) — `miniz_oxide` does not match.

**Differential approach:** the upstream remote-inventory fixtures cannot run
offline, so the two inventory files are generated locally with the pinned
oracle Python and consumed by both the real `sphinx-build` and our loader.
Resolution-fact comparison keeps `refuri`, `reftitle`, `internal`, and display
text; callable `()` suffixes are reproduced by the resolver, and the
`external ` warning prefix is normalized where the oracle source uses plain
roles (`:external:` RST parsing is a Phase 27 seam).

**Risks / deferred:** `:external:` roles are not parsed from RST text yet
(pending xrefs are annotated directly in tests); HTTP redirect target-URI
rewriting and `tls_cacerts` are not implemented; `intersphinx_resolve_self`
strips the prefix but does not re-run local resolution; fuzzy suggestions are
a scribe-only help-label enhancement (disable with
`XrefResolver::with_suggestions(false)`); `intersphinx_disabled_reftypes`
defaults to `[]` here rather than upstream's `['std:doc']`.

## Risks / open items

- **Phase 07 deferred work:** cross-file xrefs remain `pending_xref` until
  Phase 11; `include :parser: myst` dispatch still waits for a parser-dispatch
  phase (Phase 08 only owns source discovery); full `myst_dmath`/`myst_amsmath`
  delimiter ambiguity is approximated.
- **Phase 06 deferred work:** xrefs stay `pending_xref` until Phase 11;
  `contents`/`sectnum` leave transform placeholders for Phase 10; image
  `candidates`, equation numbers and footnote ids/backrefs are writer/post-
  transform data. `code-block`/`literalinclude` are `LiteralBlock` placeholders
  until Phase 15 supplies highlighting tokens. Differential normalization is
  documented in `ARCHITECTURE.md` §10.6.
- **Role redefinition seam:** `role`/`default-role` update `Context` maps, but
  Phase 05's pure inline recogniser cannot consult per-document state; a future
  stateful inline pass must consume them (documented in §10.5).
- **Phase 04 refinement (Phase 06):** directive bodies may now be separated
  from the marker by blank lines, and option-looking lines are no longer
  mistaken for implicit arguments; prior 177 tests still pass byte-for-byte.
- **Frozen AST gaps (Phase 05):** `NodeKind` has no `title_reference`; the default
  role is emitted as `Inline(classes="title-reference")` and differential tests map
  the tag back. PEP/RFC implicit references are out of Phase 05 scope.
  Footnote/citation definitions remain Phase 04 `Pending` nodes; generated
  `ids`/`backrefs`/`docname` and the `pending_xref` → resolved-reference
  replacement are Phase 06/10/11 work, so those families are compared as inline
  reference profiles rather than byte-for-byte.
- **Role registry scope:** the static role→domain table in `inline.rs` covers the
  built-in Std/Py/C/C++/JS/Math/RST roles inspected in the oracle; roles added by
  third-party extensions (Phase 27) will need table extensions. Unknown roles
  degrade to `Problematic` rather than panicking.
- **Frozen AST gaps (Phase 04):** `NodeKind` has no `Classifier` and no
  `OptionGroup`; classifiers are emitted as `Inline(classes="classifier")` and
  options attach directly to `OptionListItem`. Differential fixtures avoid
  both; writers/Phase 06 can map them to the Docutils nodes.
- **Inline/directive scope:** Phase 04 emits raw `Text` and unresolved `Pending`
  nodes by design; Phase 05/06 will expand them. Paragraphs that contain inline
  markup are therefore not yet byte-identical to oracle pseudoxml.
- **Slugification:** Docutils `make_id` is reproduced with a static Latin
  decomposition map instead of full Unicode NFKD; exotic decompositions (e.g.
  combining marks outside the mapped ranges) may differ. CJK ids collapse to a
  generated `section-N` id, as docutils drops non-ASCII after NFKD.
- **Simple tables:** East Asian double-width padding is not implemented; grid
  tables (including row/column spans) are fully faithful.
- **Tab width default:** `ParseOptions::default()` has `tab_width = 0`, which
  `parse_with_options` normalizes to Docutils' default of 8.
- **Recovery loop guard:** `parse_blocks` forces progress when a construct
  fails to consume its line, so malformed markup cannot wedge the scanner.

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
- Pseudo-XML is a *lossy* format (no attribute type tags, indentation/text ambiguity). The AST →
  pseudo-XML emitter is byte-exact against Docutils/Sphinx; the reverse (pseudo-XML → AST) test
  parser handles only the documented subset (element tags, text lines, string/bool/list attributes
  inferred from a fixed key table, text not starting with 4+ spaces). Full fidelity will come from
  the real RST/MyST parsers (Phases 04–07), not from parsing pseudo-XML.
- `NodeKind` has no third-party/custom variant (kept `Copy` for ergonomics); extension-defined nodes
  (Phase 27) will need a contract amendment approved by the orchestrator.
- `Document::fixture_document()` is a test-support builder in the library; its internal `.expect()`
  calls operate only on freshly created ids and are documented as invariants in the source.
- No git commits created — orchestrator will audit gates and commit.

## Phase 13 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **517 passed, 0 failed** on runs 1, 2 and 3 (514 library/integration + 3 doctests) |
| `cargo test -p scribe-domains --test domains13` | 22 passed: all 10 registry domains, labels, C/C++/JS declarator forms, namespace/module scope, overload buckets, `_CPPvN` ID subset, `:any:` dispatch, duplicate warnings, persistence, two adversarial corpora |
| `cargo test -p scribe-domains --test domain_differential -- --nocapture` | 6 passed: `test-domain-c` (16/16 signature parity), `test-domain-cpp` (17/19 = 89.5%), `test-domain-js` (11/11), resolution facts and exact `c:identifier` warnings |
| `.oracle-venv/bin/sphinx-build -b pseudoxml -E -n -q <fixture> <out>` | Real oracle trees captured for the three gate fixtures; normalization documented in `ARCHITECTURE.md` §17.6 |

**New files:** `crates/scribe-domains/src/{c/,cpp/,javascript,rst,math,changeset,citation,index}.rs`,
`crates/scribe-domains/src/sigbuild.rs`, `crates/scribe-rst/src/domain_directives.rs`,
`crates/scribe-domains/tests/{domains13,domain_differential}.rs`.
**Modified (additive):** `crates/scribe-domains/src/{lib,registry}.rs`,
`crates/scribe-rst/src/{lib,directives,role_registry}.rs`, `ARCHITECTURE.md`
(new §17), `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain`
trait public shapes are untouched. No commits created.

**Per-domain coverage:** `c` (10 types / 13 directives / 12 roles, declarators +
scope + identifier fallback), `cpp` (10 types / 16 directives / 13 roles,
templates/operators/overloads + `_CPPvN` IDs), `js` (6 types / 6 directives /
6 roles, module + `this` nesting), `rst` (3 types / 3 directives / 2 roles),
`math` (numref + equation numbering), `changeset`/`citation`/`index` (empty
inventories, resolvable no-ops). All eight registered in
`DomainRegistry::builtin()` alongside `std` and `py`; `:any:` dispatch verified
across C/C++/JS/RST.

**Tracked gaps (documented in `ARCHITECTURE.md` §17.6, not gate failures):**
full Itanium ABI mangling/SFINAE/`decltype`, expression-role internal token
xrefs, maximum-signature-line-length multi-line rendering, per-token signature
byte spans, and the empty `changeset`/`citation`/`index` inventories (owned by
Phases 10/25). No risks to the three differential gates.

**Risks:** C++ parity is 89.5% (2 signatures differ in visibility/angle-bracket
placement only); both still register and resolve. `c:scope`/`cpp:scope`/
`js:module` read-context attributes are additive side data on pending xrefs,
never emitted as pseudo-XML. Root `prepare_document` now dispatches all domain
passes, but each is a no-op outside its own skeletons, preserving Phase 12.

---

## Phase 15 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **605 passed, 0 failed** on runs 1, 2 and 3 (601 library/integration + 4 doctests) |
| `cargo test -p scribe-builders --test highlight_api` | 10 passed: aliases, unknown-language fallback, no-panic degenerates, Pygments token classes, line specs, linenos/emphasis, diff classes, CSS determinism, JSON round-trip, rayon order |
| `cargo test -p scribe-builders --test code_directives` | 20 passed: registration, tokens, linenos/emphasis ranges, out-of-range warning, caption wrapper, dedent (auto/explicit/warning), highlight context + threshold, unknown-language warning, force, literalinclude (lines/lineno-match/start-end/prepend-append/tab-width/pyobject/diff/no-caption), missing file, invalid options, no-panic, multi-block |
| `cargo test -p scribe-builders --test oracle_differential` | 5 passed: real `sphinx-build -b html -E -q` on `test-reST-code-block`, `test-highlight_options`, `test-directive-code` (index + options) and `test-reST-code-role`; per-character token-class stream compared after span-normalisation |

**New files:** `crates/scribe-builders/src/{highlight,pygments_css,code,error}.rs`,
`crates/scribe-builders/src/css/*.css` (16 embedded Pygments stylesheets),
`crates/scribe-builders/tests/{highlight_api,code_directives,oracle_differential}.rs`
and `tests/common/mod.rs`.
**Modified (additive):** workspace `Cargo.toml` (`syntect` with the
`default-fancy` backend), `crates/scribe-builders/Cargo.toml`,
`crates/scribe-builders/src/lib.rs`, `crates/scribe-rst/src/explicit.rs`
(Phase 04 remediation: keep body content after an option block separated by a
blank line), `ARCHITECTURE.md` (new §19), `STATUS.md`. Frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` / `ResolvedReference` public shapes are
untouched. **No commits created.**

**Differential approach:** oracle HTML highlight blocks are reduced to a
per-character canonical token-class stream (HTML stripped, entities decoded,
line-number text discarded, classes mapped to a canonical vocabulary, then
compressed into runs) and compared to the same reduction of
`HighlightedCode::pygments_html()`. This compares exact source text and every
token class while tolerating harmless span-splitting differences; an unmapped
class is reported as `other:<name>` and fails the test. Four fixtures compare
exact streams; `test-reST-code-role` compares text plus agreed classes.

**Tracked gaps (documented in `ARCHITECTURE.md` §19.7, not gate failures):**
syntect does not sub-tokenize invalid Python inside call-signature parameter
lists; Pygments' `.w` whitespace spans are not reproduced; `pycon` console
prompts are approximated with the Python grammar; `:encoding:` uses the
injected resolver's UTF-8 decoding; `:pyobject:` uses a lightweight
indentation-aware tag finder; unified diff uses LCS rather than `difflib`;
`:lineno-match:` renders no `*` marker in the pinned oracle; `__highlight_json`
and writer-convenience attributes appear in code-block pseudo-XML; inline code
roles remain Phase 05/27 scope.

**Risks:** the Pygments CSS strings are snapshots of the pinned oracle
(Pygments 2.21) and will need regeneration if the oracle is upgraded. The
highlight-language context lives on the document root and is read during the
document-order resolve pass; a future parallel/document-fragment resolve would
need an explicit per-document setting passed through `Context`.

---

## Phase 16 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **640 passed, 0 failed** on runs 1, 2 and 3 (636 library/integration + 4 doctests) |
| `cargo test -p scribe-theme --test render` | 5 passed: `basic/page.html`, `alabaster` via `page.html`, `classic`, `nature` render with no missing global/filter and with `<!DOCTYPE html>`/`<head>`/`rel="stylesheet"`/sidebar markers; CSS `.jinja` render is not HTML-escaped; missing template is a typed error |
| `cargo test -p scribe-theme --test theme` | 10 passed: bundled discovery, TOML + legacy `theme.conf` parsing, 3-level flattening, option overrides, cycle/missing-parent/malformed no-panic errors, deterministic static files |
| `cargo test -p scribe-theme --test globals` | 10 passed: `pathto`, `hasdoc`, `css_tag`/`js_tag`, `tobool`/`toint`/`todim`, `slice_index`, `accesskey`, `idgen`, `get_theme_option`, translation, autoescape |
| `cargo test -p scribe-theme --test differential` | 2 passed: 13 expressions rendered by Jinja2 3.1.6 (`.oracle-venv`) and minijinja; trimmed + `True`/`False` normalized, everything else byte-compared |

**New files:** `crates/scribe-theme/src/{lib,error,bundled,theme,context,globals,environment}.rs`,
`crates/scribe-theme/tests/{theme,globals,render,differential}.rs`, the vendored
`crates/scribe-theme/themes/{basic,classic,nature,alabaster}/` asset trees and
`crates/scribe-theme/themes/NOTICE`.
**Modified (additive):** workspace `Cargo.toml` (`minijinja` 2.x with
`builtins`/`loader`/`json`/`urlencode`), `crates/scribe-theme/Cargo.toml`,
`ARCHITECTURE.md` (new §20), `STATUS.md`. Frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` / `ResolvedReference` public shapes are
untouched. **No commits created.**

**Differential approach:** the oracle script re-implements the exact Python
bodies from `jinja2glue.py` / `html/__init__.py` / `util/osutil.py` on top of
Jinja2, so the comparison validates the Rust implementations rather than a
second guess. Both engines render the same 13 expressions; normalization is
limited to trimming whitespace and lowercasing Python's `True`/`False` to
minijinja's boolean spelling. Full-template parity is asserted structurally by
the render tests (doctype, head, stylesheets, sidebar/footer markers).

**Risks / seams for Phase 17:** `sidebar()`, `relbar()` and `toctree()` globals
return pre-rendered safe fragments that the HTML writer must construct;
`css_tag`/`js_tag` do not add `?v=<checksum>` cache-busting query strings;
translation tables are empty until Phase 26; `warning()` renders nothing and
is not yet wired to the diagnostic sink; the `!` template-prefix escape of
`SphinxFileSystemLoader` is unused by the vendored themes and not implemented.

---

## Phase 17 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **665 passed, 0 failed** on runs 1, 2 and 3 (661 library/integration + 4 doctests) |
| `cargo test -p scribe-builders --test html_translator` | 19 passed: one test per node family (sections/headings/headerlinks, inline markup, references, highlight blocks, lists, definition/field lists, admonitions, versionmodified, image/figure, footnotes, math, raw, toctree, desc signatures, targets/comments/system messages, unresolved xrefs) |
| `cargo test -p scribe-builders --test html_differential` | 6 passed: test-basic body equality against the oracle after normalization, page skeleton/assets, test-root structural invariants, link integrity, empty/missing source robustness |
| CLI smoke: `cargo run -p scribe-cli -- build -b html -E -Q <fixture> <out>` | test-basic: 1 page, 16 files; test-root: 18 HTML pages (matches oracle filename set), genindex/py-modindex/objects.inv present |

**New files:** `crates/scribe-builders/src/html/{mod,translator,builder,assets,index}.rs`,
`crates/scribe-builders/tests/{html_translator,html_differential}.rs`.
**Modified (additive):** workspace `Cargo.toml` (`scribe-theme`, `scribe-builders`
path deps), `crates/scribe-builders/Cargo.toml` (+core/domains/theme/minijinja/
blake3/url), `crates/scribe-builders/src/{lib,error}.rs`, `crates/scribe-cli`
(`Cargo.toml`, `src/lib.rs` — `BuildOptions` → builder options conversion and
`BuilderFailed` miette diagnostic), `ARCHITECTURE.md` (new §21), `STATUS.md`.
Frozen §7 AST, §12 `BuildEnvironment`, §15 `Domain`/`ResolvedReference` and the
Phase 16 theme public shapes are **untouched**. **No commits created.**

**Differential approach:** the oracle (`sphinx-build 9.3.0+/035a07c32`) and
scribe both build `test-basic` and `test-root` into temp dirs. For `test-basic`
the normalized body fragments are asserted byte-equal (normalization: strip
`?v=` hashes, un-double Phase 16's `&amp;#…`, collapse whitespace). For
`test-root` a curated structural-marker set is asserted in both outputs plus
page-name parity; link integrity and generated-file presence are asserted on
scribe's output alone. The exact normalization and intentional divergences are
recorded in `ARCHITECTURE.md` §21.7–21.8.

**Divergences:** Phase 16's minijinja double-escapes `&#8212;`/`&#187;` in the
title separator and omits Sphinx's second viewport `<meta>`; the pinned
version string in the footer is `9.3.0`. Earlier phases' parser does not emit
`toc-backref`/section-number anchors, footnote `backrefs`, or fully nested
epigraph/line-block/field-list metadata, so test-root's full byte stream is
not compared. `!layout.html` user-template overrides fall back to the bundled
manager. `search.html` is a placeholder; `searchindex.js` is Phase 18.

**Risks:** the local-TOC generator approximates Sphinx's section-number and
`idN` backref scheme, so complex nested TOCs may differ in anchor ids.
Cross-document xref resolution depends on all domain objects being registered
before the transform pass; a future incremental writer that rebuilds only one
page must seed the registry from the environment's domain data (already done
via `DomainRegistry::load_from_env`). `objects.inv` priorities mirror the
domain entries directly and are not re-derived from Sphinx's object inventory
ranking rules.

---

## Phase 18 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **680 passed, 0 failed** on runs 1, 2 and 3 (675 library/integration + 5 doctests); the prior 665 tests all stay green |
| `cargo test -p scribe-builders --test search_index` | 15 passed: 8 unit (tokenizer Unicode/punctuation/camelCase, Snowball stemming, 174-word stop list, title/body weighting, deterministic bytes, escaping, language fallback, empty/non-ASCII), 4 query simulation (known term→doc/title, `running`↔`run`, stop words, snippet), 3 differential/UI (test-basic exact, test-root normalized, search.html asset loading) |
| CLI smoke: `cargo run -q -p scribe-cli -- build -b html -E sphinx/tests/roots/test-basic /tmp/scribe-basic-18` | 1 page, **19 files** (was 16 in Phase 17; +`searchindex.js`, +2 stemmer assets); `searchindex.js` parses as JSON with Sphinx's 11 keys |

**New files:** `crates/scribe-builders/src/search.rs`,
`crates/scribe-builders/src/html/js/{base-stemmer.js,english-stemmer.js,README.md}`,
`crates/scribe-builders/tests/search_index.rs`.
**Modified (additive):** workspace `Cargo.toml` (`rust-stemmers` workspace dep),
`crates/scribe-builders/Cargo.toml` (`rust-stemmers`, `regex`),
`crates/scribe-builders/src/lib.rs` (module + re-exports),
`crates/scribe-builders/src/html/{builder,assets,index}.rs` (index emission,
`language_data.js` variables, functional `search.html`),
`ARCHITECTURE.md` (new §22), `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment`,
§15 `Domain`/`ObjectEntry` and the Phase 16 theme public shapes are **untouched**.
**No commits created.**

**Differential approach:** both engines build `test-basic` and `test-root`
(`sphinx-build -b html -E -q` vs `StandaloneHtmlBuilder`) into temp dirs, and
both `searchindex.js` files are parsed with `parse_js_index`. For `test-basic`
every field except `envversion` is asserted **exactly equal** to the oracle
(`docnames`, `filenames`, `titles`, `terms`, `titleterms`, `alltitles`,
`objects`, `objtypes`, `objnames`, `indexentries`). For `test-root` the
comparison is normalized: identical key sets and docname set; exact titles for
the 11 documents whose parser output agrees; normalized `alltitles`
(title→doc-index mapping, section anchors ignored); term presence on a sample
with exact weights where doc-index sets agree. Normalization is required by
earlier-phase parser divergences (BOM retention, smart quotes, `.. title::`,
section slugs) documented in `ARCHITECTURE.md` §22.7; `envversion` values are
intentionally scribe-specific (structurally compatible map of ints).

**Query simulation:** since real browser execution is not headlessly
testable, the tests port the relevant `searchtools.js` contract (`_parseQuery`
stemming/stop-word filtering and `performTermsSearch` title=15 / term=5
scoring with all-terms-required semantics) into Rust and run it against the
emitted index. Snippet generation is validated as a 240-byte context window
around the matched keyword, as `Search.makeSearchSummary` does.

**Risks:** only English has vendored stop words; the 17 other stemmable
languages (and all others, incl. `ja`/`zh`) use identity/no-stop-word fallback,
which is faithful to scribe's Rust stemmer coverage but not to Sphinx's full
per-language stop lists. `envversion` differences mean scribe cannot load a
Sphinx-generated index for incremental rebuilds (not required by the JS
client). Exact `test-root` term→doc mappings differ from the oracle because of
parser-level token differences owned by earlier phases. The client-side stemmer
JS is a verbatim vendored binary-like asset; if Sphinx's minified stemmer is
upgraded in the oracle it should be re-vendored from `sphinx/search/minified-js/`.
Alternate HTML builders (`dirhtml`, `singlehtml`) remain Phase 19.

---

## Phase 19: DirectoryHTML, SingleHTML & Metadata Builders — DONE

**Scope delivered.** Six additional builder flavors in `crates/scribe-builders`,
all reusing Phase 17's parallel render/asset pipeline through a new
`BuildFlavor` / `build_with_flavor` seam (no second writer):

* `dirhtml` — exact upstream `get_target_uri` / `get_output_path` rules,
  directory-relative `pathto`, clean toctree/relbar/search links, per-page
  relative image/source/static paths.
* `singlehtml` — toctree-traversal-order inlining, `#document-<docname>`
  markers, duplicate-anchor renumbering, cross-document → intra-page link
  rewriting, one `index.html`, no search/index pages.
* `json` (serializinghtml) — `<docname>.fjson` page contexts,
  `globalcontext.json`, `searchindex.json`, `last_build`.
* `htmlhelp` / `qthelp` / `devhelp` — deterministic `.hhp/.hhc/.hhk/.stp`,
  `.qhp/.qhcp` and gzipped `.devhelp.gz` (+ plain `.devhelp2`) project files.
* CLI dispatch for all six; unknown builders keep `scribe::unimplemented_builder`.

**Gate evidence (commands run, from `/home/credo/code/scribe`):**

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` × 3 consecutive | **698 passed, 0 failed** on runs 1, 2 and 3 (693 library/integration + 5 doctests); all 680 prior tests stay green |
| `cargo test -p scribe-builders --test alternate_builders` | 14 passed (unit, robustness, dirhtml/singlehtml/metadata differential, determinism) |
| CLI smoke `-b dirhtml` / `-b singlehtml` | clean URL layout (`bar/index.html`, `foo/foo_1/index.html`) and a single `index.html` respectively; every internal `href` resolves, every `#anchor` resolves |

**New files:** `crates/scribe-builders/src/{dirhtml,singlehtml,metadata}.rs`,
`crates/scribe-builders/tests/alternate_builders.rs`.
**Modified (additive):** `crates/scribe-builders/Cargo.toml` (`flate2`),
`crates/scribe-builders/src/lib.rs` (module + re-exports), `html/{builder,index,assets,translator}.rs`
(`BuildFlavor`, flavor-aware generated pages, toctree list parsing, HTML5
single-`id` fix, `SingleFileContext` link rewriting), `scribe-cli/src/lib.rs`
(dispatch), `ARCHITECTURE.md` (new §25), `STATUS.md`. Frozen §7 AST, §12
`BuildEnvironment`, §15 `Domain`/`ObjectEntry` and Phase 16 theme shapes
**untouched**. **No commits created.**

**Differential approach:** oracle and scribe build the same fixtures with `-E`
into separate temp dirs. `dirhtml`: compare page-file sets and assert link
integrity on every generated page; both engines emit `bar/`, `foo/` clean links
and `../`-relative resources. `singlehtml`: assert exactly one HTML file,
`#document-*` anchors/hrefs in both engines, no `.html` link to an inlined doc,
and full anchor resolution; a custom duplicate-section fixture verifies the
renumbering. Metadata: compare emitted file names and normalized key fields
(`title`, `current_page_name`, `sourcename`, `.hhp` OPTIONS header), plus
byte-for-byte determinism across two runs. Oracle-build failures are tolerated
so the suite stays self-contained.

**Risks / intentional divergences:** scribe still emits `py-modindex` pages
where the oracle omits them when no Python modules exist; the single-file
global TOC is a flat skeleton without section numbers; `devhelp` adds a plain
`.devhelp2` alongside the oracle's `.devhelp.gz`; asset-name fix for
`alabaster.css_t` is an additive correction to a Phase 17 bug. All are recorded
in `ARCHITECTURE.md` §25.4. Earlier-phase parser differences (section numbers,
`toc-backref` ids) remain owned by their phases.

## Phase 20: Built-In Core Extensions Suite — DONE

Implemented native Rust versions of all seven extensions with **zero Python
runtime dependency** for the native code paths. Frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` shapes were not modified.

### Files changed / added

- `crates/scribe-config/src/registry.rs` — added `todo_emit_warnings`,
  `extlinks_detect_hardcoded_links`, `viewcode_*`, `graphviz_*` and `duration_*`
  config keys (idempotent via `register_config_keys`).
- `crates/scribe-config/src/lib.rs` — unknown `conf.py` assignments are now
  retained in the config namespace (with a warning) so `ifconfig` can evaluate
  fixture config values, matching upstream's full namespace.
- `crates/scribe-core/src/extensions/{mod,todo,ifconfig,extlinks,viewcode,graphviz,githubpages,duration}.rs`
  — new extension registry, activation, per-extension directives/hooks, safe
  expression evaluator, `%`-format role expansion, dot renderer, page/data
  holders and the documented Phase 27 event seam.
- `crates/scribe-core/src/lib.rs`, `Cargo.toml` — re-exports and `scribe-rst`
  promoted to a normal dependency.
- `crates/scribe-rst/src/{extlinks.rs,lib.rs,inline.rs,parser.rs,directive_registry.rs,inline_seam.rs}`
  — additive `ExtLinkSpec` seam carried through `ParseOptions`, `InlineOptions`
  and `Context`; dynamic roles emitted before built-in role classification.
- `crates/scribe-builders/src/html/builder.rs` — extension activation, parse
  options, post-read hooks, todo expansion, extlink warnings, graphviz render,
  viewcode collection after domain transforms, viewcode page emission,
  githubpages assets and duration reporting.
- `crates/scribe-builders/src/html/translator.rs` — graphviz node rendering,
  `HtmlContext::graphviz`, and corrected `skip_depth`/`skip_owner` frame
  bookkeeping so self-rendering nodes no longer mis-nest following siblings.
- `ARCHITECTURE.md` (new §26), `STATUS.md`. **No commits created.**

### Public API

`ActivatedExtensions`, `BUILTIN_EXTENSIONS`, `register_config_keys`,
`register_directives`, `extlink_roles`, `relative_doc_uri`, `TodoCollector`,
`IfConfigValue`/`ifconfig::eval`, `ExtLinkSpec`, `hardcoded_link_warnings`,
`ViewcodeCollector`, `GraphvizRender`/`GraphvizHtmlInfo`, `GithubPagesFiles`,
`DurationReport` (all re-exported from `scribe_core` where applicable).

### Gate evidence (from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive runs) | **750 passed, 0 failed** each run (745 lib/integration + 5 doctests) |
| `cargo test -p scribe-core --test extensions` | 22 passed |
| `cargo test -p scribe-builders --test extensions_differential` | 8 passed (oracle differential + no-Python build + determinism) |

### Differential approach

Oracle (`.oracle-venv/bin/sphinx-build -b html -E -q`) and scribe build the same
fixture into separate temp dirs. Normalization: only the `body` fragment is
compared, `?v=` hashes / graphviz image hashes / absolute todo paths are
placeholder-replaced, whitespace collapses, and structural markers are asserted
instead of byte equality. Fixtures: `test-ext-todo` (`todo_include_todos=1`),
`test-ext-ifconfig`, both `test-ext-extlinks-hardcoded-urls*` variants,
`test-ext-githubpages`, `test-ext-graphviz`, and a minimal autodoc-free
`test-ext-viewcode` derivative. Missing `dot` is asserted to degrade to a warning
plus escaped-source fallback rather than a build failure.

### Risks / intentional divergences

- Graphviz uses BLAKE3 hash prefixes instead of upstream SHA-1 filenames
  (normalized in tests); svg relative-link rewriting and cmap parsing are
  best-effort.
- The substitution form `.. |name| digraph::` is affected by a pre-existing
  substitution-parser gap; explicit graph directives and file arguments work.
- Viewcode infers modules from local `.py` files when the earlier-phase Python
  domain leaves `module` empty; module pages use a slightly wider highlight
  wrapper than upstream.
- `sphinx.ext.autodoc` (Phase 21) is not implemented, so the stock
  `test-ext-viewcode` fixture still emits `unknown directive 'autofunction'`
  warnings; the differential test uses explicit py directives instead.
- ifconfig reports `NameError` diagnostics for config values that upstream's
  fixture `setup()` registers from Python; rendered bodies still match.
- Correcting the Phase 17 visitor `skip_depth` bookkeeping changed HTML nesting
  after self-rendering nodes (a bug fix); all 665+ Phase 17 tests remain green.

---

## Phase 21 — Native Python static & dynamic autodoc + napoleon Engine

### Status

Phase 21 is **DONE**. `crates/scribe-autodoc` is a full native engine; the
`IMPLEMENTATION_PHASE` placeholder is gone. No commits were created.

### Files

- `Cargo.toml` — workspace deps `rustpython-parser` / `rustpython-ast`;
  `scribe-core` depends on `scribe-autodoc`.
- `crates/scribe-autodoc/Cargo.toml` — new deps (parser, AST, serde, miette,
  rayon, walkdir, blake3, all four internal crates).
- `crates/scribe-autodoc/src/{lib,error,model,parser,discovery,dynamic,napoleon,config,directives}.rs`
  — model + parallel static extractor, discovery, dynamic worker protocol,
  napoleon converter, option/config handling, and auto directives.
- `crates/scribe-core/src/extensions/mod.rs` — `autodoc`/`napoleon` activation,
  config-key registration and directive registration (additive).
- `crates/scribe-autodoc/tests/{extractors,napoleon_sections,member_selection,differential,performance}.rs`
  + `tests/common/mod.rs` + `tests/fixtures/tree/` — 40 new tests.
- `ARCHITECTURE.md` (new §27), `STATUS.md`. **No commits created.**

### Public API

`scribe_autodoc::{parse_source, parse_sources, dedent_docstring,
ModuleInfo, ModuleSet, ClassInfo, FunctionInfo, AttributeInfo, Member,
ImportInfo, SourceSpan, ParameterInfo, resolve_module, search_roots,
load_modules, discover_package, introspect, DynamicOptions, DynamicModule,
convert_docstring, NapoleonOptions, auto_options, napoleon_options,
register_config_keys, register_directives, select_members, AutodocError,
AutodocResult}` (all re-exported from `scribe_autodoc`).

### Gate evidence (from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive runs) | **790 passed, 0 failed** each run (750 prior + 40 new) |
| `cargo test -p scribe-autodoc` | 40 tests (10 in-module + 30 integration), all green |
| End-to-end `scribe build -b html` on `test-ext-autodoc`, `test-ext-napoleon`, `test-ext-napoleon-paramtype` | success, API docs with signatures/annotations/fields |

### Differential approach

`tests/differential.rs` runs the pinned oracle `sphinx-build -b pseudoxml -E -q`
on `test-ext-autodoc`, `test-ext-napoleon` and `test-ext-napoleon-paramtype`,
parses the same `.rst` sources through scribe with autodoc directives + the
Python-domain read transform, and compares a documented structural subset:
object identities `(module, class, fullname)`, ordered signature name tokens,
lowercased field names, and targeted parameter/annotation presence. Generated
ids, source paths, namespaces, CSS-only and `py:*` skeleton attributes are
normalized away. All three fixtures reach identical object identities and
order; the napoleon fixtures produce the same *Parameters* field list and
parameter names. See `ARCHITECTURE.md` §27.7.

### Risks / intentional divergences

- Static mode cannot observe runtime-computed attributes, imported-member
  following beyond statically resolvable aliases, or `dataclass`/`__slots__`
  generated members; the documented dynamic worker bridge covers these only
  when explicitly invoked and degrades to a warning when Python is absent.
- `test-ext-napoleon-paramtype`: the frozen Python domain groups `:keyword:`
  fields as *Parameters* (upstream: *Keyword Arguments*) and does not rewrite
  `:kwtype:`/`:paramtype` aliases, leaving extra fields. Parameter names and
  values still match; deliberately not patched in Phase 12 code.
- Napoleon `Returns:` sections can be emitted with a capitalized field name to
  match upstream grouping; plain-RST `:returns:` keeps its lowercase label.
- `napoleon_custom_sections` and `.. index::` directive sections are not
  interpreted; `.. attribute::`/`.. method::` are mapped to the Python-domain
  skeleton emitter.
- The module cache is process-global and content-hash-keyed (bounded to 256
  entries); it is safe for concurrent builds but holds a small amount of
  memory across builds in one process.

---

## Phase 22 — autosummary & doctest Builders/Extensions

### Status

Phase 22 is **DONE**. `sphinx.ext.autosummary` and `sphinx.ext.doctest` are
implemented natively in `scribe-autodoc` and `scribe-builders`; the CLI
dispatches `scribe build -b doctest`. No commits were created.

### Files changed / added

- `Cargo.toml` + crate manifests — `minijinja`/`scribe-theme` for autodoc,
  `chrono`/`scribe-autodoc` for builders, new autosummary/doctest config keys.
- `crates/scribe-autodoc/src/autosummary/{mod,directive,render,stub,table,templates}.rs`
  + `NOTICE` — directive, table emitter, stub generator and vendored templates.
- `crates/scribe-builders/src/doctest/{mod,builder,directives,error,extract,normalize,runner}.rs`
  — five directives, extraction/grouping, Python worker and builder.
- Additive wiring: `scribe-core` extension registry, `scribe-config` builtin
  keys, HTML builder pre-read stub generation, CLI doctest dispatch.
- Tests: `crates/scribe-autodoc/tests/{autosummary,autosummary_differential}.rs`
  + `tests/fixtures/asum/`; `crates/scribe-builders/tests/{doctest,doctest_differential}.rs`
  + `tests/fixtures/{doctest,doctest-skipif}/`; `ARCHITECTURE.md` §28.
  Frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` shapes untouched.

### Public API

`scribe_autodoc::{autosummary::*, generate_stubs, StubRenderer, StubReport,
AutosummaryItem, AutosummaryOptions, escape_rst, mangle_signature, parse_names}`
and `scribe_builders::{doctest::*, DoctestBuilder, DoctestResult, DoctestError,
register_doctest_directives, register_doctest_config_keys}` (full list in §28.2).

### Gate evidence (from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean** |
| `cargo test --workspace` × 3 consecutive | **830 passed, 0 failed** each run (790 prior + 40 new) |
| `cargo test -p scribe-autodoc --test autosummary_differential` | 2 passed: 5 real fixture roots, all oracle stubs byte-identical after BOM strip; table name facts equal |
| `cargo test -p scribe-builders --test doctest_differential` | 2 passed: oracle/scribe `output.txt` byte-identical after timestamp header; real `test-ext-doctest` root 22-example count parity |
| `scribe build -b html -E` on `test-ext-autosummary` | 8/8 oracle stubs byte-exact; hidden toctree feeds stub pages |
| `scribe build -b doctest -E` on self-contained fixture | 16 tests, 0 failures; output.txt matches oracle exactly |

### Differential approach

Autosummary stubs are compared byte-for-byte (after stripping the UTF-8 BOM
that Sphinx writes with `source_encoding = utf-8-sig`) on five real oracle
roots. Class/module stub member lists, inherited attributes and ordering were
tuned until all eight stubs of `test-ext-autosummary` and all stubs of the
four variants matched exactly. Doctest execution uses the **real** Python 3.15
`doctest` engine via a JSON subprocess protocol; the self-contained fixtures
are faithful derivatives of the upstream `test-ext-doctest` and
`test-ext-doctest-skipif` roots with only the pytest-dependent cleanup
replaced, so failure diffs and runner summaries are compared byte-for-byte
after removing the date header. One documented divergence is excluded by name
(`autosummary_importfail`, which raises `SystemExit` on real import).

### Risks / intentional divergences

- `autosummary_importfail` yields one extra stub/table row statically; asserted
  in the differential tests and documented in §28.6.
- Imported/inherited autosummary members are recovered statically (source
  `self.x = ...` scanning plus import-based base resolution); runtime-computed
  attributes remain invisible.
- `:autolink:` role is not registered separately; generated table xrefs use
  `:py:obj:` through the frozen Phase 14 resolver.
- Doctest `:pyversion:` is modeled as a SKIP example (counted as a try) while
  `:skipif:` drops the snippet, matching upstream's different handling.
- Doctest execution requires a Python interpreter; without one the builder
  writes a graceful "examples skipped" report and exits successfully.
- `{% trans %}` in user autosummary templates is adapted with Phase 16's
  preprocessor; other Jinja extensions are not supported by minijinja.

---

## Phase 23 — LaTeX Writer & PDF Builder — DONE

### Delivered files

- `crates/scribe-builders/src/latex/{mod,escape,translator,builder,support}.rs`
  — public API, escaping/Unicode layer, frozen-`Visitor` translator, builder
  and embedded vendored-support-file table.
- `crates/scribe-builders/src/latex/texinputs/` + `ATTRIBUTION.md` — 23
  vendored Sphinx support files (`sphinx.sty`, `.cls`, `sphinxlatex*.sty`,
  `python.ist`, BSD-3-Clause with attribution), embedded via `include_str!`
  and copied to the output dir.
- `crates/scribe-builders/src/error.rs` — `LatexError` (thiserror + miette
  `scribe::builders::latex::*` codes) and `LatexResult` alias.
- `crates/scribe-builders/src/lib.rs` — `pub mod latex` plus top-level
  re-exports (`LatexBuilder`, `LaTeXTranslator`, `LatexContext`,
  `LatexEngine`, `escape_latex`, `LatexError`).
- `crates/scribe-cli/src/lib.rs` — `latex`/`pdf` dispatch and the
  `LatexBuilderFailed` miette diagnostic.
- Tests: `crates/scribe-builders/tests/{latex_translator,latex_differential,latex_pdf}.rs`.
- Docs: `ARCHITECTURE.md` §29 (upstream sources, API, preamble strategy,
  node→LaTeX mapping, normalization, divergences).

### Public API

`scribe_builders::latex::{LaTeXTranslator::translate, LatexContext,
LatexBuilder::build, LatexBuilder::compile_pdf, escape_latex, LatexEngine,
BuildOptions, BuildResult}` — full signatures in `ARCHITECTURE.md` §29.3.

### Gate evidence (from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean** |
| `cargo test --workspace` × 3 consecutive | **861 passed, 0 failed** each run (830 prior + 31 new) |
| `cargo test -p scribe-builders --test latex_differential` | 7 passed: 6 oracle structure differentials (index/unicode/table/numfig/equations) + determinism + support files |
| `cargo test -p scribe-builders --test latex_pdf` | 3 passed: real `latexmk -pdf` compile of generated `.tex` (PDF header + size asserted), `-b pdf` builder records a PDF, typed missing-source error |
| `cargo test -p scribe-builders --test latex_translator` | 9 passed: preamble, escaping, markup, verbatim, lists, tables, refs/math/index/figures, howto mode, determinism |
| `scribe build -b latex -E` on `test-latex-unicode` | valid `projectnamenotset.tex` written; compiled cleanly by `latexmk -pdf` → 3-page PDF (139,754 bytes) |
| `scribe build -b pdf -E` on `test-latex-unicode` | `.tex` + `Makefile`/`make.bat`/`latexmkrc` + vendored support files + compiled PDF |

### Differential approach

Both engines build six real `test-latex-*` roots; each `.tex` is reduced to a
canonical marker set (preamble commands, environments, labels, math/index/
reference markers) with alias collapsing (`sphinxmaketitle`→`MAKETITLE`,
`tabularx`/`longtable`→`TABLE_ENV`, `equation*`/`\[`→`MATH`). The curated
markers required per fixture must occur in both outputs. Full byte equality is
not attempted because the preambles differ by design (self-contained vs
`sphinx.sty`) and the shared parser lowers some constructs differently. PDF
compilation is a genuine toolchain assertion, not a string check.

### Risks / intentional divergences

- Default preamble is self-contained (standard TeX Live packages + inline
  Sphinx macro definitions) for guaranteed `latexmk` compilation; full upstream
  fidelity (`sphinx.sty`/Babel/xindy/fncychap) requires setting
  `latex_elements["use_sphinx_sty"] = true` and a full TeX Live.
- `date` is taken from `config["today"]` (empty by default) instead of the
  wall clock so `.tex` output is deterministic.
- Only a subset of `latex_elements` is mapped (`preamble` and the scribe
  extension `use_sphinx_sty`); `fvset`, `fncychap`, `sphinxsetup`, `babel`,
  geometry options and xindy are not individually honoured.
- Inline `:index:` roles are lowered by the shared parser to text, not `Index`
  nodes (documented in §29.8 and excluded from the `test-latex-index`
  differential); explicit `index` directives do emit `\index{…}`.
- Fixtures with a custom `latex_elements["preamble"]` that redefines
  `\sphinxincludegraphics` in terms of `\sphinxsafeincludegraphics`, or that
  reference images absent from their source tree, may not compile with the
  self-contained preamble; the builder degrades with warnings, never panics.
- `latex_logo` is captured in `LatexContext` but not yet emitted as
  `\sphinxlogo`.
- No commits were created, per the orchestrator's instruction.

---

## Phase 24 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` × 3 consecutive | **895 passed, 0 failed** each run (891 library/integration + 4 doctests); all 861 prior tests stay green |
| `cargo test -p scribe-builders --test text_writer` | 13 passed: wrap/CJK width, empty/Unicode no-panic, heading underlines, lists, literal blocks, admonitions, ASCII+Unicode tables, section numbers, `text_newlines`, normalized `test-build-text` differential |
| `cargo test -p scribe-builders --test man_writer` | 7 passed: escaping, header/macros/font escapes, `man_pages` parse, normalized `test-manpage_url` differential, **groff validator** |
| `cargo test -p scribe-builders --test texinfo_writer` | 7 passed: escaping, header, nodes/menus/blocks, refs/index, `texinfo_documents` parse, normalized `test-toctree` differential, **makeinfo validator** |
| `cargo test -p scribe-builders --test epub_builder` | 7 passed: escaping, external-ref stripping, XHTML/XML well-formedness, nav/NCX, default metadata, **ZIP structure validator**, OPF/nav structural differential, byte-identical determinism |
| CLI smoke `-b epub` / `-b epub3` / `-b man` / `-b texinfo` / `-b text` | All exit 0; 15/15/2/2/8 output files respectively on `test-toctree`; no panics |

**Validator evidence (real tools, asserted in tests):**

| Tool | Command | Result |
|---|---|---|
| groff 1.24.1 | `/usr/sbin/groff -man -Tutf8 <generated .1>` | exit 0, **empty stderr**, rendered text contains `PROJECTNAMENOTSET` |
| makeinfo (texi2any 7.3) | `/usr/sbin/makeinfo --no-split -o /dev/null <generated .texi>` | exit 0, **empty stderr** |
| zip 3.0 | `/usr/sbin/zip -T <generated .epub>` | exit 0 (`test of ... OK`) |
| ZIP header check | parse local file header + read member bytes | first entry `mimetype`, STORED (method 0), exact `application/epub+zip` bytes |
| roxmltree 0.20 | parse every `.opf`/`.ncx`/`.xml`/`.xhtml` member (`allow_dtd`) | all well-formed |

`epubcheck` is **not installed**, so full EPUB conformance is not machine-
checked; EPUB validity is asserted via ZIP structure + XML well-formedness as
per the phase spec (documented in `ARCHITECTURE.md` §30.4).

**New files:** `crates/scribe-builders/src/{build_common,epub,man,texinfo,text}.rs`,
`crates/scribe-builders/tests/{text_writer,man_writer,texinfo_writer,epub_builder}.rs`,
`crates/scribe-builders/tests/output_common/mod.rs`.
**Modified (additive):** workspace `Cargo.toml` (`zip`, `roxmltree` dev slot),
`crates/scribe-builders/{Cargo.toml,src/lib.rs,src/error.rs}`,
`crates/scribe-config/src/registry.rs` (new text/man/texinfo/epub config keys),
`crates/scribe-rst/src/directives.rs` (seealso optional argument remediation),
`crates/scribe-cli/src/lib.rs` (dispatch + `OutputBuilderFailed` diagnostic),
`ARCHITECTURE.md` (new §30), `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment`
and §15 `Domain`/`ResolvedReference` public shapes are **untouched**. **No
commits created** — changes left uncommitted per user request.

**Risks / deferred:** EPUB nav uses document-level (not section-anchor) nav
points; `dcterms:modified` uses a fixed epoch rather than wall clock; full
`epubcheck` conformance is untested. Texinfo `toctree_only` does not assemble
appendices. Text defaults to width 80 / ASCII tables (upstream is 70 / ASCII);
set `text_width = 70` for upstream wrap parity. The shared parser's section
numbering and nested toctree rendering remain flatter than Sphinx's resolved
writer input, so the text differential compares normalized content tokens
rather than byte-exact layout. Phase 25 (linkcheck/xml/gettext) and Phase 26
(i18n) remain pending.

## Phase 25 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **920 passed, 0 failed** on runs 1, 2 and 3 (895 prior + 25 new: unit + integration/differential + doctests) |
| `cargo test -p scribe-builders --test xml_differential` | 4 passed: oracle byte equality for `-b xml` / `-b pseudoxml` on `test-basic` (normalized source path + Docutils version), XML well-formedness via `roxmltree`, `xml_pretty=0` compact output |
| `cargo test -p scribe-builders --test changes_differential` | 3 passed: oracle byte equality for `changes.html`, `index.html` and all `rst/*.html` on `test-changes`; grouping assertions; empty-version behavior |
| `cargo test -p scribe-builders --test linkcheck_differential` | 3 passed: local tokio HTTP server + oracle status comparison for good/anchor/bad-anchor/redirect/broken/ignored links; deterministic ordering; malformed URL / network error / `mailto:` no-panic; internal doc/anchor verification |
| `./target/debug/scribe build -b linkcheck -q -E <src> <out>` | writes `output.txt` + `output.json`; exit 0 with broken links and no `-W` |
| `./target/debug/scribe build -b linkcheck -q -E -W <src> <out>` | same report plus structured miette `scribe::linkcheck::broken_links` diagnostic; exit 1, no panic |
| `./target/debug/scribe build -b xml\|pseudoxml\|changes -q -E <fixture> <out>` | writes one file per doc (or the changelog summary files) without panics |

**New files:** `crates/scribe-builders/src/{xml,changes,linkcheck}.rs`,
`crates/scribe-builders/tests/{xml_differential,changes_differential,linkcheck_differential}.rs`,
`crates/scribe-builders/tests/phase25_common/mod.rs`.
**Modified (additive):** `crates/scribe-builders/{Cargo.toml,src/lib.rs,src/build_common.rs}`
(new modules/deps, upstream directive aliases for version directives / `module` /
`class`), `crates/scribe-config/src/registry.rs` (17 new `linkcheck_*` + `xml_pretty` keys),
`crates/scribe-cli/src/lib.rs` (builder dispatch + `LinkcheckBrokenLinks` diagnostic),
`ARCHITECTURE.md` (new §31), `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment`
and §15 `Domain` public shapes are **untouched**. **No commits created** —
changes left uncommitted per user request.

**Differential approach:** `xml`/`pseudoxml` are compared byte-for-byte to
`sphinx-build -b xml|pseudoxml` on `test-basic`, normalizing only the absolute
`source` path and the Docutils generator version comment; the real-XML
translator is additive (the frozen `to_xml()` helper is left intact).
`changes` is compared byte-for-byte to `sphinx-build -b changes` on
`test-changes`, including every `rst/*.html` source frame. `linkcheck` uses a
process-local tokio HTTP server (no external network) and compares each URI's
status from `output.json` against the oracle's report, with additional
assertions for deterministic ordering, JSON field order and no-panic handling
of malformed URLs/network failures.

**Risks / deferred:** linkcheck's non-zero exit on broken links is gated on
`-W` (upstream always exits 1); the HTML anchor scanner is conservative
(start-tag `id`/`name` attributes, may over-accept); complex Python config
values (auth tuples, dict regexes) are only available through the typed config
subset; `changes_titles_*` keys do not exist in the pinned oracle and are
ignored; the changes builder emits placeholder CSS rather than verbatim theme
stylesheets (excluded from byte-comparison). Internal `#anchor` / `refid` and
scheme-less local links are verified against the parsed doctrees and source
tree without network; external HTTP anchors are checked via the HTML attribute
scanner. Cross-document internal `refid` links resolved by Phase 14 may carry
generated ids that are only present in the target doctree, which the index
covers.

## Phase 26 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **963 passed, 0 failed** on runs 1, 2 and 3 (920 prior + 43 new) |
| `cargo test -p scribe-core --test i18n_transforms` | 15 passed: extraction from titles/paragraphs/admonitions, literal/substitution skipping, explicit attribute opt-in/out, Locale apply + node marking, idempotent/empty translations, priority-ordered pipeline, translation classes, preserve/original, inline unwrapping, progress metadata serialization, malformed-input no-panic |
| `cargo test -p scribe-builders --test i18n_differential` | 7 passed: byte-exact oracle POT parity for `test-gettext-template` (`index`+`sphinx` domains), oracle message-set parity for `test-intl` (`admonitions`/`translation_progress`), upstream POT header, Babel-`.mo`→scribe-HTML round trip, scribe-only `.po`→`.mo` compile, `.po` translation preservation + stable POT timestamp, malformed catalog no-panic |
| `scribe build -b gettext -E -Q <src> <out>` | Emits `<domain>.pot` with upstream header/locations/ordering; updates existing `.po` in place preserving `msgstr`; `should_write` keeps POT-Creation-Date stable on unchanged body |
| Babel `.po`→`.mo` → `scribe build -b html -E -Q` | Localized HTML contains translated heading, paragraph, admonition title and admonition body (asserted in two integration tests) |
| scribe-written `.mo` read by Python `gettext.GNUTranslations` | `t.gettext("Hello world.")` → `"HELLO WORLD."`; the binary layout is interoperable with the reference implementation |

**New files:** `crates/scribe-core/src/i18n.rs`,
`crates/scribe-core/tests/i18n_transforms.rs`,
`crates/scribe-builders/src/i18n/{mod,builder,catalog,mo,po}.rs`,
`crates/scribe-builders/src/i18n/tests/fixtures/oracle.mo` (Babel-generated),
`crates/scribe-builders/tests/i18n_differential.rs`.
**Modified:** `crates/scribe-config/src/registry.rs` (8 `gettext_*` keys),
`crates/scribe-core/src/{lib,transforms,post_transforms}.rs` (context catalog,
activated seams, progress substitution),
`crates/scribe-builders/src/{lib,build_common}.rs`,
`crates/scribe-builders/src/html/builder.rs`,
`crates/scribe-builders/src/latex/builder.rs`,
`crates/scribe-cli/src/lib.rs` (`-b gettext` dispatch),
`ARCHITECTURE.md` (new §32, §14 seam status), `STATUS.md`. Frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` public shapes are **untouched**. **No
commits created** — changes left uncommitted per user request.

**Round-trip evidence:** the `babel_mo_round_trip_localized_html` test compiles
a four-message `.po` with the oracle's Babel and asserts that scribe's HTML
contains `LOCALIZED HEADING`, `HELLO WORLD.`, `IMPORTANT NOTE` and
`NOTE BODY TEXT.`; `scribe_compiles_po_without_babel` deletes the `.mo` and
asserts scribe recompiles it from the `.po` and produces the same translated
HTML. Both tests are green.

**Differential normalization:** POT files are compared byte-for-byte after
replacing only the `POT-Creation-Date` value (documented in `ARCHITECTURE.md`
§32.4). The `test-intl` comparison is at the message-set level because of the
documented `rawsource` and numbered-heading parser divergences also recorded
in §32.5.

**Risks / deferred:** upstream extracts `rawsource`, so inline-markup messages
differ from scribe's rendered-text `msgid`s; docutils heading line numbers can
differ by 1–2; only the first block of a multi-paragraph `msgstr` is
transplanted; `gettext_uuid` is registered but random UUID comments are not
emitted (determinism); fallback catalog chains are reduced to the first
available `.mo`; Jinja plural/variable `trans` constructs are out of scope.
These are parser / scope driven, not i18n-pipeline defects, and are all
documented in §32.5.

## Phase 27 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo check --workspace --all-targets --features wasm-plugins` | `Finished dev profile`; **0 warnings** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo clippy --workspace --all-targets --features wasm-plugins -- -D warnings` | **clean** |
| `cargo test --workspace` (×3 consecutive) | **998 passed, 0 failed** each run (963 prior + 35 new) |
| `cargo test -p scribe-cli --test interop` | 2 passed; real `scribe build -b html -E` loads a fixture Python extension via `scribe.toml` and its `source-read` + `pybadge` directive markers appear in `index.html` |
| `cargo test -p scribe-builders --test interop` | 5 passed; includes byte-for-byte built-in-extension output equality with/without the bus and an "all core events fired in one small build" assertion |
| `cargo test -p scribe-core --lib --features wasm-plugins` | 59 passed; trivial Wasm plugin manifest loads and its callback is invoked through the ABI (`WasmPluginHost::is_available() == true`) |

**Plugin/shim evidence:** `crates/scribe-core/tests/fixtures/python_extension.py`
registers a `source-read` handler (replaces `<<MARKER>>` with `PYTHON-WAS-HERE`)
and a `pybadge` directive; both appear in a real `scribe build` HTML page.
`crates/scribe-core/examples/wasm_plugin.rs` documents the v1 ABI with a
Rust/C-compatible example; with `wasm-plugins` on, a trivial plugin manifest is
loaded and `on_source_read` invoked, mutating a shared source through the bus.

**Files:** new `crates/scribe-core/src/{events,plugin,python_shim,bridge}.rs`
plus `examples/wasm_plugin.rs`; new integration tests in `scribe-core/tests`,
`scribe-builders/tests/interop.rs` and `scribe-cli/tests/interop.rs`; additive
`bridge` field on `scribe_builders::html::BuilderOptions`; three new
`scribe-config` keys; `wasm-plugins` feature on `scribe-core`.

**Risks / deferred:** Wasm bytecode interpretation is not compiled — the ABI,
manifest format, host-function interface and native-trampoline backend are
stable, with graceful skip by default (no `wasmtime`/`extism` dependency). The
Python shim is synchronous (one worker per script); only scalar
`html-page-context` keys are applied; third-party roles are declared by the
protocol but lack an inline adapter; built-in extensions remain direct calls
for frozen-AST fidelity, with their hook points mirrored by emitted events and
proven byte-identical by differential tests. No commits were created — changes
left uncommitted per user request.


## Phase 28 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** (default + new serve code/tests) |
| `cargo test --workspace` (× 3 consecutive) | **1032 passed, 0 failed** on runs 1, 2 and 3 (998 prior + 34 new) |
| `cargo test -p scribe-cli --lib` | 33 passed: 24 new serve-module unit tests (MIME mapping, HTML-only injection + idempotency, path→docname mapping, reserved-path exclusion, event-kind filtering incl. Access events, change classification, debounce coalescing, CLI arg/defaults/`watch` alias, `build_options` mapping, stable error messages) + 9 prior CLI tests |
| `cargo test -p scribe-cli --test serve` | 10 passed: real `scribe serve --port 0` subprocess with stdout-derived address and bounded polling — injected `index.html`, `/` + CSS, 404/traversal, `.rst` edit→HTTP refresh, SSE `scribe-reload` after successful rebuild, `--no-reload`, **differential** (served page minus injection byte-equal to fresh `scribe build -b html -E`), performance smoke (~20 ms), SIGINT graceful exit 0, port conflict + missing source → clear non-zero errors |

**New files:** `crates/scribe-cli/src/serve/{mod,mapping,mime,http,inject}.rs`,
`crates/scribe-cli/tests/serve.rs`.
**Modified (additive):** workspace `Cargo.toml` (`notify` 8, tokio features),
`crates/scribe-cli/Cargo.toml` (`notify`, `tokio` net/fs/io-util/sync/signal,
`serde_json`), `crates/scribe-cli/src/lib.rs` (`Command::Serve` with visible
`watch` alias + dispatch + subcommand-registration assertion),
`ARCHITECTURE.md` (new §34), `STATUS.md`. Frozen §7 AST, §12 `BuildEnvironment`
and §15 `Domain` public shapes untouched. **No commits created.**

**Integration approach:** every test spawns the actual `scribe` binary against a
process-unique temp fixture, parses the resolved port from the flushed
`serving at http://HOST:PORT` stdout line, and never uses fixed sleeps — all
synchronization is bounded polling of `GET /_scribe_health`, the served page,
or an SSE stream. The child's stdout/stderr are drained on background threads
for its whole lifetime (dropping the stdout reader closes the pipe and makes
the server panic on its next `println!`, exit 101). A critical loop bug was
found and fixed during development: `notify`'s recommended Linux watcher emits
`Access(Open)` events when the builder merely *reads* sources, which caused a
self-sustaining rebuild loop; `mapping::is_relevant_event` now filters them.

**Differential approach:** after an incremental rebuild triggered by editing a
source page, the served HTML (with the single `data-scribe-live` script tag
stripped) is compared byte-for-byte with an independent full
`scribe build -b html -E` into a separate directory. Injection happens at
response time precisely so on-disk output stays byte-identical to a fresh
build. No oracle `serve` command exists, so the differential targets rebuild
correctness rather than server bytes.

**Risks / deferred:** true per-page HTML writes with an incremental cache for
project-wide artifacts (search index / genindex / objects.inv / nav) are a
documented future optimization — the dev server currently re-runs the
deterministic full builder, gated by the Phase 08 minimal rebuild set and
skipped entirely when nothing relevant changed; SSE is used instead of
WebSocket (explicit spec fallback); config edits that do not touch
`RelevantConfig` do not invalidate the environment (matching upstream
`CONFIG_OK`). No commits were created — changes left uncommitted per user
request.

---

## Phase 29 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **1055 passed, 0 failed** on runs 1, 2 and 3 (1032 prior tests all stay green + 23 new) |
| `cargo test -p scribe-cli --test conformance -- --nocapture` | 2 passed: harness runs 39 fixture/builder pairs through oracle + scribe in a bounded rayon pool; deterministic `CONFORMANCE.md` written; committed baseline regression guard green |
| `cargo test -p scribe-cli --test regression_guard` | 5 passed: all persisted proptest seed files valid + replayed; Phase 05/07/02/12/13 adversarial corpora re-run through every parser |
| `cargo test -p scribe-rst --test fuzz` | 13 passed (7 properties × 1024 default + 6 deterministic regressions) |
| `cargo test -p scribe-myst --test fuzz` / `--test adversarial` | 5 passed / 7 passed (default cases) |
| `cargo test -p scribe-domains --test fuzz` | 6 passed (5 properties + 1 deterministic regression) |
| `cargo test -p scribe-config --test fuzz` | 5 passed (4 properties + 1 deterministic regression) |

**Stress campaign (all green):**

| Command | Result |
|---|---|
| `PROPTEST_CASES=10000 cargo test -p scribe-rst --test fuzz` | 13 passed (70,000 generated cases) |
| `PROPTEST_CASES=10000 cargo test -p scribe-myst --test fuzz` | 5 passed (40,000) |
| `PROPTEST_CASES=10000 cargo test -p scribe-myst --test adversarial` | 7 passed (50,000) |
| `PROPTEST_CASES=10000 cargo test -p scribe-domains --test fuzz` | 6 passed (50,000) |
| `PROPTEST_CASES=10000 cargo test -p scribe-config --test fuzz` | 5 passed (40,000) |
| **Total** | **250,000 fuzz iterations** across 25 property functions |

**Measured conformance (tracked metric, not a gate):** **65.6 % semantic
parity** over 34 applicable fixtures — 3 pass, 25 partial, 6 fail, 5 skip
(out of a 39-pair selection). `CONFORMANCE.md` holds the full per-fixture
table, skip reasons and categorized divergences; the rubric and harness design
are in `ARCHITECTURE.md` §35. Parity is honestly reported as measured; no test
was weakened and no number inflated to approach the 95 % aspirational target.

**New files:** `crates/scribe-cli/tests/conformance.rs`,
`crates/scribe-cli/tests/regression_guard.rs`,
`crates/scribe-cli/tests/fixtures/conformance_baseline.txt`,
`crates/scribe-myst/tests/fuzz.rs`, `crates/scribe-domains/tests/fuzz.rs`,
`crates/scribe-config/tests/fuzz.rs`,
`crates/scribe-domains/tests/fuzz.proptest-regressions` (new seed for the bug
below), `CONFORMANCE.md`.
**Modified (additive):** workspace `Cargo.toml` (`scribe-myst` dep slot),
`crates/scribe-{cli,domains,config}/Cargo.toml` (dev-deps: rayon/proptest/
cross-crate crates), `crates/scribe-rst/tests/fuzz.rs`,
`crates/scribe-myst/tests/adversarial.rs` (env-driven case counts; defaults
unchanged), `crates/scribe-domains/src/cpp/parser.rs` (bug fix),
`ARCHITECTURE.md` (new §35 + status line), `STATUS.md`. Frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` public shapes untouched. **No commits
created** — changes left uncommitted per user request.

**Bug found & fixed by fuzzing:** `parse_cpp_declaration("=aa", …)` panicked
(index out of bounds at `cpp/parser.rs`) when a trailing `=` alias left no
declaration head. The parser now returns a well-formed empty `CppDecl` for an
empty head; the failing input, its persisted proptest seed and three neighbours
are pinned as deterministic regressions. Two earlier false-positive properties
in the new suites (a `Tags` cache assertion using a tag already present, and a
constant-expression expectation for `? :` syntax) were corrected during
development — no production behaviour changed.

**Honest gaps (tracked, not gate failures):** the lowest-scoring fixtures
(`test-prolog`, `test-toctree-glob`, `test-root`, `test-domain-cpp`,
`test-productionlist`, `test-default_role`) reflect known earlier-phase limits
in prolog/epilog label injection, globbed toctrees, full theme DOM parity,
C++ signature wrapping, productionlist token xrefs and default-role mapping.
Warning-score divergence on nitpicky builds comes from differing warning
messages/codes between the two engines. The rubric compares tag multisets (not
attribute/text bytes); byte-level fidelity remains the job of the per-phase
differential suites. Five fixtures are explicitly skipped with recorded
reasons (network, external `dot`, import-heavy extension, and two roots that
require Sphinx's temp-testroot copy step). No fuzz target found a panic in the
RST, MyST, Python-signature or Tags parsers; the C++ panic above was the sole
production bug fixed.

## Phase 30 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **1056 passed, 0 failed** on runs 1, 2 and 3 |
| `cargo test --workspace --release` | **1056 passed, 0 failed**, **0 Rust compiler warnings** |
| `cargo test -p scribe-cli --test selfhost -- --nocapture` | 1 passed: real `sphinx/doc` build, exit 0, 158 HTML pages, no panic, 6.77 s debug |
| `RUNS=5 PAGES=40 ./scripts/bench.sh` | scribe 0.12 s / 42,520 KB; sphinx-build 0.63 s / 79,196 KB; **5.25× / 1.86×** |
| `RUNS=3 PAGES=150 ./scripts/bench.sh` | scribe 0.26 s / 116,524 KB; sphinx-build 1.66 s / 92,600 KB; **6.38× / 0.79×** |
| `scribe build -b html -c sphinx/doc sphinx/doc <out>` | 155 HTML pages written, **exit 1** (custom `sphinx13` theme inheritance) |
| `scribe build -b html -c sphinx/doc -D html_theme=basic sphinx/doc <out>` | **exit 0**: `build succeeded, 1359 warning(s).` 155 pages / 337 files |
| `bash -n scripts/{bench,release}.sh` + `python -m py_compile scripts/gen_bench_fixture.py` | clean |
| `./scripts/release.sh --check` | Only host target installed; `cross`/`lipo` absent → cross-compilation intentionally not run |

**New files:** `README.md`, `MIGRATION.md`, `BENCHMARKS.md`,
`crates/scribe-cli/tests/selfhost.rs`, `scripts/gen_bench_fixture.py`,
`scripts/bench.sh`, `scripts/release.sh`, `.github/workflows/release.yml`.
**Modified (additive):** workspace `Cargo.toml` (`[profile.release]`),
`ARCHITECTURE.md` (status line + new §36), `STATUS.md` (Phase 30 row + this
ledger). **No production algorithm changed; frozen §7 AST, §12
`BuildEnvironment` and §15 `Domain` shapes untouched. No commits created.**

---

## Final program summary (Phase 30 — program complete)

### Delivered

A single Rust workspace of **11 crates** implementing a Sphinx-compatible
documentation engine: reStructuredText and MyST parsers, a frozen arena AST,
nine domains with cross-reference and `objects.inv` resolution, 15+ output
builders, a theme/template engine, syntax highlighting, client-side search,
gettext i18n, native autodoc/napoleon/autosummary, a Python/Wasm interop
bridge, a live-reload dev server, and a conformance/fuzz harness.

### Honest self-hosting outcome

The real 155-page `sphinx/doc` tree builds end-to-end. With the custom
`sphinx13` theme overridden by a bundled theme
(`-D html_theme=basic`) the build exits **0** and writes 155 pages / 337 files
(1359 warnings, mostly unresolved external/domain cross-references). With the
unmodified `conf.py` it writes all 155 pages but exits **1** solely because
the custom theme inherits from a non-bundled theme. No Rust panic occurred in
either case. This is recorded verbatim in `BENCHMARKS.md` and exercised by
`crates/scribe-cli/tests/selfhost.rs`.

### Measured performance (this host: Ryzen 9 7900X, 12 cores, Linux WSL2)

| Workload | scribe wall / RSS | sphinx-build wall / RSS | Measured ratio |
|---|---|---|---|
| 40-page offline fixture | 0.12 s / 42,520 KB | 0.63 s / 79,196 KB | **5.25× faster, 1.86× less RSS** |
| 150-page offline fixture | 0.26 s / 116,524 KB | 1.66 s / 92,600 KB | **6.38× faster, ~1.26× more RSS** |
| `sphinx/doc` (scribe only) | ~1.05 s / ~284,112 KB | — | exit 0 with theme override |

The aspirational 10–50× speedup was **not** achieved; the honest measured
range is ~5–6× on the document-processing core. These are tracked metrics.

### Verification totals

- **1056 tests** pass (3 consecutive debug runs + 1 release run), zero Rust
  compiler warnings, clean `cargo check` and `cargo clippy -D warnings`.
- **65.6% semantic parity** over 34 applicable upstream fixtures
  (3 pass / 25 partial / 6 fail / 5 skip) — `CONFORMANCE.md`, guarded by a
  committed baseline.
- **250,000 fuzz cases** across 25 properties; one real C++ empty-head alias
  panic found and fixed during Phase 29.

### Remaining known gaps (categorized, tracked — not gate failures)

- **Parser/AST fidelity:** frozen AST lacks `title_reference`, `Classifier` and
  `OptionGroup`; slugification uses a static Unicode map; `rst_prolog`/`rst_epilog`
  label injection and globbed toctrees are incomplete (lowest conformance
  fixtures: `test-prolog`, `test-toctree-glob`).
- **Domain/xref:** C++ signature wrapping and productionlist token xrefs are
  partial; external/nitpicky warning messages and codes differ from Sphinx, so
  `-W` behaviour can diverge (`test-domain-cpp`, `test-productionlist`,
  `test-default_role`).
- **Themes/DOM:** four bundled themes only; custom theme inheritance (e.g.
  `sphinx13`) fails; full theme HTML DOM parity is incomplete
  (`test-root`, `test-theming`).
- **Extensions/interop:** arbitrary Python extensions and native-module
  autodoc are unavailable; `sphinx.ext.intersphinx` inventory resolution exists
  but automatic remote fetching is not fully wired; `coverage`,
  `inheritance_diagram` and non-`dot` graph backends are unsupported;
  linkcheck needs network.
- **Performance/memory:** peak RSS scales worse than the oracle on large trees
  (~1.26× at 150 pages); arena retention and bundled-asset memory are future
  optimization targets.
- **Process tooling:** `quickstart`/`clean`/`check` were Phase 01 placeholders
  at the time of the Phase 30 summary; **Phase 31 has replaced all three with
  production implementations** (see the Phase 31 ledger below). The
  `-M`/make-mode shim and a Criterion parser micro-benchmark remain out of
  scope (the end-to-end harness is authoritative).

All seven tracked documents — `README.md`, `MIGRATION.md`, `BENCHMARKS.md`,
`CONFORMANCE.md`, `ARCHITECTURE.md`, `STATUS.md` (this file) — exist and are
consistent with the measurements and gates above. **Phase 30 is DONE; the
30-phase program is complete.**

---

## Phase 31 gate evidence (commands run, from `/home/credo/code/scribe`)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished dev profile`; **0 warnings, 0 errors** |
| `cargo clippy --workspace --all-targets -- -D warnings` | `Finished dev profile`; **clean** |
| `cargo test --workspace` (× 3 consecutive) | **1073 passed, 0 failed** on runs 1, 2 and 3 (1056 prior tests all green + 17 new) |
| `cargo test -p scribe-cli --test cli_commands` | **9 passed, 0 failed**: real-binary e2e for all three commands |
| `cargo test -p scribe-cli --lib` | New unit tests: quickstart defaults/suffix/validation, clean path safety, check report counting |

**New files:** `crates/scribe-cli/src/{clean,check,quickstart}.rs`,
`crates/scribe-cli/tests/cli_commands.rs`.
**Modified:** `crates/scribe-cli/src/{lib,options}.rs`,
`crates/scribe-cli/Cargo.toml` (production deps on `scribe-ast`, `scribe-rst`,
`scribe-domains`), `STATUS.md`, `BENCHMARKS.md`, `CONFORMANCE.md`. Frozen §7
AST, §12 `BuildEnvironment` and §15 `Domain` public shapes are untouched; no
`.unwrap()`/`.expect()` was added to library code.

**Implemented command behaviour (measured with the real binary):**

- `scribe clean <path>` removes `_build` (including any configured
  `build_dir`/`outdir`/`output_dir`) and `.scribe-cache`, is idempotent (second
  run exits 0), refuses `/`, `.`, `..`, absolute/outside-root targets and plain
  files, and supports `--dry-run`. Source files are never removed.
- `scribe check <path>` loads `conf.py`/`scribe.toml` through the existing
  bridge, discovers sources through `scribe-core`, parses/resolves with
  `scribe-rst`, detects toctree cycles/missing entries through
  `collect_toctrees_with_sources`, and resolves/diagnoses broken
  cross-references through `scribe-domains::XrefResolver`. It emits no build
  assets and writes no cache. Exit 0 on success, 1 on errors; `-W` escalates
  warnings, `-n` is honoured as a documented compatibility flag (nitpicky
  reporting is always enabled in `check`).
- `scribe quickstart <path> [--batch]` generates `conf.py`, the master
  document, `Makefile`, `make.bat` and `.gitignore`, plus source/build
  directories. `--batch` is fully non-interactive; without it missing values
  are prompted for. Existing files are never overwritten without `--force`.

**Snapshot / VCS note:** the working tree still carries **148 uncommitted
files**; they are protected by the pre-existing safety snapshot
`stash@{0}: On main: pre-phase-31-snapshot`. Per explicit user instruction
**no git commits, tags, branches, `git add` or `git stage` were created** in
Phase 31, and the stash was not modified.

**Measurement honesty:** Phase 31 did **not** re-run the benchmark or
conformance harness. Every number in `BENCHMARKS.md` and `CONFORMANCE.md`
remains the Phase 30/29 measurement, unchanged; those figures are explicitly
not re-measured here.

## Phase 32 gate evidence (commands run, from `/home/credo/code/scribe`)

**Objective:** diagnose and eliminate superlinear peak-RSS scaling on the
150-page benchmark, make RSS scale linearly and drop strictly below Python,
and improve wall-clock throughput over the 6.38× Phase 30 figure.

**Profiling:** added an opt-in `SCRIBE_PROFILE=1` stage sampler
(`crates/scribe-builders/src/profile.rs`) reading `/proc/self/status` `VmRSS`/
`VmHWM`; no new deps and no production-path cost when unset. A release build on
the 151-page fixture attributed **+79.5 MB** to pass B: the per-page
`HtmlContext` cloned the whole `BuildEnvironment` and `Config` O(pages) times
and `prepared` held a second copy of every AST while doctrees were written only
at the very end.

**Changes (additive to frozen §7 AST / §12 `BuildEnvironment` / §15 Domain
contracts; serde/cache format unchanged):**
- `HtmlContext.config`/`HtmlContext.env` → `Arc<Config>`/`Arc<BuildEnvironment>`
  shared across all pages (one snapshot, not N clones).
- Pass B takes AST ownership out of `parsed` (`parsed.remove`) — one resident
  copy; each doctree is serialized to the disk cache immediately after its
  transforms, not retained until build end.
- Updated all three `HtmlContext` constructors (`html/builder.rs`, `html/mod.rs`
  `test_context`, `epub.rs`); no `.unwrap()`/`.expect()` added to library code.

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished`; 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (× 3 consecutive) | **1073 passed, 0 failed** each run |
| `cargo test --workspace --release` | **1073 passed, 0 failed** |
| `cargo test -p scribe-cli --test selfhost` | 1 passed (exit 0, 1359 warnings) |
| `cargo test -p scribe-cli --test regression_guard` | 5 passed |
| `cargo test -p scribe-cli --test conformance` | 2 passed; parity still 65.6% (CONFORMANCE.md unchanged) |
| `RUNS=3 PAGES=150 ./scripts/bench.sh` | scribe 0.22 s / 76,656 KB / 154 pages; sphinx 1.59 s / 92,640 KB / 153 pages → **7.23×** (2nd run 7.09×) |
| `RUNS=5 PAGES=40 ./scripts/bench.sh` | scribe 0.10 s / 35,796 KB / 44 pages; sphinx 0.62 s / 79,156 KB / 43 pages → **6.20×** |
| `/usr/sbin/hyperfine --warmup 1 --runs 10` @150p | scribe 108.3 ± 4.3 ms; sphinx 648.5 ± 20.4 ms |
| self-host release `-D html_theme=basic` | exit 0, 0.93 s / 193,716 KB, 158 `.html` files |

**Before → after (@150 pages):** peak RSS 116,292 → 76,656 KB (**−39,636 KB,
−34.1%**); wall 0.26 → 0.22 s (**−15%**); wall speedup 6.23× → **7.23×**.
Baseline re-measured this machine before the refactor (Phase 32 *before*),
superseding the Phase 30 numbers (6.38×).

**Exit gates:**
- Peak RSS @150 pages strictly < 92,600 KB (Python): **76,656 KB — PASS**.
- RSS scales approximately linearly: 40p 35,796 KB → 150p 76,656 KB; per-page
  overhead *falls* from ~629 KB to ~442 KB above the ~10 MB fixed floor —
  **PASS** (no superlinear growth).
- Wall speedup @150 pages measurably > 6.38×: **7.23× / 7.09× — PASS**.
- HTML correctness unchanged: 44/43 @40p, 154/153 @150p; self-host 158 `.html`
  with exit 0; conformance guard + regression guard green — **PASS**.

**VCS note:** per explicit user instruction **no git commits, tags, branches,
`git add` or `git stage` were created** in Phase 32; the pre-existing safety
snapshot `stash@{0}: On main: pre-phase-31-snapshot` was not modified. One new
scratch file (`.phase32-notes.md`, untracked) records the raw baseline numbers.

**Measurement honesty:** every before/after figure above was measured via the
exact `scripts/bench.sh`, hyperfine and `/usr/bin/time -v` commands shown;
`BENCHMARKS.md` was rewritten with those numbers. `CONFORMANCE.md`'s parity
number was left unchanged (65.6%); the aspirational ≥10× throughput target was
**not** reached — the measured improvement is 6.23× → 7.23×, which satisfies
the hard >6.38× gate.

## Phase 33 — intersphinx HTTP completion + cross-reference root causes

**Baseline re-measured on this machine (same release binary, before any Phase 33
edit):** `./target/release/scribe build -b html -c sphinx/doc -D html_theme=basic sphinx/doc /tmp/opencode/phase33-baseline-out`
→ exit 0, 155 pages, **1,359 warnings** (1,374 stderr lines containing
`warning`). Unresolved-xref subset (reproducible grep in
`/tmp/opencode/phase33-count.sh`) = **891**: 713 `reference target not found`,
63 `unknown document`, 115 `Failed to create a cross reference` (numref).

| Baseline `(domain, role)` | Count |
|---|---:|
| rst:dir | 194 |
| py:mod | 114 |
| py:class | 92 |
| py:meth | 74 |
| rst:role | 63 |
| confval | 49 |
| cpp:type | 31 |
| py:func | 30 |
| py:data | 19 |
| py:attr | 15 |
| cpp:class | 13 |
| py:exc / envvar / c:type / cpp:func | 4 each |
| cpp:var / c:var | 2 / 1 |

**Root causes fixed (all measured, not speculative):**

1. Domain objects were registered before `prepare_document`, so `py:fullname`
   / `rst:name` did not exist at registration time. Builders now expand every
   document before the registration pass; the read-phase `SphinxDomains`
   transform skips already-registered docs via `DomainRegistry::has_seen_doc`.
   rst:dir 194 → 41, rst:role 63 → 14 in the first isolated measurement.
2. `py:module` objects were keyed `module.module`; fixed in
   `PythonDomain::process_doc`. py:mod 114 → 87.
3. Leading `!` (upstream "never link, never warn") now short-circuits to the
   content node in `resolve_pending_xrefs`; leading `/` on `:doc:` targets is
   treated as project-root-relative in `StandardDomain::resolve_doc`.
4. `title_for_label` now titles standalone `Target` nodes from the following
   enumerable block (sibling, then nearest section by source span), eliminating
   the entire 115-warning "A title or caption not found" family.
5. Unprefixed Python directive spellings (`.. class::`, `.. function::`,
   `.. data::`, `.. attribute::`, `.. module::`, …) now register as domain
   aliases in `scribe-rst`; autodoc no longer overrides `attribute`/`method`
   with a content-rejecting handler; autodoc search roots include the parent of
   `sphinx/doc` so the `sphinx` package it documents is discoverable.
6. Registered `:type:` on object directives, a minimal `.. event::` directive,
   multi-alias `.. confval:: master_doc / root_doc`, and a partially-qualified
   Python xref suffix fallback.

**Intersphinx (Phase 33 task 2):** `FetchOptions` gained separate connect/read
timeouts, bounded exponential retry backoff, `User-Agent`, explicit redirect
policy, and opt-in stale-cache offline fallback (`InventoryCache::get_stale`);
`fetch_http` diagnostics now include the full error source chain. All failures
remain structured `IntersphinxError`s that callers turn into warnings.

**Final measurement (release binary, same exact command as baseline):**
`build succeeded, 443 warning(s)`, exit 0, 155 pages, 337 files.

| Metric | Baseline | Final | Reduction | Gate |
|---|---:|---:|---:|---|
| Total warnings | 1,359 | 443 | 67.4% | ≤ 544 — **PASS** |
| Unresolved xrefs | 891 | 298 | 66.6% | ≥ 60% — **PASS** |
| numref/no-title | 115 | 0 | 100% | — |
| unknown document | 63 | 2 | 96.8% | — |

Final unresolved `(domain, role)` grouping: py:meth 62, py:class 62, rst:dir
44, cpp:type 31, py:func 21, py:mod 14, py:data 13, cpp:class 13, rst:role 11,
confval 7, c:type 4, cpp:func 4, py:exc 3, py:attr 3, cpp:var 2, envvar 1,
c:var 1. Remaining cases are mostly external stdlib/Sphinx-API targets that
would require network-hosted inventories not available in this offline
environment, plus intentionally-unresolvable C++ template examples in the docs.

**New integration tests** (`crates/scribe-domains/tests/intersphinx_http.rs`,
6 tests, no new dependency): local `TcpListener` mock server covering success,
302 redirect, 404 degradation, read-timeout → stale-cache fallback, cache hit
within TTL / miss after TTL, and a cached real inventory fixture
(`tests/fixtures/sphinx_docs.inv`, captured from the repo's self-host build).
One existing test (`resolution.rs::cache_reuses_inventory_after_source_disappears`)
was adjusted to set `offline_fallback: false` for its expired-cache case to
reflect the new opt-in flag; its assertion is otherwise unchanged.

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished`, 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (× 3 consecutive) | **1,079 passed, 0 failed** each run |
| `cargo test --workspace --release` | **1,079 passed, 0 failed** |
| `cargo test -p scribe-cli --test conformance` | 2 passed; parity still 65.6% (`CONFORMANCE.md` unchanged) |
| `cargo test -p scribe-cli --test regression_guard` | 5 passed |
| `cargo test -p scribe-cli --test selfhost` | 1 passed (exit 0, 443 warnings) |
| `cargo test -p scribe-domains --test intersphinx_http` | 6 passed |

**VCS note:** per explicit user instruction **no git commits, tags, branches,
`git add` or `git stage` were created**; `stash@{0}: On main: pre-phase-31-snapshot`
was not modified.

**Measurement honesty:** every before/after figure above came from commands run
on this machine. The counter script is `/tmp/opencode/phase33-count.sh`; the
final stderr transcript is `/tmp/opencode/phase33-final-stderr.txt`. The
baseline transcript was captured at measurement time, but the harness restarted
mid-phase and cleared that scratch file; the baseline counts recorded in the
table above are the ones read from that transcript before the restart (and the
build itself reported "1359 warning(s)"). No number was estimated: the 1,359
figure was additionally confirmed by the pre-existing Phase 32 selfhost evidence.
`CONFORMANCE.md`'s 65.6% figure was left unchanged because the conformance suite
was re-run and still reports it.

---

## Phase 34 — AST node gaps + Sphinx warning-code / exit-semantics parity

**AST nodes (append-only, frozen §7):** added `TitleReference`, `Classifier`
and `OptionGroup` at the END of `scribe_ast::NodeKind` (serde-compatible).
`title_reference` replaces the former `inline classes="title-reference"`
workaround; definition-list classifiers are now real `<classifier>` nodes;
option lists gained the upstream `<option_group>` wrapper. The pseudoxml writer
picks the tags up via `tag_name()`. HTML5 now emits `<cite>`, `<span
class="classifier">` (closing `</dt>` from the last classifier) and the
docutils 0.23 `<dt><kbd><span class="option">…</span></kbd></dt>` structure with
`<var>` arguments and comma separators. LaTeX emits `\sphinxtitleref{}`,
`{[}…{]}`, `\item[…] ` and option commas. Other writers already had catch-all
arms and compile unchanged.

**Warning codes:** new `scribe_core::warnings` module maps stable `scribe::*`
codes onto canonical Sphinx `type.subtype` codes and implements
`sphinx.util.logging.is_suppressed_warning` verbatim (bare `type`, `type.*`,
exact `type.subtype`). Missing references map data-dependently to `ref.doc`,
`ref.term`, `ref.option`, `ref.python`, …; stable codes are never renamed.
Plain builder warning strings are classified without changing displayed text
(so conformance warning comparison is unaffected).

**Exit semantics:** both `scribe build` (all builders) and `scribe check` now
filter warnings through `suppress_warnings` *before* counting them and apply
`-W` (warnings → non-zero exit). `--keep-going` is accepted without changing
the final status, matching `sphinx-build 9.3.0`. Verified against the pinned
oracle on `sphinx/tests/roots/test-domain-py-xref-warning`.

**New tests (19):** 5 oracle-pseudoxml parity tests (three node families),
3 HTML5 parity tests against `sphinx-build -b html -D html_theme=basic`
fragments, 5 warning-code/suppression unit tests, 5 CLI `-W`/suppression
integration tests (oracle-compared) and 1 append-only serialization test.
Three existing assertions were strengthened to expect the real upstream nodes
(no assertion was weakened).

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished`, 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (× 3 consecutive) | **1,098 passed, 0 failed** each run |
| `cargo test --workspace --release` | **1,098 passed, 0 failed** |
| `cargo test -p scribe-cli --test conformance` | 2 passed; parity **66.7%** over 34 fixtures (was 65.6%) — `CONFORMANCE.md` regenerated, no baseline assertion weakened |
| `cargo test -p scribe-cli --test selfhost` | 1 passed (exit 0, **443** warnings unchanged) |
| `cargo test -p scribe-rst --test node_parity` | 5 passed (oracle byte-exact) |
| `cargo test -p scribe-builders --test node_html_parity` | 3 passed |
| `cargo test -p scribe-cli --test warning_semantics` | 5 passed (oracle-compared) |

**VCS note:** no commits, tags, branches, `git add` or `git stage` were created;
`stash@{0}: On main: pre-phase-31-snapshot` was not modified.

**Measurement honesty:** every figure above was produced by commands run on
this machine during Phase 34.

---

## Phase 35 — External HTML theme resolution (unmodified `sphinx/doc` self-host)

**Root cause (measured, not guessed):** the conf.py bridge *already* preserves
`html_theme_path` — a temporary `eprintln!` probe at the `HTML builder` theme
setup showed `get_theme("sphinx13")` resolving correctly with template dirs
`["sphinx/doc/_templates", "sphinx/doc/_themes/sphinx13", "<bundle>/themes/basic",
"sphinx/doc/_themes", "<bundle>/themes"]`. The fatal error came from the
*generated index pages*: `build_with_flavor` selected the bundled-only
`fallback_manager` for `write_index_pages` whenever `templates_path` was
non-empty (true for `sphinx/doc`, which sets `templates_path = ['_templates']`).
Those pages then rendered with `ctx.theme_name = "sphinx13"` against a manager
containing only the four bundled themes, producing
`theme 'sphinx13' inherits from 'sphinx13', which is not a loaded theme
(loaded: alabaster, basic, classic, nature)` and exit 1 **after** 155 pages had
been written. The misleading "inherits from itself" wording is just
`MissingParent { name, parent }` reporting the absent root theme as both child
and parent; `sphinx/doc/_themes/sphinx13/theme.toml` correctly says
`inherit = "basic"` — nothing was hard-coded for `sphinx13`.

**Changes:**
- `scribe-theme`: added a theme-aware `with_config_no_user_templates` manager
  (keeps `html_theme_path` + inheritance, skips user `templates_path`), made
  built-in themes win over same-named local directories (`add_named_theme`),
  and added low-precedence site-packages discovery (Python `sysconfig`
  `purelib`/`platlib`, `VIRTUAL_ENV`/`CONDA_PREFIX`, and a
  `SCRIBE_THEME_SITE_PATHS` override for deterministic tests) scanning nested
  layouts such as `furo/theme/furo/theme.conf` up to depth 3.
- `scribe-builders`: index/search/genindex/py-modindex pages now use the
  theme-aware fallback, so a user template minijinja cannot compile no longer
  silently swaps the project theme for `basic`. Added a strict missing-theme
  classifier: only a genuinely absent root theme warns and falls back to
  `basic`; a present-but-misconfigured theme (bad TOML, cycle, too many
  ancestors, missing *ancestor*) stays a hard error.
- New tests: 5 theme-discovery tests (local path inheriting a builtin,
  three-level external CSS/option chain, built-in precedence, cross-external
  cycle, nested site-packages discovery), 1 builder classifier unit test, and
  the Phase 35 self-host integration test (exit 0, no fatal error, ≥100 pages,
  custom theme markers `pageheader`, `sphinx-logo.svg`, `svg-arrow-right`,
  `<span>Sphinx</span>` in `index.html`, `_static/sphinx13.css` +
  `_static/sphinx-logo.svg` copied, custom layout in `search.html`).

**Exit gate (exact commands, run from the repo root):**

| Command | Exit | Evidence |
|---|---|---|
| `./target/debug/scribe build -b html -E -q -c sphinx/doc sphinx/doc /tmp/opencode/scribe-phase35-final` | **0** | 155 pages built, 158 `.html` files, 339 files written, 0 fatal errors; custom markers + assets present |
| `./target/debug/scribe build -b html -E -q -c sphinx/doc -D html_theme=basic sphinx/doc /tmp/opencode/scribe-phase35-override` | **0** | 158 `.html` files, 337 files written (override regression guard preserved) |

The unmodified build writes 2 more files than the override build — exactly the
custom theme's `sphinx13.css` and `sphinx-logo.svg` — showing the theme was
really used, not replaced by `basic`.

**Verification:**

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | `Finished`, 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (× 3 consecutive) | **1,106 passed, 0 failed** each run (1,099 baseline + 7 new) |
| `cargo test --workspace --release` | **1,106 passed, 0 failed** |
| `cargo test -p scribe-cli --test conformance` | 2 passed; parity **66.7% over 34 fixtures unchanged** (`CONFORMANCE.md` regenerated, no baseline assertion weakened) |
| `cargo test -p scribe-cli --test regression_guard` | 5 passed |
| `cargo test -p scribe-cli --test selfhost` | 2 passed (both override and unmodified builds exit 0, 158 pages) |
| `cargo test -p scribe-theme --test theme` | 15 passed (10 prior + 5 new) |

**Gate status: PASS** — `./sphinx/doc` builds cleanly with its **unmodified**
`conf.py`, no `-D html_theme=basic`, zero fatal errors, and the custom
`sphinx13` theme's templates/assets demonstrably used.

**VCS note:** no commits, tags, branches, `git add` or `git stage` were created;
`stash@{0}: On main: pre-phase-31-snapshot` was not modified.

**Measurement honesty:** every figure above was produced by commands actually
run on this machine during Phase 35. The transient `eprintln!` probe used to
locate the failure was removed before the final build.

---

## Phase 36 — final conformance push, 250k fuzz sweep & verified docs

**Objective:** push semantic parity to ≥80%, verify zero memory/span regressions
under a 250k-case fuzz sweep, and publish final verified benchmark/conformance
docs. Work was restricted to Phase 36; no commits were created.

### Conformance outcome (measured, honest)

`cargo test -p scribe-cli --test conformance` regenerated `CONFORMANCE.md` from
a real oracle/scribe campaign over the read-only 39-fixture selection.

| Phase | Parity | Pass / Partial / Fail / Skip |
|---|---:|---|
| 29–32 (baseline) | 65.6% | 3 / 25 / 6 / 5 |
| 33–34 | 66.7% | 3 / 26 / 5 / 5 |
| **36 (final)** | **70.8%** | **4 / 26 / 4 / 5** |

**The ≥80% exit gate was NOT met.** Measured final parity is **70.8%**, short
of the required 80.0% by 9.2 percentage points. No fixture was skipped to dodge
a divergence, no baseline assertion was weakened, no generated number was
hand-edited, and the harness/baseline files were treated as read-only for
scoring. The regression guard and determinism checks remain green.

### Real fixes made (each measured with a conformance re-run)

1. **`role` directive option spec** (`scribe-rst/src/directives.rs`): the
   directive now accepts the upstream option set (`:language:`, `:class:`,
   `:name:`) instead of `OptionSpec::empty()`. Removed spurious "unknown
   option" warnings on `test-smartquotes` and `test-reST-code-role`.
   *Impact:* `test-smartquotes` 0.502 → 0.676, `test-reST-code-role` 0.569 →
   0.719, `test-directives-admonition-collapse` 0.654 → **1.000 (pass)** via
   the companion `collapsible` fix.
2. **`collapsible` admonition option** (`scribe-rst/src/directives.rs`):
   switched from a strict `Choice(open, closed)` to `Unchanged`, matching
   upstream `directives.unchanged` so a bare `:collapsible:` is valid.
3. **Toctree diagnostics in Sphinx logger format**
   (`scribe-builders/src/build_common.rs`): the `not_included` / `secnum` /
   `circular` / `empty_glob` / `duplicate_entry` / `not_readable` /
   `excluded` warnings produced by the toctree collector are now surfaced with
   the canonicalized source path, `WARNING:` severity and `[toc.*]` code so
   their normalized lines match the oracle. *Impact:* `test-default_role`
   0.546 → 0.696, `test-toctree-glob` 0.322 → 0.472, `test-toctree` 0.547 →
   0.647, `test-domain-c` 0.747 → 0.809.
4. **`productionlist` content parsing** (`scribe-rst/src/resolve.rs` and
   `directive_registry.rs`): docutils folds an indented body into the single
   final argument when `final_argument_whitespace` is set and `has_content` is
   false; `split_arguments` now preserves the raw multi-line tail and the
   resolver rejoins body lines instead of emitting "directive does not accept
   content". *Impact:* `test-productionlist` 0.458 → 0.581 (fail → partial),
   structure 0.539 → 0.683.

### Remaining divergences (categorized, from the regenerated CONFORMANCE.md)

- **parser / structure** (29 fixtures): the dominant residual. The largest
  single cause is that pseudoxml output retains a raw `<toctree>` node while
  Sphinx resolves it into `compact_paragraph` → `bullet_list` → `list_item` →
  `reference` subtrees at post-transform time (`test-toctree`, `test-prolog`,
  `test-smartquotes`, `test-toctree-glob`). Doctree node-shape differences in
  domains, footnotes and tables account for the rest.
- **html classes** (28 fixtures): HTML DOM marker/class differences in the
  handful of `-b html` fixtures.
- **warning codes** (~20 fixtures): scribe's builder/domain diagnostics use
  `docname: message` text rather than Sphinx's
  `path:LINE: WARNING: message [code]` form, and several domain messages differ
  in wording (e.g. `cpp:class` vs `cpp:identifier`). Xref warnings with a source
  line number cannot yet match because the builder drops the diagnostic span
  before formatting. Only the toctree family above now matches exactly.
- **directive** (19 fixtures) / **domain** (15 fixtures): remaining directive-
  and domain-specific node-shape mismatches (numfig gating, glossary,
  production tokens as xrefs, JS short-name resolution, duplicate C/C++
  declarations).

These are genuine semantic gaps requiring a full toctree post-transform and a
structured (span-carrying) warning pipeline; they were not papered over.

### Fuzz sweep (measured)

Extended the property suite with two new properties:
`scribe-rst/tests/fuzz.rs::new_ast_node_constructs_have_valid_spans` (covers the
new `TitleReference` / `Classifier` / `OptionGroup` node-producing constructs)
and `scribe-domains/tests/fuzz.rs::inventory_loads_never_panics` (arbitrary-byte
`Inventory::loads`). The sweep ran `PROPTEST_CASES=10000` across **27 proptest
properties** (25 historical + 2 new):

| Suite | Properties | Cases | Result |
|---|---:|---:|---|
| scribe-config `fuzz` | 4 | 40,000 | green |
| scribe-rst `fuzz` | 8 | 80,000 | green |
| scribe-myst `fuzz` | 4 | 40,000 | green |
| scribe-myst `adversarial` | 5 | 50,000 | green |
| scribe-domains `fuzz` | 6 | 60,000 | green |
| **Total** | **27** | **270,000** | **0 panics, all spans in bounds** |

### Final verification (all commands run on this machine)

| Command | Result |
|---|---|
| `cargo check --workspace --all-targets` | 0 warnings / 0 errors |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (× 3 consecutive) | **1,108 passed, 0 failed** each run |
| `cargo test --workspace --release` | **1,108 passed, 0 failed** |
| `cargo test -p scribe-cli --test conformance` | 2 passed; parity 70.8%; report regenerated |
| `cargo test -p scribe-cli --test regression_guard` | 5 passed |
| Self-host (release), unmodified `sphinx13` theme | exit 0, 158 HTML pages |
| Self-host (release), `-D html_theme=basic` | exit 0, 158 HTML pages |

### Benchmarks (release, re-measured in Phase 36)

| Fixture | scribe wall / RSS | sphinx-build wall / RSS | Speedup | RSS gate |
|---|---|---|---:|---|
| 40 pages | 0.12 s / 35,296 KB | 0.65 s / 79,180 KB | 5.42× | — |
| 150 pages | 0.23 s / 76,824 KB | 1.61 s / 92,496 KB | 7.00× | **PASS** (< 92,600 KB) |

Reproduce with `cargo build --release -p scribe-cli && RUNS=5 PAGES=40
./scripts/bench.sh && RUNS=3 PAGES=150 ./scripts/bench.sh`.

### Gate status

| Gate | Requirement | Measured | Status |
|---|---|---:|---|
| Conformance parity | ≥ 80.0% | **70.8%** | **FAIL** |
| Fuzz panics | 0 across 250,000 cases | 0 across 270,000 | **PASS** |
| Peak RSS @150p | < ~92,600 KB | 76,824 KB | **PASS** |
| Release tests | 100% green, 0 warnings | 1,108 passed | **PASS** |

**Overall: FAIL** — only because of the parity gate. Per the phase contract,
the measured parity, fixes, remaining divergences and blockers are reported
rather than inflating the metric or relaxing the gate.

**Docs updated:** `CONFORMANCE.md` (regenerated by the test),
`BENCHMARKS.md`, `STATUS.md`, `ARCHITECTURE.md`.

**VCS note:** no commits, tags, branches, `git add` or `git stage` were created;
`stash@{0}: On main: pre-phase-31-snapshot` was not modified.

**Measurement honesty:** every figure above was produced by commands actually
run on this machine during Phase 36.
