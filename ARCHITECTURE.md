# scribe — Architecture

**Status:** Phase 03 (unified document AST & doctree model).
**Oracle:** `/home/credo/code/scribe/sphinx` (Python Sphinx HEAD `035a07c3`), installed at
`/home/credo/code/scribe/.oracle-venv/bin/sphinx-build` (reports `sphinx-build 9.3.0+/035a07c32`).

All behavior claims in this document were taken from the upstream source
(`sphinx/sphinx/cmd/build.py`, `sphinx/sphinx/cmd/make_mode.py`, `sphinx/sphinx/cmd/quickstart.py`,
`sphinx/sphinx/config.py`, `sphinx/sphinx/util/tags.py` and the standard builder/extension `setup()`
functions) rather than guessed.

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
Currently `scribe-cli` (clap, thiserror, miette), `scribe-core` (thiserror), `scribe-config`
(serde, serde_json, toml, thiserror) and `scribe-ast` (thiserror, serde, serde_json) pull dependencies;
the rest are documented placeholders. All remaining dependency slots are declared once in
`[workspace.dependencies]` and will be opted into with `.workspace = true` by their owning phases.
`scribe-ast` also has a `[workspace.dependencies]` entry so later phases can consume it via
`.workspace = true`.

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

- **Phase 03 — DEFINED & FROZEN in Phase 03.** `scribe-ast::Document`, `NodeKind`,
  the `u32` arena `NodeId`, and the `pending_xref` node shape. Consumed by Phases 04–07
  (parsers), 09–10 (toctree/transforms), 11–14 (domains/xrefs), 17–25 (writers). The exact
  public shapes are documented exhaustively in §7 below.
- **Phase 08** — the `BuildEnvironment` public API (lives in `scribe-core`).
- **Phase 11** — the `Domain` trait with `resolve_xref` / `resolve_any_xref` (lives in
  `scribe-domains`), shared with the Phase 10 transform engine.

Phase 01 deliberately defined none of these types, so no premature shape was baked in.

---

## 6. Phase 02 configuration API (`scribe-config`)

### Dependencies opted in

`serde` (derive), `serde_json`, `toml` and `thiserror` are now inherited with `.workspace = true`
in `crates/scribe-config/Cargo.toml`. `scribe-cli` depends on `scribe-config` through a new
`[workspace.dependencies]` entry (`scribe-config = { path = "crates/scribe-config" }`). No new
external crates were added to the workspace.

### Public types

| Type | Purpose |
|---|---|
| `ConfigValue` | Tagged enum `Str / Bool / Int / Float / List / Dict / Path / None` with typed accessors (`get_str`, `get_bool`, `get_list`, `get_dict`, `get_path`, `get_strings`, `to_json`, `display_string`). `Float` is included because upstream registers numeric settings such as `linkcheck_timeout` with `float` in the valid-type set. |
| `ConfigValueKind` | Discriminant of `ConfigValue`; implements `Display` (`str`, `bool`, `list`, ...). |
| `ConfigType` | Declared type of a registry key: `Str`, `Bool`, `Int`, `Float`, `List`, `Dict`, `Path`, `Enum { candidates }`, `Any`. |
| `ConfigDefault` | `Value(ConfigValue)` or `Computed(fn(&Config) -> ConfigValue)` for upstream callable defaults. |
| `ConfigEntry` | `{ name, default, value_type, rebuild, description }` plus `is_list`/`is_dict`. |
| `ConfigRegistry` | Ordered registry of known keys. `builtin()`, `get`, `contains`, `iter`, `register`, `coerce_override`. Defaults are transcribed from `sphinx/config.py` and the standard builder/extension `setup()` functions. |
| `Config` | Resolved configuration. `get`/typed getters, `set`, `set_explicit`, `apply_override`, `refresh_computed_defaults`, `iter`, `warnings`, `source`, `master_doc`, `source_suffixes`. Maintains the `master_doc`/`root_doc` and `copyright`/`project_copyright` aliases. |
| `Tags` | `new`, `with_tags`, `add`, `remove`, `has`, `contains`, `iter`, `eval_condition` (canonical, matching upstream), `eval` (alias). Evaluates `tag`, `not`, `and`, `or`, parentheses and Jinja conditional expressions with a hand-written tokenizer + recursive-descent parser; results are cached. |
| `TagsError` | `UnexpectedToken`, `ExpectedToken`, `ChunkAfterExpression`, `EmptyCondition`, `InvalidNode`, `Internal`. |
| `ConfigError` | `thiserror` enum: I/O, missing/unsupported config file, TOML/JSON parse, bridge failure/timeout, duplicate key, unknown override/key, dictionary override, invalid number/bool, static parse failure. |
| `ConfigWarning` / `ConfigWarningKind` | Non-fatal diagnostics (`UnknownKey`, `TypeMismatch`, `OverrideIgnored`, `NoConfigFile`, `PythonUnavailable`, `PythonExecutionFailed`, `UnserializableValue`, `StaticParseIssue`, `LanguageFallback`). `Display` renders `path:line: warning [kind]: message`. |
| `BridgeStrategy` | `Auto` (Python then static fallback), `Python`, `Static`. |
| `PythonBridgeOptions` | Interpreter path and timeout (default 15s). `default_python_interpreter()` resolves `SCRIBE_PYTHON`, the workspace oracle venv, ancestor search, then `python3`. |
| `BridgeOutcome` | Raw values + warnings + strategy actually used. |
| `LoadRequest` | `conf_dir`, `overrides`, `config_file`, `tags`, `strategy`, `python`. |

