# scribe

A production-grade Rust reimplementation of the [Sphinx](https://www.sphinx-doc.org/)
documentation engine. scribe reads the same reStructuredText / MyST sources
and `conf.py`, and exposes a `sphinx-build`-compatible command line — without
requiring Python at runtime for the common path.

> **Status:** phase 30 (final). 11 crates, 1055+ tests, 15+ builders, ten
> domains/parser families, a live-reload dev server and a conformance harness.
> See `STATUS.md` for the full phase ledger and `CONFORMANCE.md` for the
> measured 65.6% upstream semantic-parity metric. This is a tracked metric,
> not a claim of complete parity.

## Quickstart

```bash
# Build a project that already has a conf.py
cargo build --release -p scribe-cli
./target/release/scribe build -b html -c docs docs docs/_build/html

# Or install it
cargo install --path crates/scribe-cli
scribe build -b html docs docs/_build/html

# Development server with live reload
scribe serve docs            # alias: scribe watch docs
```

`scribe build` accepts the `sphinx-build` flag set (`-b`, `-j`, `-a`, `-E`,
`-d`, `-c`, `-C`, `-D`, `-A`, `-t`, `-n`, `-v/-q/-Q`, `-w`, `-W`, `-T`, …).
Run `scribe build --help` for the full list.

Hello-world project:

```rst
Hello
=====

This is a paragraph with *emphasis*, ``literal`` and a :doc:`link <other>`.

.. note::

   scribe renders admonitions, lists, code blocks, tables and toctrees.
```

```toml
# scribe.toml
project = "Hello"
extensions = []
master_doc = "index"
```

## Features

- **Parsers:** reStructuredText (blocks, inline roles/directives, tables,
  footnotes) and MyST Markdown, both lowering into one frozen AST.
- **Domains:** standard, Python, C, C++, JavaScript, reST, math, citation and
  changeset, with cross-reference resolution and `objects.inv` compatibility.
- **Builders:** HTML5 (`html`, `dirhtml`, `singlehtml`, `json`, `htmlhelp`,
  `qthelp`, `devhelp`), LaTeX/PDF, EPUB3, man, Texinfo, text, gettext, doctest,
  linkcheck, XML/pseudoxml/changes.
- **Tooling:** incremental BLAKE3 cache, BLAKE3-based deterministic assets,
  syntax highlighting (syntect), sandboxed minijinja theming, client-side
  search indexing, gettext i18n, an event bus with Python/Wasm interop, and a
  `notify`-based live-reload server.
- **Extensions:** native support for `autodoc`, `napoleon`, `autosummary`,
  `doctest`, `todo`, `ifconfig`, `extlinks`, `viewcode`, `graphviz`,
  `githubpages` and `duration`.

## Configuration

scribe auto-discovers `conf.py` (evaluated through a Python bridge with a
static fallback) or a native `scribe.toml`, preferring `conf.py`. Point `-c`
at the directory containing either file. `scribe.toml` uses flat TOML keys;
dict settings such as `[html_theme_options]` become TOML tables. See
`MIGRATION.md` for the full mapping and extension support matrix.

## Documentation

| File | Contents |
|---|---|
| `README.md` | This overview |
| `MIGRATION.md` | Moving a Python Sphinx project to scribe |
| `BENCHMARKS.md` | Measured wall-clock/RSS results, self-hosting outcome, methodology |
| `CONFORMANCE.md` | Machine-generated upstream parity report and regression baseline |
| `ARCHITECTURE.md` | Crate layout, frozen contracts and per-phase design notes |
| `STATUS.md` | Phase ledger, gate evidence and final program summary |

## Development

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release
```

Benchmarks are reproducible with `scripts/bench.sh`; the self-hosting smoke
test lives at `crates/scribe-cli/tests/selfhost.rs`. Release packaging for
Linux (glibc/musl x86_64 + aarch64), macOS (universal) and Windows (MSVC) is
defined in `scripts/release.sh` and `.github/workflows/release.yml`.

## License

Dual licensed under MIT OR Apache-2.0. The vendored English stop-word list and
minified Snowball stemmer JS retain their upstream attribution notices.
