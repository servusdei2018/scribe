# Migrating a Python Sphinx project to scribe

scribe is a Rust reimplementation of the Sphinx documentation engine. It
reads the same `.rst`/`.md` sources and a `conf.py`, and exposes a
`sphinx-build`-compatible CLI. This guide describes what transfers unchanged,
what needs a `scribe.toml`, and where the two engines still diverge.

> **Honest framing:** scribe covers the parser, the standard/py/c/cpp/js/rst
> domains, the core builders and a set of bundled extensions. It does **not**
> execute arbitrary Python extensions. See `CONFORMANCE.md` for the measured
> per-fixture parity (65.6% over 34 applicable fixtures) and `BENCHMARKS.md`
> for the self-hosting outcome. Treat a migration as "try it, diff the
> warnings", not as a guaranteed byte-for-byte swap.

## 1. Quick start

```bash
# Build with the existing conf.py and sources
scribe build -b html -c docs docs docs/_build/html

# Convert once to a native config (optional)
#   - rename conf.py -> scribe.toml using the mapping in section 2
scribe build -b html docs docs/_build/html
```

No Python is required at runtime for the common path. `scribe` auto-discovers
`conf.py` first, then `scribe.toml`, in the `-c` directory (default: the source
directory). If no config file is found, built-in defaults are used.

## 2. Configuration: `conf.py` compatibility and `scribe.toml`

### conf.py

`conf.py` is supported through a bridge with two strategies:

1. **Python bridge** — if a usable interpreter is available, `conf.py` is
   evaluated and its namespace serialized to JSON. `SCRIBE_PYTHON` overrides
   the interpreter. This handles computed values such as
   `release = __display_version__`.
2. **Static fallback** — if Python is unavailable, times out, or fails,
   simple assignments are parsed directly from the source.

Values that cannot be serialized (modules, functions, compiled patterns) and
keys scribe does not know are reported as **warnings**, not errors, and are
skipped. This is why a build of `sphinx/doc` reports `coverage_*`, `epub_*`,
`html_additional_pages`, `html_use_opensearch`, `latex_use_xindy` and similar
keys as unknown. They are harmless for scribe builds.

`-D key=value`, `-A key=value` and `-n` map onto the same override mechanism
as upstream: `-A` becomes `html_context.<key>`, `-n` becomes `nitpicky=1`.

### scribe.toml

A native TOML config avoids the Python dependency entirely. It accepts flat
top-level keys (recommended) or an equivalent `[config]` table. Nested dict
settings become TOML tables. Example:

```toml
project = "My Project"
author = "The Authors"
release = "1.2.3"
version = "1.2"
extensions = ["sphinx.ext.autodoc", "sphinx.ext.todo"]
source_suffix = ".rst"
master_doc = "index"
language = "en"
nitpicky = false
exclude_patterns = ["_build"]

[html_theme_options]
sidebar_width = "300px"
show_powered_by = false
```

Mapping notes:

- `copyright` / `project_copyright` and `master_doc` / `root_doc` are aliases,
  as in Sphinx.
- Dict-valued config (`html_context`, `html_theme_options`, `latex_elements`,
  …) is written as a TOML table.
- Values the registry does not recognize are warned about and ignored.
- `language = None` is normalized to `"en"` (with a warning), matching Sphinx.

You can keep both files: scribe prefers `conf.py`. Remove or rename it to use
`scribe.toml`.

## 3. Extensions

### Implemented natively

These `sphinx.ext.*` names are recognized and run in Rust with no Python
dependency:

| Extension | Notes |
|---|---|
| `sphinx.ext.autodoc` | Native static path via `rustpython-parser`; dynamic Python worker fallback |
| `sphinx.ext.napoleon` | Google and NumPy docstring conversion |
| `sphinx.ext.autosummary` | Stub generation + autosummary builder |
| `sphinx.ext.doctest` | `doctest` builder executes examples when a Python interpreter is available |
| `sphinx.ext.todo` | `.. todo::` / `.. todolist::` |
| `sphinx.ext.ifconfig` | Config-driven conditional content |
| `sphinx.ext.extlinks` | Hardcoded external-link roles |
| `sphinx.ext.viewcode` | `[source]` links for Python objects |
| `sphinx.ext.graphviz` | `dot` rendering when the external `dot` binary is available |
| `sphinx.ext.githubpages` | `.nojekyll` / `CNAME` emission |
| `sphinx.ext.duration` | Per-document timing report |

### Partial / library-level support