### Entry points

- `load_config(conf_dir, overrides, config_file) -> Result<(Config, Vec<ConfigWarning>), ConfigError>`
- `load_config_with(request) -> Result<(Config, Vec<ConfigWarning>), ConfigError>`
- `load_toml(path, registry)` — flat top-level keys or a nested `[config]` table.
- `parse_static_conf_py(path)` / `evaluate_conf_py(path, tags, options)` / `load_conf_values(...)`.

Auto-discovery prefers `conf.py`, then `scribe.toml`, in the supplied configuration directory.
Source suffix normalization mirrors `convert_source_suffix` (string/list → dict), and
`language = None` is rewritten to `en` with a warning, as upstream does. `-D` coercion follows
upstream `Config.convert_overrides`: booleans accept only `0`/`1`, lists are comma-split, integers
are parsed, dictionaries reject whole-value overrides (use `name.key=value`), enums pass through
except `0`/`1` when `False`/`True` are candidates, and callable defaults pass through as strings.

### Fidelity note: Tags constants

Upstream's `BooleanParser` recognises `true`/`True`, `false`/`False` and `none`/`None` as Jinja
`Const` nodes, but `Tags._eval_node` has no `Const` branch and raises
`TypeError: invalid node, check parsing`. The pinned oracle behaves identically, so scribe returns
`TagsError::InvalidNode` for constants (and for a conditional expression whose omitted `else`
branch is taken). `crates/scribe-config/tests/differential.rs` verifies this against the oracle.

### CLI integration

`scribe_cli::build_config(&BuildOptions)` converts `-D`, `-A` (as `html_context.<key>`), `-n`
(as `nitpicky=1`) and `-t` into a `LoadRequest`. `run_build` calls it, prints a summary of the
parsed values and warnings via `println!`/`eprintln!`, and surfaces load failures as the miette
diagnostic `scribe::config::load_failed`. Builders remain unimplemented until Phases 17-25.

---

## 7. Phase 03 document AST (`scribe-ast`) — FROZEN PUBLIC CONTRACT

The entire crate is a leaf dependency (only `thiserror`, `serde`, `serde_json`). Its public API
is re-exported from `scribe_ast`; no `.unwrap()`/`.expect()` exists in library code except the
documented invariants inside the test-support builder `Document::fixture_document()`.

### 7.1 Arena model

```rust
pub struct NodeId(pub u32);                 // Copy, Eq, Hash, Ord, serde
impl NodeId {
    pub const NULL: NodeId;                 // u32::MAX sentinel ("no node")
    pub const ROOT: NodeId;                 // NodeId(0)
    pub fn is_null(self) -> bool;
    pub fn index(self) -> u32;
}

pub struct Document {
    pub nodes: Vec<Node>,                   // arena; slot n == NodeId(n)
    pub root: NodeId,                       // always NodeId::ROOT
    pub source_name: Option<String>,
    pub metadata: DocumentMetadata,         // { docname, title, language }
    // private, serialized: parents: Vec<Option<NodeId>>, alive: Vec<bool>
}
```

Invariants (maintained by every structural method):

* The arena is **append-only with tombstones**: removal marks `alive[id] = false` and retains the
  slot, so an id is *never* reused or shifted. A held `NodeId` is therefore valid for the document's
  whole lifetime, even across arbitrary transform passes — this is the property the domain and
  writer phases rely on.
