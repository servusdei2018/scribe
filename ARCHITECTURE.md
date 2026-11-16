# scribe — Architecture

**Status:** Phase 01 (workspace architecture, CLI skeleton, upstream oracle harness).
**Oracle:** `/home/credo/code/scribe/sphinx` (Python Sphinx HEAD `035a07c3`), installed at
`/home/credo/code/scribe/.oracle-venv/bin/sphinx-build` (reports `sphinx-build 9.3.0+/035a07c32`).

All behavior claims in this document were taken from the upstream source
(`sphinx/sphinx/cmd/build.py`, `sphinx/sphinx/cmd/make_mode.py`, `sphinx/sphinx/cmd/quickstart.py`)
rather than guessed.

---

## 1. Workspace layout

```
scribe/
├── Cargo.toml                  # workspace manifest, shared package metadata, dependency slots, lints
├── ARCHITECTURE.md             # this document
├── STATUS.md                   # phase ledger and gate evidence
├── .oracle-venv/               # Python Sphinx oracle (gitignored)
├── sphinx/                     # read-only upstream checkout (gitignored)
└── crates/
    ├── scribe-cli/             # binary `scribe`; clap CLI + miette diagnostics + differential tests
    ├── scribe-core/            # shared result/error vocabulary; Phase 08 BuildEnvironment
    ├── scribe-ast/             # Phase 03 document AST (frozen contract)
    ├── scribe-rst/             # Phases 04-06 RST lexer/inline/directives
    ├── scribe-myst/            # Phase 07 MyST Markdown
    ├── scribe-config/          # Phase 02 config engine / conf.py bridge
    ├── scribe-domains/         # Phases 11-14 domains (frozen Domain trait)
    ├── scribe-builders/        # Phases 17-25 output builders
    ├── scribe-theme/           # Phase 16 templates/themes
    ├── scribe-autodoc/         # Phase 21 Python introspection
    └── scribe-cache/           # Phases 08/30 deterministic incremental cache
```

Every crate has a real `Cargo.toml` inheriting `[workspace.package]` and `[workspace.lints]`.
Only `scribe-cli` (clap, thiserror, miette) and `scribe-core` (thiserror) currently pull dependencies;
the rest are documented placeholders. All remaining dependency slots are declared once in
`[workspace.dependencies]` and will be opted into with `.workspace = true` by their owning phases.

### Crate responsibilities

| Crate | Responsibility | Inputs | Outputs |
|---|---|---|---|
| `scribe-cli` | Parse/validate CLI, dispatch subcommands, render diagnostics | argv | `BuildOptions`, diagnostics; later: build invocation |
| `scribe-core` | Errors, result alias, cross-cutting traits/environment | — | library vocabulary |
| `scribe-ast` | Arena document tree, node enum, `pending_xref` | source tokens | `Document` |
| `scribe-rst` | RST blocks, indentation, inline markup, roles, directives | text | AST nodes |
| `scribe-myst` | MyST Markdown parsing | text | AST nodes |
| `scribe-config` | `conf.py` bridge and `-D`/`-A` overrides | overrides/files | validated config |
| `scribe-domains` | Object registration, xref resolution | AST refs | resolved targets |
| `scribe-builders` | Writers for all output formats | transformed doctree | files on disk |
| `scribe-theme` | Template lookup/inheritance, static theme assets | config | rendered templates |
| `scribe-autodoc` | Python module introspection, napoleon docstrings | Python code | AST nodes |
| `scribe-cache` | Content digests and serialized doctrees | files/doctrees | cache artifacts |

### Dependency graph (planned, acyclic)

```
scribe-cli ──► scribe-core ──► thiserror
     │            ▲
     │            ├──► scribe-config ──► serde, toml
     │            ├──► scribe-ast (leaf; later serde)
     │            │      ▲        ▲        ▲
     │            │      │        │        └──► scribe-autodoc ──► scribe-domains
     │            │      │        └───────────► scribe-builders ──► scribe-theme
     │            │      └────────────────────► scribe-rst / scribe-myst
     │            └───────────────────────────► scribe-domains
     └──► clap, thiserror, miette        scribe-cache ──► blake3, bincode/rkyv
```

Rules: `scribe-ast` stays dependency-light (it is the shared currency); no crate may depend on
`scribe-cli`; `scribe-core` depends only on error primitives; cycle-prone pairs (transform engine ↔
domains) communicate through frozen contracts in `scribe-ast` and `scribe-domains`.

---

## 2. CLI flag mapping to `sphinx-build`

`BuildArgs` in `crates/scribe-cli/src/options.rs` mirrors `sphinx/cmd/build.py::get_parser()`
exactly. `BuildOptions` is the reusable, clap-independent model produced by `TryFrom<BuildArgs>`.