- `sphinx.ext.intersphinx` — `objects.inv` v2 reading/writing, inventory
  caching and xref resolution exist in `scribe-domains`, but automatic remote
  inventory fetching is not fully wired into the default build. External
  references may remain as "target not found" warnings.
- `sphinx.ext.mathjax` — math nodes are emitted; a JavaScript math renderer
  is not bundled.

### Not supported

- Any third-party Python extension that relies on the Sphinx Python API at
  runtime (a JSON-RPC/Wasm interop bridge exists — see `ARCHITECTURE.md` §33 —
  but it is not a drop-in replacement for arbitrary extensions).
- `sphinx.ext.coverage`, `sphinx.ext.inheritance_diagram`, and extensions that
  require Python import machinery or graph backends beyond `dot`.

Unknown extension names produce an upstream-style
`extension <name> not found` warning and are skipped; the build continues.

## 4. Builder mapping

All builders below are dispatched by `scribe build -b <name>`.

| sphinx-build builder | scribe | Output |
|---|---|---|
| `html` | ✅ | Standalone HTML5 pages |
| `dirhtml` | ✅ | One directory per page |
| `singlehtml` | ✅ | Single concatenated HTML file |
| `json` / `htmlhelp` / `qthelp` / `devhelp` | ✅ | HTML-family variants |
| `latex` / `pdf` | ✅ | LaTeX sources (`pdf` uses `latexmk` when present) |
| `epub` / `epub3` | ✅ | EPUB3 archive |
| `man` | ✅ | Groff manpage |
| `texinfo` | ✅ | Texinfo source |
| `text` | ✅ | Plain text |
| `gettext` | ✅ | POT/PO catalogs + GNU `.mo` pipeline |
| `doctest` | ✅ | Doctest report |
| `linkcheck` | ✅ | Link report (network access required for real checks) |
| `xml` / `pseudoxml` / `changes` | ✅ | Doctree/XML/changes output |
| `htmlhelp`-adjacent make-mode targets (`-M`) | ❌ | No `-M` / make-mode shim |
| Custom Python builders | ❌ | Not supported |

The `quickstart`, `clean` and `check` subcommands are deterministic
placeholders; `scribe serve` (alias `watch`) provides live reload and is
scribe-specific.

## 5. CLI flag differences

`build` mirrors `sphinx-build`'s flags essentially flag-for-flag:

`-b`, `-j` (`auto` supported), `-a`, `-E`, `-d`, `-c`, `-C`, `-D`, `-A`, `-t`,
`-n`, `-v`, `-q`, `-Q`, `--color`/`-N`, `-w`, `-W`, `--keep-going`, `-T`, `-P`,
`--exception-on-warning`, plus positional `SOURCE_DIR OUTPUT_DIR [FILENAMES…]`.

Differences to be aware of:

- `-M/--mode` and `-r/--release` live in Sphinx's `make_mode.py` /
  `quickstart.py`, not in `sphinx-build`, and are not implemented.
- `-A` is `html_context.<key>` (as in upstream `sphinx-build`), **not** an
  HTML-theme-option setter. Use `scribe.toml`'s `[html_theme_options]` for
  theme settings.
- `-q` suppresses the build summary but config parsing warnings are still
  emitted; `-Q` is fully silent.
- `scribe serve` / `scribe watch` are additions with no `sphinx-build`
  equivalent.
- Diagnostics use `miette` codes such as `scribe::builder::failed` rather than
  Sphinx's traceback format.

## 6. Known divergences

- **Doctree/DOM output is not byte-identical to Sphinx.** `CONFORMANCE.md`
  tracks the measured 65.6% semantic parity and lists every divergent fixture
  and category (parser, directive, domain, HTML classes, warning codes).
- **Warning codes and counts differ.** Scribe emits far fewer warning types;
  a `-W` build can therefore succeed where Sphinx fails, or vice versa.
- **Custom themes are limited.** Four themes are bundled (`basic`, `classic`,
  `nature`, `alabaster`). A custom theme that inherits from a non-bundled
  theme fails with a theme-resolution error. Override with
  `-D html_theme=basic` as a workaround (see the `sphinx/doc` self-hosting
  run in `BENCHMARKS.md`).
- **Python-only features** (arbitrary extensions, true autodoc import of
  native modules, graph backends other than `dot`) are unavailable or
  partial.
- **AST fidelity gaps:** the frozen AST has no `title_reference`,
  `Classifier` or `OptionGroup` node; writers map these to equivalent inline
  forms. See `ARCHITECTURE.md` §7–§9.

For each of these, the authoritative, machine-generated list of affected
fixtures is in `CONFORMANCE.md`.