* `parents[id]` is the O(1) parent lookup; orphaned/removed nodes have `None`. It can be rebuilt
  with `Document::reindex_parents()` after a mutator edits `children` directly.
* The root (`NodeId(0)`) can never be removed or replaced (`AstError::InvalidMutation`).
* All fallible accessors return `AstResult<_>`; out-of-range ids give
  `AstError::InvalidNodeId { id, len }`, the sentinel gives `AstError::NullNodeId`. No panics.

Constructor & mutation API (all `AstResult`-returning unless noted):

* `Document::new(source: Option<String>)` — creates only the `document` root; when `source` is
  `Some`, sets the root's `source` attribute so pseudo-XML shows `<document source="…">`.
* `root() / root_mut() -> &Node / &mut Node`, `arena_len()`, `is_empty()`, `contains(id)`,
  `is_live(id)`.
* `get(id) / get_mut(id) -> &Node / &mut Node` (checked).
* `parent(id) -> Option<NodeId>`, `children(id) -> &[NodeId]`, `ancestors(id)` (iterator),
  `is_ancestor(a, b)`.
* `add(kind, parent)`, `add_node(node, parent)`, `new_text(text, parent)` — append to arena and
  optionally link as a child.
* `append_child`, `insert_child`, `remove_child`, `replace_children`, `splice_children`,
  `replace_node` (in-place, id preserved), `remove_node` (tombstone + subtree),
  `replace_with_children` (unwrap). All reject cycles and root removal.
* `preorder_ids()`, `postorder_ids()`, `subtree_ids(id)`, `iter()` (live nodes in id order),
  `reindex_parents()`.
* `Document::fixture_document()` — the canonical tree shared by the Rust and oracle tests (§7.6).

### 7.2 Node record & kinds

```rust
pub struct Node {
    pub kind: NodeKind,
    pub attrs: Attributes,        // typed, BTreeMap-backed (sorted keys)
    pub children: Vec<NodeId>,
    pub source: Option<SourcePos>,
    pub span: Option<Span>,
}

pub struct SourcePos { pub source: Option<String>, pub line: Option<u32>, pub offset: Option<u32> }
pub struct Span { pub start: u32, pub end: u32 }   // byte offsets, [start, end)
```

`NodeKind` is a `Copy + Eq + Hash` enum covering **all** standard Docutils nodes (document … generated)
and Sphinx addnodes (`toctree`, `pending_xref`, `pending_xref_condition`, the full `desc_*` / `desc_sig_*`
family, `versionmodified`, `seealso`, `highlightlang`, `only`, `glossary`, `productionlist`, `production`,
`compact_paragraph`, `centered`, `acks`, `hlist`, `hlistcol`, `tabular_col_spec`, `number_reference`,
`download_reference`, `literal_emphasis`, `literal_strong`, `manpage`). `NodeKind::tag_name()` returns
the exact upstream pseudo-XML tag (`bullet_list`, `pending_xref`, …); `Text` → `#text`. The aliases
`VersionAdded`/`VersionChanged`/`Deprecated` all render as `versionmodified` with a `type` attribute,
matching upstream's single `versionmodified` class. Convenience predicates: `is_admonition()`,
`is_inline()`, `is_version_modified()`, `version_type()`.

Text is stored as Docutils stores it: `NodeKind::Text` leaves carry their content in the `"text"`
string attribute. `Node::text(text)`, `Node::text_content()`, `Node::set_text()`, `Document::new_text()`
are the ergonomic surface. `Node::astext(&doc)` concatenates a subtree's text.

### 7.3 Typed attributes

```rust
pub enum AttributeValue {
    Str(String), Bool(bool), Int(i64), Float(f64), List(Vec<String>),
    NodeId(NodeId), NodeIds(Vec<NodeId>), Null,
}
pub struct Attributes(pub BTreeMap<String, AttributeValue>); // Deref to the map
```

`Attributes` offers `get_str`, `get_bool`, `get_int`, `get_list`, `get_node_id(s)` and matching
`set_*` helpers, plus the common aliases `Node::ids()`, `classes()`, `names()`. Wrong-type reads
yield `AstError::AttributeType`. `NodeId`/`NodeIds` values are internal cross references and are
**never** emitted as pseudo-XML attributes.

### 7.4 `pending_xref` — FROZEN typed shape

