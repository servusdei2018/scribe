# scribe real-world corpus benchmark

> Every number in this file was **measured on this machine** with
> `RUNS=3 ./scripts/corpus_bench.sh`. Wall-clock and peak RSS come from GNU
> `time -v`; values are the median of 1 unmeasured warmup + 3 measured cold
> builds per engine. Nothing is estimated or copied from upstream claims.

## Environment & toolchain

| Item | Value |
|---|---|
| Date | 2026-11-21 (UTC) |
| Kernel / arch | Linux 6.18.40.1-microsoft-standard-WSL2 x86_64 |
| CPU | AMD Ryzen 9 7900X 12-Core Processor, 12 logical CPUs |
| Memory | 31 GiB total |
| scribe | `scribe 2.0.0`, release profile `opt-level=3`, `lto="thin"`, `codegen-units=1` |
| Oracle | `sphinx-build 9.3.0+/035a07c32` from the vendored checkout, CPython 3.15.2 |
| Timer | GNU `/usr/bin/time -v` |
| Repositories | shallow (`--depth 1`) clones of the `main` branches, sparse-checked-out to their documentation trees |

## Methodology

- Each repository was cloned with `git clone --depth 1 --filter=blob:none --sparse`
  into `/tmp/scribe-corpus/` (gitignored) and sparse-checked-out to the docs
  directory listed below.
- Both engines ran `build -b html -E -q <srcdir> <out>` with the repository's
  **unmodified** `conf.py`; the output directory was deleted before every run.
  `-D html_theme=basic` was used only where a theme could not be resolved
  (recorded per-repo below).
- Warning counts are the number of `WARNING:` lines in each engine's output;
  page counts are the number of `.html` files emitted.
- The oracle venv was bootstrapped with the third-party extensions each
  project's `conf.py` requires (see "Extensions encountered" below) so that
  unmodified `conf.py` files could be built by both engines.

## Measured results

| Repository | Engine | Exit | Warnings | HTML pages | Median wall (s) | Peak RSS (KB) | Wall speedup |
|---|---|---:|---:|---:|---:|---:|---:|
| flask | scribe | 0 | 0 | 79 | 0.72 | 149,364 | **6.40×** |
| flask | sphinx-build | 0 | 1 | 80 | 4.61 | 171,044 | — |
| requests | scribe | 0 | 0 | 18 | 0.24 | 59,608 | **7.33×** |
| requests | sphinx-build | 0 | 10 | 27 | 1.76 | 114,748 | — |
| pytest | scribe | 0 | 506 | 260 | 1.36 | 378,872 | **15.11×** |
| pytest | sphinx-build | 0 | 0 | 290 | 20.55 | 302,512 | — |
| sqlalchemy | scribe | 0 | 75 | 184 | 1.24 | 301,764 | **55.90×** |
| sqlalchemy | sphinx-build | 0 | 23 | 184 | 69.31 | 934,408 | — |
| django | scribe | 0 | 151 | 684 | 4.39 | 900,724 | **5.82×** |
| django | sphinx-build | 0 | 0 | 684 | 25.56 | 454,528 | — |

Wall speedup is `sphinx-build median wall / scribe median wall`. Both engines
exited 0 on all five repositories.

### Per-run raw timings (seconds / peak RSS KB)

| Repository | Engine | Run 1 | Run 2 | Run 3 |
|---|---|---|---|---|
| flask | scribe | 0.72 / 149,656 | 0.70 / 149,088 | 0.73 / 149,364 |
| flask | sphinx-build | 4.57 / 170,576 | 5.43 / 171,044 | 4.61 / 171,116 |
| requests | scribe | 0.24 / 59,608 | 0.24 / 59,688 | 0.24 / 59,228 |
| requests | sphinx-build | 1.77 / 114,748 | 1.76 / 114,664 | 1.75 / 114,860 |
| pytest | scribe | 1.36 / 378,588 | 1.35 / 379,348 | 1.39 / 378,872 |
| pytest | sphinx-build | 20.60 / 302,512 | 20.23 / 302,692 | 20.55 / 302,304 |
| sqlalchemy | scribe | 1.24 / 301,764 | 1.21 / 301,996 | 1.38 / 301,616 |
| sqlalchemy | sphinx-build | 74.03 / 936,136 | 69.31 / 934,324 | 67.79 / 934,408 |
| django | scribe | 4.42 / 900,724 | 4.30 / 900,644 | 4.39 / 901,196 |
| django | sphinx-build | 25.56 / 454,444 | 26.17 / 454,528 | 25.50 / 455,304 |