| sphinx-build flag | scribe form | Notes |
|---|---|---|
| `SOURCE_DIR OUTPUT_DIR [FILENAMES...]` | positional under `scribe build` | validated: filenames incompatible with `-a` |
| `--version` | `scribe build --version` | no `-V` short form in build.py; global `scribe -V/--version` is separate |
| `-b`, `--builder` (default `html`) | identical | |
| `-j`, `--jobs` (default `1`) | identical | accepts positive integer or `auto` (`available_parallelism`) |
| `-a`, `--write-all` | identical | |
| `-E`, `--fresh-env` | identical | |
| `-d`, `--doctree-dir` | identical (long name is `doctree-dir`) | default `OUTPUT_DIR/.doctrees` |
| `-c`, `--conf-dir` | identical (long name is `conf-dir`) | default `SOURCE_DIR` |
| `-C`, `--isolated` | identical | |
| `-D`, `--define setting=value` | identical, repeatable | validated `name=value` |
| `-A`, `--html-define name=value` | identical, repeatable (this is *not* `html-theme-option`) | |
| `-t`, `--tag` | identical, repeatable | |
| `-n`, `--nitpicky` | identical | |
| `-v`, `--verbose` | identical, counted | |
| `-q`, `--quiet` | identical | |
| `-Q`, `--silent` | identical | |
| `--color` / `-N`, `--no-color` | identical | mutually exclusive; resolves to `ColorChoice` |
| `-w`, `--warning-file` | identical | |
| `-W`, `--fail-on-warning` | identical | |
| `--keep-going` | identical | upstream uses `help=argparse.SUPPRESS`, so it is accepted but hidden in `--help` |
| `-T`, `--show-traceback` | identical | |
| `-P`, `--pdb` | identical | |
| `--exception-on-warning` | identical | |

Intentional omissions: `-M/--mode` and `-r/--release` live in `sphinx/cmd/make_mode.py` and
`sphinx/cmd/quickstart.py`, not the `sphinx-build` parser. They will be added only if a make-mode /
quickstart compatibility shim is introduced in a later phase.

`quickstart`, `clean` and `check` are deterministic, non-panicking placeholders: quickstart prints a
banner, clean removes `.scribe-cache` under the supplied path (or reports its absence), and check
reports that no source has been discovered yet.

---

## 3. Differential test harness

Location: `crates/scribe-cli/tests/differential.rs` plus
`crates/scribe-cli/tests/fixtures/basic_pseudoxml.markers`.

Flow:

1. Derive the workspace root from `CARGO_MANIFEST_DIR` (works regardless of cargo's CWD).
2. Locate `.oracle-venv/bin/sphinx-build`; fail loudly with the path if it is missing.
3. Run the fixture `sphinx/tests/roots/test-basic` with `-b pseudoxml -E -q` into a
   process-unique temporary output directory.
4. Capture exit status, stdout, stderr and the recursively collected list of generated files
   (`OracleCapture`).
5. Read `index.pseudoxml`, normalize the absolute source path to `<SOURCE>`, and assert that every
   marker in `basic_pseudoxml.markers` appears in the output.
6. A second test runs `sphinx-build --help` and asserts that the flags mirrored by our CLI are
   present (with `--keep-going` deliberately excluded because upstream suppresses it from help).

Phase 01 validates the *mechanism* only — oracle invocation, artifact capture, normalization and
comparison scaffolding. From the phase where scribe can emit a doctree, the same harness will build
the fixture with `scribe` and compare normalized pseudoxml against the oracle, reporting diffs with
`miette` source spans via the reserved `similar` dependency.

---

## 4. Error and diagnostic strategy

- Library crates use `thiserror` enums (`scribe_core::ScribeError`, `scribe_cli::ScribeCliError`).
- User-facing output uses `miette` with stable diagnostic codes:
  `scribe::cli::invalid_build_options`, `scribe::cli::missing_argument`,
  `scribe::unimplemented_builder`, `scribe::not_implemented`.
- `main` returns `miette::Result<()>`; `miette`'s `fancy` feature renders codes, messages and help.
- No `.unwrap()`/`.expect()` in library or production paths; they appear only in tests and are
  guarded by assertions. `unwrap_or` is used for the CPU-count fallback.

---

## 5. Frozen cross-phase contracts

These are owned by later phases and **must not change without orchestrator approval**:

- **Phase 03** — `scribe-ast::Document`, the node enum, the `u32` arena `NodeId`, and the
  `pending_xref` node shape. Consumed by Phase 10 transforms and Phase 11 domains.
- **Phase 08** — the `BuildEnvironment` public API (lives in `scribe-core`).
- **Phase 11** — the `Domain` trait with `resolve_xref` / `resolve_any_xref` (lives in
  `scribe-domains`), shared with the Phase 10 transform engine.

Phase 01 deliberately defines none of these types, so no premature shape can be baked in.