Exposed via `Node::pending_xref() -> Option<PendingXref>` and
`Node::pending_xref_mut() -> Option<PendingXrefMut>` (return `None` on non-`PendingXref` nodes).
The views are typed getters/setters over the node's attribute map. The following fields are the
frozen contract consumed by Phases 10–14:

| Field | Type | Attribute key | Notes |
|---|---|---|---|
| `refdomain` | `Option<String>` | `refdomain` | owning domain (`py`, `std`, …) |
| `reftype` | `Option<String>` | `reftype` | object type (`func`, `class`, …) |
| `reftarget` | `Option<String>` | `reftarget` | normalized target |
| `refexplicit` / `refspecific` / `refwarn` | `Option<bool>` | same | flags; pseudo-XML `"1"`/`"0"` |
| `refdoc` / `refid` / `refuri` / `reftitle` / `refname` | `Option<String>` | same | resolution inputs/outputs |
| `classes` | `Vec<String>` | `classes` | CSS classes |
| `inner` | `Option<NodeId>` | `inner` | explicit inner content node |
| `resolve_conditions` | `Vec<NodeId>` | `resolve_conditions` | child `pending_xref_condition` ids |

A `pending_xref_condition` node carries a string `condition` attribute (`"resolved"`, `"*"`, …);
`Node::condition()` reads it. Setting a typed field to `None` removes the attribute entirely.

### 7.5 Traversal, mutation & serialization

* `Visitor`: `visit_node(&Node, NodeId)` (pre-order) + `depart_node(&Node, NodeId)` (post-order).
  `walk(doc, &mut v)` does a walkabove; `walk_preorder` / `walk_postorder` call only one hook.
* `Mutator`: `mutate(&mut Node, NodeId) -> AstResult<Mutation>`; `transform(doc, &mut m)` snapshots
  the live ids, lets the mutator edit the node in place, then applies a structural `Mutation`
  (`None`, `ReplaceWith(Node)`, `Remove`, `ReplaceChildren`, `SpliceChildren`,
  `ReplaceWithChildren`) after the borrow ends. `apply_mutation` applies one change. Ids never move.
* `Document::to_json() / from_json()` — serde JSON, exact round-trip (including arena ids).
* `Document::to_pseudoxml()` — Docutils/Sphinx pseudo-XML (§7.6).
* `Document::to_xml()` — optional deterministic XML with entity escaping.

`AstError` variants: `InvalidNodeId`, `NullNodeId`, `NotLive`, `InvalidMutation`, `AttributeType`,
`Serialization`; alias `AstResult<T>`.

### 7.6 Pseudo-XML fidelity & differential testing

Oracle investigation (read-only): `sphinx/addnodes.py`, Docutils `nodes.py`
(`Element.pformat`, `starttag`, `serial_escape`, `pseudo_quoteattr`), `sphinx/writers/xml.py`
(`PseudoXMLWriter.translate` → `document.pformat()`), and the writer node-walk patterns. The
emitter reproduces: 4-space indentation, **start tags only** (no end tags), alphabetically sorted
attributes, `Str`→`"v"`, `Bool`→`"1"`/`"0"`, `Null`→`"True"`, lists→space-joined after
`serial_escape` (`\`→`\\`, space→`\ `), empty `ids`/`classes`/`names`/`dupnames` omitted, and text
nodes as indented `str.splitlines()` lines. Internal `NodeId` attributes are skipped.

Tests in `crates/scribe-ast/tests/`:

* `pseudoxml.rs::snapshot_fixture_matches_oracle` — hardcoded expected string captured from the
  oracle command recorded in the test file.
* `pseudoxml.rs::differential_against_python_pformat_script` — runs
  `.oracle-venv/bin/python crates/scribe-ast/tests/fixtures/gen_pseudoxml.py` (builds the same tree
  as `fixture_document()` and prints `document.pformat()`) and asserts byte equality.
* `pseudoxml.rs::differential_against_real_sphinx_build` — runs
  `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q <tiny-src> <out>`, parses the resulting
  pseudoxml back into our AST with a small hand-written parser, re-emits, and asserts exact equality.
* `roundtrip.rs` — `oracle pseudoxml → AST → pseudoxml` and
  `oracle pseudoxml → AST → JSON → AST → pseudoxml` fixpoints, plus arena-id survival across JSON.

Round-trip subset (documented limitation of the lossy pseudo-XML format): element start tags,
indented text lines, and string/bool/list attributes whose types are inferred from a fixed
attribute-name table; text content never begins with four or more spaces. Full Docutils XML
(`to_xml()`) is generated, not parsed.