Small drift between runs is ordinary wall-clock variance on this host.

## Extensions encountered in the wild

The oracle venv initially contained only Sphinx and its bundled dependencies.
The following third-party packages were installed so that the repositories'
unmodified `conf.py` files could be imported:

| Repository | Extensions / theme required | Installed from |
|---|---|---|
| flask | `pallets_sphinx_themes`, `sphinx_tabs.tabs`, `sphinxcontrib.log_cabinet` | PyPI |
| requests | `alabaster` (already bundled), local `Requests` theme | — |
| pytest | `furo`, `sphinx_removed_in`, `sphinx_inline_tabs`, `sphinxcontrib_trio`, `sphinxcontrib.towncrier`, `sphinx_issues`, `pygments_pytest` | PyPI |
| sqlalchemy | `zzzeeksphinx` (>= 1.6.4), `sphinx_paramlinks`, `sphinx_copybutton`, `changelog` | GitHub (`sqlalchemyorg/zzzeeksphinx`, `sqlalchemyorg/changelog`) + PyPI |
| django | local `djangodocs` theme (inherits `pydata_sphinx_theme`), local `github_links` extension, `django` | Django `main` branch (dev build) |

Notes:

- `zzzeeksphinx` on PyPI declares `sphinx<8.3`; the repository's `conf.py`
  requires `>=1.6.4`, which is only available from its Git repository. The
  local Sphinx 9.3.0 checkout was re-installed with `--no-deps` after each
  pip operation so the oracle stayed pinned to the vendored version.
- Django's `main` branch `conf.py` uses `django.VERSION.status`, which only
  exists in the development build; the released Django 6.1.3 exposes a plain
  tuple. The dev build was installed to let the unmodified `conf.py` succeed.
- No repository needed `-D html_theme=basic`: every theme resolved after the
  packages above were installed.

## Honest gaps

- **scribe warnings on real-world trees:** scribe reports 506 warnings on
  pytest, 75 on sqlalchemy and 151 on django. These are from directives,
  roles and domain objects supplied by third-party extensions that scribe
  does not execute (`sphinx_issues`, `towncrier`, `djangodocs` custom
  directives, SQLAlchemy's `changelog` extension, etc.). They are reported,
  not hidden; the engines are not semantically equivalent on these trees.
- **HTML page-count differences:** flask (79 vs 80), requests (18 vs 27) and
  pytest (260 vs 290) differ because scribe does not execute extension
  hooks that generate extra pages (autosummary stubs, issue/changelog
  pages, linkcode views). sqlalchemy (184) and django (684) match exactly.
- **Peak RSS:** scribe uses more memory on django (900 MB vs 450 MB) because
  it retains the whole AST for the largest corpus; this is a known trade-off
  of its parallel parse/render architecture, not a regression.
- **Oracle pytest error:** pytest's `sphinx-build` run emits one `ERROR:
  Command exited unexpectedly` from `towncrier` (the sparse checkout omits a
  changelog template) but still exits 0; this does not affect the timing.

## Reproduce

```bash
cargo build --release -p scribe-cli
RUNS=3 ./scripts/corpus_bench.sh [results.md]
```

The script is idempotent: it reuses existing shallow clones under
`/tmp/scribe-corpus/` and only clones a repository when its `conf.py` is
missing.
