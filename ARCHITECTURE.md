# scribe — Architecture

**Status:** Phase 30 (performance benchmarking, packaging, docs & release readiness; Phases 01–29 intact).
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
    ├── scribe-rst/             # Phase 04 block parser; Phases 05-06 inline/directives (§8)
    ├── scribe-myst/            # Phase 07 MyST Markdown
    ├── scribe-config/          # Phase 02 config engine / conf.py bridge
    ├── scribe-domains/         # Phases 11-14 domains (frozen Domain trait)
    ├── scribe-builders/        # Phases 17-25 output builders
    ├── scribe-theme/           # Phase 16 templates/themes
    ├── scribe-autodoc/         # Phase 21 Python introspection
    └── scribe-cache/           # Phases 08/30 deterministic incremental cache
```

Every crate has a real `Cargo.toml` inheriting `[workspace.package]` and `[workspace.lints]`.
Currently `scribe-cli` (clap, thiserror, miette, scribe-config, scribe-core), `scribe-core`
(thiserror, serde, serde_json, blake3, bincode, regex, walkdir, rayon, scribe-ast, scribe-config,
scribe-cache, unicode-normalization), `scribe-cache` (serde, blake3, bincode, thiserror,
parking_lot), `scribe-config`
(serde, serde_json, toml, thiserror), `scribe-ast` (thiserror, serde, serde_json) and
`scribe-rst` (thiserror, miette, serde, serde_json, once_cell, regex, scribe-ast) pull dependencies;
the rest are documented placeholders. All remaining dependency slots are declared once in
`[workspace.dependencies]` and will be opted into with `.workspace = true` by their owning phases.
`scribe-ast` also has a `[workspace.dependencies]` entry so later phases can consume it via
`.workspace = true`.

### Crate responsibilities

| Crate | Responsibility | Inputs | Outputs |
|---|---|---|---|
| `scribe-cli` | Parse/validate CLI, dispatch subcommands, render diagnostics | argv | `BuildOptions`, diagnostics; later: build invocation |
| `scribe-core` | Errors, result alias, frozen `BuildEnvironment`, discovery, globs, transform pipeline | config + source tree | rebuild set, cached inventory, transformed doctree |
| `scribe-ast` | Arena document tree, node enum, `pending_xref` | source tokens | `Document` |
| `scribe-rst` | RST blocks, indentation, inline markup, roles, directives | text | AST nodes |
| `scribe-myst` | MyST Markdown parsing | text | AST nodes |
| `scribe-config` | `conf.py` bridge and `-D`/`-A` overrides | overrides/files | validated config |
| `scribe-domains` | Object registration, xref resolution | AST refs | resolved targets |
| `scribe-builders` | Writers for all output formats | transformed doctree | files on disk |
| `scribe-theme` | Template lookup/inheritance, static theme assets | config | rendered templates |
| `scribe-autodoc` | Python module introspection, napoleon docstrings | Python code | AST nodes |
| `scribe-cache` | BLAKE3 digests, versioned typed store, atomic writes | files/snapshots | deterministic cache artifacts |

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
- **Phase 08 — DEFINED & FROZEN in Phase 08.** The `BuildEnvironment` public API (lives in
  `scribe-core`): fields, `UpdateOptions` / `UpdateReport`, dependency / inclusion edges,
  persistence and cache format. Documented exhaustively in §12 below.
- **Phase 11 — DEFINED & FROZEN in Phase 11.** The `Domain` trait with
  `resolve_xref` / `resolve_any_xref`, the `DomainRegistry`, the owned
  `ResolvedReference` / `ObjectEntry` / `DomainData` values and the
  `DomainEnv` adapter (all in `scribe-domains`), shared with the Phase 10
  transform engine. Documented exhaustively in §15 below. The frozen §7 AST
  and §12 `BuildEnvironment` public shapes are untouched; the latter gains an
  *additive private* domain-data side table with new accessors and a new
  snapshot field (`ENV_FORMAT_VERSION` bumped `1 → 2`).

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
---

## 8. Phase 04 RST lexer & block parser (`scribe-rst`)

### 8.1 Upstream sources inspected

All rules below were extracted from the pinned checkout/venv, not guessed:

* `sphinx/sphinx/parsers.py` — `RSTParser.parse()` uses
  `statemachine.string2lines(..., tab_width=document.settings.tab_width,
  convert_whitespace=True)` and `RSTParser.decorate()` calls
  `_prepend_prologue()` / `_append_epilogue()`.
* `sphinx/sphinx/util/rst.py` — `_prepend_prologue()` inserts the prologue
  *after* a leading docinfo field list; `_append_epilogue()` appends a blank
  line then the epilogue.
* `.oracle-venv/.../docutils/statemachine.py::string2lines` — tabs expand to
  the next multiple of `tab_width` (default 8), `\v`/`\f` become spaces, every
  line is right-stripped, `str.splitlines()` line endings.
* `.oracle-venv/.../docutils/parsers/rst/states.py` — `Body.patterns` and the
  `Body` / `Text` / `Line` / `Explicit` / specialized-list state methods
  (section hierarchy via `title_styles`, `make_id`, `classifier_delimiter`,
  `parse_option_marker`, `split_attribution`, `::` literal introducers,
  transition/overline rules).
* `.oracle-venv/.../docutils/parsers/rst/tableparser.py` — the grid-table
  corner-walking algorithm and simple-table column/row detection.
* `.oracle-venv/.../docutils/nodes.py::make_id` — slugification (digraph and
  single-character translate tables, NFKD + ASCII-ignore, `[^a-z0-9]+` → `-`,
  trim leading digits/hyphens and trailing hyphens).

### 8.2 Public API

| Item | Purpose |
|---|---|
| `Lexer::new(&str)` / `Lexer::with_tab_width(&str, usize)` | zero-copy, span-tracking line scanner; `scan() -> Vec<SourceLine>` |
| `SourceLine` | 1-based `number`, original `raw` slice, normalized `text` (`Cow<str>`), byte `start`/`end`/`next`, `eol`, `indent_cols`, `indent_bytes`, `blank`; `content()`, `span()`, `content_span()`, `adornment()` |
| `adornment_of`, `is_adornment_char`, `column_width` | Docutils `Body.patterns['line']` detection and `column_width()` approximation |
| `parse(&str) -> ParseOutput` | block parser, default options |
| `parse_with_options(&str, ParseOptions) -> ParseOutput` | block parser with source name, tab width, `rst_prolog` / `rst_epilog` |
| `try_parse_document(&str) -> RstResult<Document>` | convenience: first diagnostic becomes an error |
| `ParseOptions { source_name, tab_width, prolog, epilog }` | parser inputs; `tab_width == 0` means Docutils default 8 |
| `ParseOutput { document, diagnostics }` | best-effort doctree plus recoverable diagnostics; `list_is_tight(NodeId)`, `tight_lists()`, `into_document()`, `has_diagnostics()` |
| `RstError` / `RstDiagnostic` / `Severity` / `named_report` | `thiserror` + `miette::Diagnostic` errors, byte spans, help labels, source-name-aware reports |
| `pending::{PendingDirective, fill_pending, ...}` | typed view and constants for the frozen Phase 06 `Pending` contract (§8.5) |
| `inline::{parse_inline, materialize_inline, InlineSource, InlineNode, XrefMeta}`, `parse_inline_placeholder`, `InlineContext` | Phase 05 inline parser (§8.4, §9) |

The lexer borrows the source whenever a line needs no normalization and allocates
only for lines containing tabs, form feeds or trailing whitespace. All node
`Span`s and miette `SourceSpan`s are byte offsets into the original (or
prolog/epilog-augmented) source, never into normalized lines.

When `prolog`/`epilog` are supplied, `parse_with_options` builds the augmented
source first (prologue after leading docinfo fields, blank-line separated;
epilogue appended after a blank line) and every span refers to that augmented
text. This mirrors `sphinx.util.rst` so Phase 06/later build env can inject
`rst_prolog`/`rst_epilog` verbatim.

### 8.3 Block construct coverage

| Construct | AST nodes | Notes |
|---|---|---|
| Section headers | `Section`, `Title` | underline and overline+underline; hierarchy by first-seen adornment char; ids via Docutils `make_id`, names via `fully_normalize_name`; duplicate ids get `-1`, `-2`, … plus an INFO diagnostic |
| Paragraphs | `Paragraph` + raw `Text` | blank-line separation; `::` introducers collapse to `:` |
| Transitions | `Transition` | uniform ASCII punctuation, ≥4 chars, blank-line delimited |
| Bullet lists | `BulletList(bullet)`, `ListItem` | `- + * • ‣ ⁃`; nested lists; tight/loose computed in a side table (`ParseOutput::list_is_tight`) since the frozen AST has no attribute for it |
| Enumerated lists | `EnumeratedList(enumtype, prefix, suffix, start)` | arabic / lower+upper alpha / lower+upper roman / `#`; `parens`, `rparen`, `period` formats |
| Definition lists | `DefinitionList`, `DefinitionListItem`, `Term`, `Definition` | classifiers split on Docutils ` +: +`; see frozen-AST gap below |
| Field lists | `FieldList`, `Field`, `FieldName`, `FieldBody` | inline and indented bodies |
| Option lists | `OptionList`, `OptionListItem`, `Option`, `OptionString`, `OptionArgument(delimiter)`, `Description` | short/long, `=`/space/attached arguments; see frozen-AST gap below |
| Line blocks | `LineBlock`, `Line` | `\| ` with nesting by post-bar indentation |
| Block quotes | `BlockQuote`, `Attribution` | indented blocks; `--`/`---`/em-dash attribution after a blank line |
| Literal blocks | `LiteralBlock(xml:space="preserve")` | introduced by `::`, dedented, internal whitespace preserved |
| Doctest blocks | `DoctestBlock(xml:space="preserve")` | `>>> ` runs |
| Explicit markup | `Pending`, `Comment`, `Target`, `SubstitutionDefinition` | see §8.5 |
| Grid tables | `Table`, `Tgroup(cols)`, `Colspec(colwidth)`, `Thead`, `Tbody`, `Row`, `Entry(morerows/morecols)` | full corner-walking algorithm incl. row/column spans and head separator |
| Simple tables | same node set | top/bottom borders, header separator, column spans, unbounded rightmost column |
| Footnotes / citations | `Pending` with `directive_name = "footnote"/"citation"` | raw payload; resolved in Phase 06 |

### 8.4 Phase 05 inline seam — FINAL INTERFACE

Phase 05 replaced the Phase 04 placeholder with a real recogniser (§9). The
seam changed signature/call sites as permitted. The stable public surface is:

```rust
pub mod inline;
pub use inline::{
    parse_inline, materialize_inline, InlineSource, InlineNode, InlineOptions,
    InlineParseResult, InlineWarning, MaterializedInline, XrefMeta,
};
pub use inline_seam::{parse_inline_placeholder, InlineContext};
```

`InlineContext { line, span, parent, source_name }` is the block parser's
hand-off; `parse_inline_placeholder` is kept as a narrow convenience wrapper
that parses a single contiguous string anchored at `span.start` and returns
detached `InlineNode`s. The block parser itself uses the richer path:
`BlockParser::inline_source()` builds an `InlineSource` (text + absolute byte
offset table), `parse_inline()` is a pure recogniser, and
`materialize_inline()` inserts arena nodes and returns the xref side table.
`ParseOutput::inline_xrefs()` exposes that table to Phases 06/11. The details,
including span semantics and the role payload contract, are in **§9**.

### 8.5 `Pending` node attribute contract (Phase 06)

Every `.. name:: ...` construct emits a `NodeKind::Pending` node with the
following attributes. These key names are a **frozen contract** between
Phase 04 and Phase 06; `pending::{KEY_*, fill_pending, PendingDirective,
as_pending_directive}` are the typed accessors.

| Key | `AttributeValue` | Meaning |
|---|---|---|
| `directive_name` | `Str` | lower-cased, whitespace-normalized directive name (`"note"`, `"code-block"`, …) |
| `arguments` | `Str` | raw argument string; falls back to the first contiguous non-blank content line when no inline argument is present |
| `options` | `Str` | JSON object (`BTreeMap<String, String>`) of `:name: value` option lines after the arguments |
| `content` | `Str` | raw, dedented directive content joined with `\n` (may be empty) |
| `directive_line` | `Int` | 1-based source line of the `.. name::` marker |

`options` is JSON because the frozen AST's `AttributeValue` has no map variant;
`PendingDirective::options()` decodes it and returns an empty map on malformed
input. Phase 06 consumes these nodes and replaces them with concrete Sphinx /
Docutils nodes; it must not re-read the original source. Explicit targets,
substitution definitions and comments are *not* `Pending`: they are emitted as
`Target`, `SubstitutionDefinition` and `Comment` directly.

### 8.6 Differential parity approach

`crates/scribe-rst/tests/differential.rs` runs the real oracle
(`sphinx-build -b pseudoxml -E -q`) on four tiny generated projects and compares
byte-for-byte after normalization. Compared subset: node kinds, nesting,
pseudo-XML indentation, all parser-produced attributes, and text content.
Normalized away (added by Sphinx *after* parsing): `document.source`,
`translation_progress`, `xmlns:*`, `literal_block.force/language/linenos`, and
`doctest_block.classes="doctest"`. The four fixtures cover (1) sections and
paragraphs, (2) bullet/enumerated/field/definition lists, (3) line blocks,
quotes, literals and doctests, and (4) grid and simple tables. Fixtures avoid
inline markup (Phase 05), directives (Phase 06) and the two node kinds absent
from the frozen AST (below).

### 8.7 Known limitations (frozen-AST and scope driven)

* `NodeKind` has no `Classifier` and no `OptionGroup`. Classifiers are emitted
  as `Inline(classes="classifier")` children of `DefinitionListItem`, and
  `Option` nodes are attached directly to `OptionListItem`. Writers/Phase 06
  can map these to Docutils `<classifier>` / `<option_group>`; the block tests
  assert this representation and the differential fixtures avoid both.
* Slugification reproduces Docutils' translate tables and Latin decomposition
  with a static map rather than full Unicode NFKD; CJK and emoji are dropped
  exactly as Docutils' `NFKD + encode('ascii','ignore')` drops them.
* Simple-table colspans on lines other than the top border are supported, but
  East Asian double-width padding (Docutils' `\x00` padding) is not.
* A footnote/citation is preserved as a `Pending` payload rather than parsed
  into `Footnote`/`Citation` nodes in Phase 04 (Phase 06 resolves it).
* Sphinx post-transform attributes are intentionally absent from Phase 04
  output; inline markup is expanded by Phase 05 (see §8.4, §9). Footnote and
  citation *definitions* remain `Pending` payloads for Phase 06.

---

## 9. Phase 05 inline markup & role parser (`scribe-rst`)

### 9.1 Upstream sources inspected

Rules transcribed from the pinned checkout/venv (not guessed):

* `.oracle-venv/.../docutils/parsers/rst/states.py::Inliner` — start/end string
  patterns (`parts`, `patterns`), `quoted_start`, the `non_whitespace_*`
  lookbehinds, `inline_obj`, `interpreted_or_phrase_ref`, `phrase_ref`,
  `substitution_reference`, `footnote_reference`, `standalone_uri`,
  `implicit_inline`; and `escape2null` / `unescape` (`utils/__init__.py`,
  `nodes.py`) for backslash handling.
* `.oracle-venv/.../docutils/utils/punctuation_chars.py` — the full
  `openers` / `closers` / `delimiters` sets and `match_chars` used by the start
  prefix and end suffix recognition rules; `docutils/utils/urischemes.py` — the
  108 recognised URI schemes.
* `sphinx/sphinx/roles.py::XRefRole` and
  `sphinx/sphinx/util/docutils.py::ReferenceRole` — `pending_xref` creation,
  `refdomain` / `reftype` / `refexplicit` / `refwarn`, the `Title <target>`
  explicit-title regex and the `!` "disabled" marker.
* `sphinx/sphinx/domains/python/__init__.py::PyXRefRole.process_link` — the
  `~` / `.` modifier semantics and `add_function_parentheses`;
  `sphinx/sphinx/domains/__init__.py` — domain roles are registered under
  `domain:name`, which is why `:ref:` is `std:ref` and `:func:` is `py:func`.
* `sphinx/sphinx/addnodes.py` (read-only) — the frozen `pending_xref` shape it
  populates (already documented in §7.4).

### 9.2 Public API

| Item | Purpose |
|---|---|
| `InlineSource` | text + parallel `offsets: Vec<u32>` table mapping each byte of `text` to an absolute source byte offset; `single`, `from_lines`, `slice`, `abs_span`. |
| `InlineNode` | detached tree node: `kind`, `text` (Text leaves), `attrs`, `children`, absolute `span`, `line`. |
| `InlineParseResult` / `InlineWarning` | pure recogniser output plus recoverable inline diagnostics. |
| `XrefMeta` | role payload contract (§9.4). |
| `parse_inline(&InlineSource, &InlineOptions) -> InlineParseResult` | pure, non-fallible, panic-free recogniser. |
| `materialize_inline(&mut Document, &[InlineNode], NodeId, &InlineContext) -> RstResult<(Vec<NodeId>, Vec<(NodeId, XrefMeta)>)>` | inserts arena nodes and returns the xref side table. |
| `set_line`, `InlineSource::slice` | helpers for multi-line / trimmed inline text. |
| `ParseOutput::inline_xrefs()` | `(NodeId, XrefMeta)` side table, documented for Phases 06/11. |

The `BlockParser` calls `add_inline` / `add_inline_slice` for paragraphs, titles,
terms and classifiers, field names, line-block lines, attributions, table-cell
paragraphs and substitution replacements. Literal blocks, doctests and comments
keep raw `Text` (Docutils does not parse inline markup there). Prolog/epilog
content is parsed by the same block parser, so its substitution definitions and
roles take effect in the injected text.

### 9.3 Span semantics

* Every `InlineNode.span` is an absolute byte range into the (possibly
  prolog/epilog-augmented) source. `InlineSource::from_lines` joins multi-line
  paragraphs with `\n` while mapping each byte to its original offset, so
  token-local offsets become exact source offsets.
* Element spans cover their delimiters: `*em*` → `[start, start+4)`; the inner
  `Text` child covers only `em` (`[start+1, start+3)`).
* Backslash escapes emit only the escaped character as text but keep a span
  covering the backslash sequence (`\*` is one byte of text, two source bytes).
* Inside inline literals and role payloads, backslashes are *restored*
  (`unescape(text, True)`), matching Docutils; everywhere else they are
  dropped.
* Tab-expanded lines can drift normalized-text offsets past the original
  source end; `add_inline` clamps every span to `source.len()` before
  materialisation so all spans stay in bounds (documented fuzz invariant).
* A synthesised/empty node (e.g. the text child of an inline literal whose
  closing `` starts at its body, an empty payload or label) carries a
  zero-width span **anchored at its construct's own token offset**, never the
  global `Span { 0, 0 }`. This keeps every child span inside its parent even
  when the parent's base offset is nonzero — the property the fuzz suite
  checks by re-parsing each generated string at base 0 and at base 97 (and 1000).
* **Byte offsets are never used as token indices.** `span_of` / `abs_span` take
  indices into the token vector (or byte indices into `InlineSource::text`);
  the Drop-mode rendered payload string is only ever used for *text content*,
  never to compute a span. In `emit_phrase_reference` the explicit-title
  `<...>` delimiters are located at the token level (honouring escapes, as
  docutils' `escape2null` does), and the title child's span is derived from its
  token range. A multi-byte character (em-dash, CJK, emoji, combining mark) is
  one token but several bytes, so using `raw[..lt].len()` as a token index
  would overshoot the parent (the second span bug fixed here).
* `InlineSource::abs_span` is robust to **non-monotonic offset tables**. A
  tab-expanded line can be longer than its original source slice, so the next
  line's absolute offsets can be *lower* than the previous line's end. The span
  is therefore the min/max of `offsets[start..=end]`, not the two endpoints.
  This is identical for normal monotonic tables and makes containment
  structural: a child token range inside its parent's range always maps to a
  child byte span inside the parent's byte span.
* `materialize_one` invokes a `checked_child_span` debug-only invariant for
  every child, so a future span regression trips at the materialisation source
  in debug builds (no `.unwrap()`/`.expect()` in production; release builds
  unaffected).

### 9.4 Role payload contract (Phases 06/11)

Direct roles become concrete AST nodes: `:math:` → `Math`, `:literal:` →
`Literal`, `:emphasis:` → `Emphasis`, `:strong:` → `Strong`, the default role →
`Inline(classes="title-reference")` (frozen-AST gap; see §9.6).

Cross-reference roles (`:doc:`, `:ref:`, `:any:`, `:py:func:`,
`:py:class:`, `:std:term:`, `:c:func:`, …) become a frozen `NodeKind::PendingXref`
populated through `Node::pending_xref_mut`:

| Frozen field | Value |
|---|---|
| `refdomain` | domain (`std`, `py`, `rst`, `c`, `cpp`, `js`, `""` for `any`) |
| `reftype` | role name within the domain (`ref`, `doc`, `func`, …) |
| `reftarget` | whitespace-normalised target after `~` / `.` processing |
| `refexplicit` | `Title <target>` was used |
| `refspecific` | a leading `.` modifier was present |
| `refwarn` | upstream `warn_dangling` for the role |
| `refdoc` | document name from `ParseOptions::source_name` |
| `classes` | `["xref", domain, "domain-type"]` |

The single child is an inner `Literal` / `Inline` node holding the displayed
title (which may have had `~`/`.` modifiers applied). To keep pseudo-XML clean,
the additional contract lives in `ParseOutput::inline_xrefs()` keyed by the
xref's `NodeId`:

```rust
pub struct XrefMeta {
    pub role: String,          // as written, e.g. "py:func"
    pub raw_payload: String,   // text between the backticks
    pub target: String,        // parsed + modifier-adjusted target
    pub target_span: Span,     // absolute span of the target
    pub title_span: Span,      // absolute span of the displayed title
    pub explicit_title: bool,
    pub specific: bool,        // ~ / . modifier
}
```

The role-to-domain/reftype/inner-node mapping is a static table in
`inline.rs::xref_role` / `direct_role`, transcribed from Sphinx's domain role
registries. Unknown roles emit `Problematic` with the payload text. Phase 06
registers new roles; Phase 11 resolves the pending xrefs using these fields.

### 9.5 Differential parity and fuzz evidence

`crates/scribe-rst/tests/inline_differential.rs` runs the real oracle and
compares **byte-for-byte** (after the §8.6 normalization plus the
title-reference tag mapping) four fixtures: (1) emphasis/strong/literals/
escapes, (2) Docutils roles, (3) named/anonymous/phrase/standalone references,
(4) substitution expansion. A fifth fixture compares the *inline reference
profile* (node kinds + text) for footnotes/citations, because Sphinx assigns
generated ids/labels and converts definitions to `footnote`/`citation` nodes in
post-transforms owned by later phases. A sixth asserts that the parsed targets
and titles for `:py:class:` / `:py:func:` / `:ref:` match the oracle's resolved
text. Specifically **not** compared byte-for-byte: generated `ids`/`refid`/
`backrefs`/`docname` on footnotes/citations, and the post-transform replacement
of `pending_xref` with resolved `reference`/`inline`/`literal` nodes.

`crates/scribe-rst/tests/fuzz.rs` uses `proptest` with
`ProptestConfig::with_cases(1024)` on seven properties (7,168 generated cases):
delimiter/backslash-heavy ASCII (alphabet extended with em-dash, CJK, combining
marks and emoji), arbitrary Unicode, prolog/epilog injection, 500–4000-byte
single-token runs, and multi-byte explicit-title phrase references and xref
roles (each checked at bases 0/97/1000). It asserts no panic and that all spans
are in bounds, non-inverted and (for the pure recogniser) nested/ordered. Six
deterministic regressions pin: the shrunk empty-child input `"||[````]"`, the
Unicode byte-vs-token input `` "`———<>`_}" ``, a multi-byte explicit-title
corpus, a non-monotonic offset table, a tab-expanded prolog/epilog document, and
adjacent adversarial cases. The fuzz runs uncovered and fixed a block-parser
char-boundary panic (`parse_option_string` slicing at byte 2), inline
slice-boundary and tab span-clamping issues, a synthesised-empty-child span
anchored at global 0, a byte-length-as-token-index title span, and a
non-monotonic-offset collapse. Stress evidence (all green): five consecutive
`cargo test -p scribe-rst --test fuzz` runs, three consecutive
`cargo test --workspace` runs (338 passed each), `PROPTEST_CASES=8192` for both
the fuzz suite and the full workspace, and `PROPTEST_CASES=16384` for the fuzz
suite.

### 9.6 Known limitations (Phase 05)

* Frozen `NodeKind` has no `title_reference`; represented as
  `Inline(classes="title-reference")`. Differential tests map the tag back.
* PEP/RFC implicit references are not recognised (not in the Phase 05 scope).
* Suffix roles (`` `text`:role: ``) are recognised, but simultaneous prefix and
  suffix roles produce `Problematic` exactly as upstream does.
* Footnote/citation *definitions* stay as Phase 04 `Pending` nodes; references
  get `refname`/`auto`/`refspecific` but no generated `ids`/`backrefs` until
  Phase 06/10.
* Substitution expansion handles plain `|name|`; `|name|_` becomes a
  `Reference` wrapping a `SubstitutionReference` (per Docutils) and is not
  further resolved here.
---

## 10. Phase 06 directive & role registry (`scribe-rst`)

### 10.1 Upstream sources inspected

All node shapes, option names, defaults and run-method behaviour were read from
the pinned oracle rather than guessed:

* `sphinx/sphinx/directives/{__init__,other,patches,admonitions,code}.py` —
  Sphinx's `TocTree`, `Only`, `Include`, `HList`, `Acks`, `Centered`,
  `TabularColumns`, `Figure`, `CSVTable`, `Code`, `MathDirective`, `Rubric`,
  the seven admonitions plus `seealso`, and their option specs.
* `sphinx/sphinx/domains/changeset.py` — `versionadded` / `versionchanged` /
  `deprecated` / `versionremoved` (`versionmodified` node, label classes,
  paragraph folding).
* `sphinx/sphinx/domains/std/__init__.py` — `glossary`, `productionlist`,
  standard-domain xref roles and their `warn_dangling` flags.
* `sphinx/sphinx/roles.py` — `generic_docroles` (`command`, `dfn`,
  `mailheader`, `makevar`, `mimetype`, `newsgroup`, `program`, `regexp`),
  `Abbreviation`, `Keyboard`, `GUILabel`, `MenuSelection`, `EmphasizedLiteral`,
  `Manpage`, `PEP`, `RFC`, `XRefRole`.
* Docutils 0.23 `parsers/rst/directives/{__init__,misc,body,parts,tables,
  images,admonitions}.py` — conversion functions (`flag`, `unchanged*`, `path`,
  `uri`, `int`, `positive_int`, `nonnegative_int`, `class_option`, `choice`,
  `length_or_*`, `percentage`, `encoding`), `Include`, `Raw`, `Replace`,
  `Unicode`, `Date`, `Class`, `Role`, `DefaultRole`, `Title`, `Meta`,
  `Contents`, `Sectnum`, `Topic`, `Sidebar`, `Compound`, `Container`,
  `Epigraph`/`Highlights`/`PullQuote`, `MathBlock`, `CSVTable`, `ListTable`,
  `Image`, `Figure`.

### 10.2 Public API

| Item | Purpose |
|---|---|
| `Directive` | Extensible handler trait: `name`, `required_arguments`, `optional_arguments`, `final_argument_whitespace`, `option_spec`, `has_content`, `run(&mut DirectiveContext) -> Result<Vec<NodeId>, DirectiveError>`. |
| `DirectiveRegistry` | `new`, `builtin`, `register`, `register_arc`, `get`, `contains`, `names`, `len`; case-insensitive names (`pull_quote` == `pull-quote`), `RwLock<HashMap>` + `Arc<dyn Directive>`, deterministic lookups. |
| `DirectiveContext` | Arena + parent + split arguments + typed options + raw option strings + dedented content + source line/span + shared `Context`; helpers `argument`, `flag`, `option_str/int/list`, `classes`, `names`, `add`, `add_text`, `set_common`, `parse_inline_into`, `parse_nested`, `parse_nested_into`, `parse_block_text`. |
| `Context` | Shared build state: `Tags`, optional `Config`, `source_dir`, `source_name`, `source_text`, `tab_width`, `IncludeResolver`, the substitution side table, and the `role`/`default-role` seam. |
| `IncludeResolver` / `FsIncludeResolver` | Injectable file reader for `include` / `raw.file` / `csv-table.file`; the default resolves relative to `source_dir` then `source_name`'s parent. |
| `DirectiveError` | `thiserror` + `miette::Diagnostic`: `unknown_directive`, `missing_argument`, `too_many_arguments`, `missing_content`, option errors, `invalid_only_expression`, `include_io`, `directive_failed`, `ast`. |
| `OptionKind` / `OptionSpec` / `OptionValue` / `OptionError` | Typed converters and validator; `flag`, `unchanged`, `unchanged_required`, `path`, `uri`, `int`, `positive_int`, `nonnegative_int`, `optional_int`, `percentage`, `class_option`, `length`, `length-or-percent`, `choice`, `encoding`, `boolean`, `positive_int_list`, `auto_or_positive_int_list`, `single_char`, `value_or_none_int`, `Custom`. Diagnostics: `unknown_option`, `invalid_option_value`, `duplicate_option`, `malformed_option_line`. |
| `resolve_pending`, `resolve_output`, `ResolveReport`, `ResolveIssue` | The resolve pass and its recoverable diagnostics. |
| `Role` / `BuiltinRole` / `RoleRegistry` | Extensible role handlers and a case-sensitive `BTreeMap` registry; `RoleClass::{Direct, Xref, External}` is the single source of truth consumed by the Phase 05 inline recogniser. |
| `RoleInvocation` / `RoleError` / `DirectBehavior` / `InnerKind` | Direct-call role payload, errors and node-class metadata. |

`DirectiveRegistry::builtin()` currently registers 55 names; `RoleRegistry::builtin()`
registers 70 names, including every role required by the Phase 06 scope plus
domain-qualified aliases.

### 10.3 Resolve pass

`resolve_pending(&mut Document, &DirectiveRegistry, &Context)` walks the frozen
arena with `Document::preorder_ids()` and resolves every directive-carrying
`NodeKind::Pending` in a fixpoint loop:

1. Read `directive_name`, `arguments`, `options` (JSON), `content` and
   `directive_line` through the frozen `PendingDirective` view — **no source
   re-reading**.
2. Look up the handler; an unknown name produces an `unknown_directive`
   `ResolveIssue` (byte span from the Pending node, line from `directive_line`)
   and the Pending node is left in place so writers still see a usable tree.
3. Validate argument counts and typed options, then run the handler. Its nodes
   are spliced into the parent at the Pending node's position and the Pending
   node is tombstoned (arena ids never move).
4. Nested block parses reuse the Phase 04 `parse()` on the dedented content.
   Source lines are offset by the marker line plus option lines; byte spans
   into the temporary content string are deliberately cleared. Any nested
   `Pending` node copied from that parse is resolved on the next iteration —
   this is what makes arbitrarily nested directives work.
5. Substitution definitions whose replacement is a directive (`image`,
   `unicode`, `date`) are resolved from the additive side table described in
   §10.5; `replace` definitions were already inline-parsed by Phase 05.
   Already-expanded references are textually refreshed from the concrete
   definition after resolution.
6. Footnote/citation `Pending` payloads become `Footnote` / `Citation` nodes.

`ResolveReport { resolved, nested_parses, unresolved }` is non-fatal by design;
`diagnostics()` returns renderable miette reports. Failed nodes are remembered
so a fixpoint iteration cannot report the same issue twice.

### 10.4 Core directive coverage

* **Admonitions:** `admonition`, `note`, `warning`, `tip`, `important`,
  `caution`, `danger`, `error`, `hint`, `attention`, `seealso`
  (`classes`/`names`/`collapsible`, `admonition-<title-id>` fallback).
* **Changeset:** `versionadded`, `versionchanged`, `deprecated`,
  `versionremoved` → `versionmodified` with `type`, `version` and the
  `versionmodified <added|changed|deprecated|removed>` label inline.
* **Body:** `rubric`, `epigraph`, `highlights`, `pull-quote`, `compound`,
  `container`, `topic`, `sidebar`, `centered`, `acks`, `hlist`, `glossary`,
  `productionlist`, `tabularcolumns`, `only` (`Tags::eval_condition`).
* **Parts:** `contents` and `sectnum` emit `Pending` placeholders carrying a
  JSON `__details` string for the Phase 10 post-transform pipeline.
* **Files:** `include` (`literal`, `code`, `parser`, `encoding`, `tab-width`,
  `start-line`, `end-line`, `start-after`, `end-before`, `number-lines`,
  `class`, `name`) and `raw` (`file`, `encoding`, `class`).
* **Substitutions / metadata:** `replace`, `unicode`, `date`, `class`,
  `rst-class`, `cssclass`, `role`, `default-role`, `title`, `meta`, `math`.
* **Media/tables:** `image`, `figure`, `csv-table`, `list-table`.
* **Footnotes:** `footnote`, `citation`.
* **Highlighting placeholders:** `code`, `code-block`, `sourcecode`,
  `literalinclude` emit `LiteralBlock` (`language`, `force`, `linenos`,
  `source`); Phase 15 replaces them with highlighted token streams.

### 10.5 Additive side tables and seams

* `ParseOutput::substitution_directives()` maps a `SubstitutionDefinition`
  `NodeId` to a `SubstitutionDirective { name, arguments, options }`. Phase 04
  records it while parsing `.. |name| directive:: ...`; it is *not* a
  pseudo-XML attribute. `resolve_output()` copies it into `Context` before
  calling `resolve_pending()`.
* `ParseOutput::inline_xrefs()` remains the frozen Phase 05 xref contract;
  Phase 06 never re-parses role text and keeps `PendingXref` nodes for
  Phase 11. `resolve_output()` prunes side-table entries whose node was
  removed.
* `Context::custom_roles` and `Context::default_role` record `.. role::` and
  `.. default-role::` declarations. Phase 05's `parse_inline` is a *pure*
  function by contract (§9.2), so per-document role redefinitions cannot be
  applied during that pass; these maps are the documented seam for a future
  stateful inline pass / Phase 26 i18n work.
* `rst_prolog` / `rst_epilog` are injected in Phase 04, so their directives and
  roles travel through the same resolve pass.

### 10.6 Differential parity (`tests/directive_differential.rs`)

Two fixtures are run through the real `sphinx-build -b pseudoxml` oracle and
compared **byte-for-byte** after normalization:

1. **18 directives + 4 roles:** `note`, `admonition`, `versionadded`, `rubric`,
   `epigraph`, `compound`, `container`, `topic`, `sidebar`, `centered`,
   `tabularcolumns`, `image`, `figure`, `csv-table`, `list-table`, `glossary`,
   `hlist`, `productionlist`; roles `:abbr:`, `:kbd:`, `:math:`, `:regexp:`.
2. **4 directives with build state:** `include` (literal + line clipping),
   `raw`, `only` (enabled with `-t draft` on the oracle side), `image` with
   `:target:`.

Normalized away in addition to §8.6: `image.candidates` (Sphinx image
collector), generated implicit `ids="idN"` / `ids="term-*"`, the glossary
post-transform (`definition_list.classes="glossary"`, `<index>` nodes),
`literal_block.source` (absolute temp paths), and the frozen-AST gap
`abbreviation` → `Inline(classes="abbreviation")`. Everything else — node
kinds, nesting, attributes and text — is compared verbatim.

### 10.7 Known limitations (Phase 06)

* Writer/post-transform attributes and nodes owned by later phases are absent:
  resolved xrefs (11), section numbers/toctrees (09/10), image `candidates`,
  equation numbers, footnote `ids`/`backrefs`, syntax tokens (15).
* `contents` / `sectnum` leave transform `Pending` placeholders; they are not
  byte-compared against the oracle's fully transformed topic/bullet tree.
* `productionlist` token cross-references are text for now; Phase 11 adds the
  grammar-token domain resolution.
* `include` supports the documented clip options and RST parsing; the
  `parser` option records but does not dispatch to non-RST parsers (MyST
  inclusion arrives in Phase 07).
* `date` honours `SOURCE_DATE_EPOCH` for reproducible output and falls back to
  the current clock; strftime covers `%Y %y %m %d %H %M %S %%`.
* `only` is evaluated eagerly at resolve time (tag true → children spliced,
  false → node removed), which matches the final pseudoxml but does not keep
  the `only` wrapper for later re-evaluation.
---

## 11. Phase 07 MyST Markdown parser integration (`scribe-myst`)

### 11.1 Upstream sources and oracle strategy

The pinned oracle checkout (`./sphinx`) does **not** ship MyST-Parser and the
`.oracle-venv` does not have `myst_parser` installed. A Markdown source can
therefore not be run through a MyST-enabled Sphinx for a direct pseudo-XML
diff. As permitted by the task, the differential strategy is:

* **MyST ↔ RST equivalence** for every construct that has an RST spelling:
  both sources are parsed by their respective engines and compared with a
  span-free canonical tree signature (node kinds, nesting, attributes and
  text). See `crates/scribe-myst/tests/equivalence.rs`.
* **Directive payload parity**: MyST fenced directives emit the exact Phase 06
  `Pending` shape and are resolved through the *same* `DirectiveRegistry` as
  RST directives; the resolved trees are compared byte-for-byte in the
  canonical signature.
* **Span invariants** are asserted directly against the original `.md` source.

The CommonMark engine is [`pulldown-cmark`](https://docs.rs/pulldown-cmark)
0.13, used in streaming `into_offset_iter()` mode so no separate Markdown AST
is built and every `Span` is an exact byte range into the original source.

### 11.2 Public API

| Item | Purpose |
|---|---|
| `parse(&str) -> MystResult<ParseOutput>` | default-options entry point |
| `parse_with_options(&str, ParseOptions) -> MystResult<ParseOutput>` | named document / explicit options |
| `parse_and_resolve(&str, ParseOptions) -> MystResult<(ParseOutput, ResolveReport)>` | convenience: parse, then resolve directive `Pending`s with the builtin RST registry |
| `resolve(&mut ParseOutput, &DirectiveRegistry) -> ResolveReport` | resolve directives with caller-supplied registry (default context) |
| `resolve_with_context(&mut ParseOutput, &DirectiveRegistry, &Context)` | resolve with a shared Phase 08 build context |
| `ParseOutput` | `{ document, diagnostics, frontmatter, xrefs, anchors }` + `has_diagnostics`, `into_document`, `pending_xrefs`, `frontmatter_str`, `source`, `source_name` |
| `ParseOptions { source_name }` | document name (root `source` attr + `refdoc`) |
| `Frontmatter { raw, value: serde_json::Value, span }` | parsed YAML metadata + `get_str` / `is_empty` |
| `Anchor { label, node_id, span, kind }`, `AnchorKind::{Heading, Target}` | MyST `(label)=` cross-reference targets |
| `MystError` / `MystDiagnostic` / `Severity` / `named_report` | `thiserror` + `miette` diagnostics with byte spans |
| `directives::{parse_directive_info, parse_directive_body, myst_content_to_rst}` | directive fence helpers and nested-content conversion seam |

`ParseOutput` mirrors `scribe_rst::ParseOutput`: it always yields a best-effort
document plus recoverable diagnostics; MyST-only data lives in additive side
tables (frontmatter, xrefs, anchors) rather than in frozen AST attributes.

### 11.3 Feature coverage and RST-equivalence mapping

| MyST / CommonMark | AST nodes | RST equivalent / mapping notes |
|---|---|---|
| `# Heading` | `Section(ids,names)` + `Title` | `Title\n=====`; ids via the copied Docutils `make_id`; nested by heading level |
| paragraph, `*em*`, `**strong**`, `` `code` `` | `Paragraph`, `Emphasis`, `Strong`, `Literal` | RST uses double backticks for literals; equivalent tree |
| `[text](uri)`, `[t](#frag)`, `![alt](src)` | `Reference(refuri/refid)`, `Image(uri,alt,title)` | same node kinds as RST references/images |
| `- item`, `1. item` | `BulletList(bullet)`, `EnumeratedList(start,enumtype,prefix,suffix)`, `ListItem` | bullet char normalized in equivalence tests |
| `> quote` | `BlockQuote` | indented RST block quote |
| `***` | `Transition` | RST transition |
| fenced / indented code | `LiteralBlock(xml:space=preserve)` | RST literal block |
| GFM table | `Table` → `Tgroup(cols)` → `Colspec`/`Thead`/`Tbody` → `Row` → `Entry` | same node set as RST grid/simple tables |
| raw HTML (`<div>`, inline tags) | `Raw(format="html")` | RST `raw:: html` directive |
| `$x$` / `$$x$$` | `Math` / `MathBlock` | `:math:` role / `math` directive |
| `[^1]` / `[^1]: …` | `FootnoteReference(refname)` / `Footnote(ids,names)` | RST footnote reference + definition |
| `term` / `: definition` | `DefinitionList` → `DefinitionListItem` → `Term`/`Definition` | RST definition list (paragraph wrapper normalized) |
| YAML `---` frontmatter | `ParseOutput.frontmatter` (no AST node) | RST `meta`/field-list metadata seam |
| `` ```{note} … ``` `` / `:::{note} … :::` | `Pending` with the §8.5 keys | `.. note:: …`; resolved by the shared registry |
| `` {role}`payload` `` / `` {py:func}`…` `` | direct nodes or `PendingXref` + `XrefMeta` | RST `:role:` payloads (§9.4) |
| `(label)=` before heading/block | `Section(ids,names)` override or `Target` + `Anchor` | RST explicit target `.. _label:` |

Colon fences are rewritten to backtick fences in a byte-length-preserving
preprocessor (`:` → `` ` ``), so pulldown-cmark's offsets remain exact spans
into the original source. Nested fenced directives are converted into RST
directive syntax in the outer directive's `content` payload, so
`scribe_rst::resolve_pending` resolves them recursively.

### 11.4 Tests

`crates/scribe-myst/tests/` contains 71 tests (13 unit + 58 integration +
1 doctest):

* `commonmark.rs` — 15 tests for each core construct with exact span checks.
* `myst.rs` — 17 tests for every MyST extension, including resolution and
  malformed input.
* `equivalence.rs` — 16 RST↔MyST canonical-tree comparisons.
* `mixed_project.rs` — 2 tests with one `.rst` and one `.md` file that
  cross-reference each other; asserts pending xrefs' `reftarget`s match the
  other file's targets/labels at the AST level.
* `adversarial.rs` — 5 `proptest` properties (2,560 generated cases) plus
  degenerate/long inputs: no panic, all spans in bounds and on UTF-8
  boundaries, JSON round-trip stability.

### 11.5 Known limitations (Phase 07)

* MyST-Parser is not installed in the oracle, so no direct Markdown → Sphinx
  pseudo-XML diff is performed (documented in §11.1).
* Cross-file/pending xrefs remain unresolved by design — final URI/domain
  resolution is Phase 08/11–14.
* GFM table colspecs carry no `colwidth`; header alignment is parsed by
  pulldown-cmark but not yet emitted as `align` (Docutils column alignment is
  a writer concern).
* `myst_dmath`/`myst_amsmath` escaping is approximated: `$…$` → `Math`,
  `$$…$$` → `MathBlock`, `\$` stays literal. Full TeX delimiter ambiguity
  rules (dollar escaping inside math, AMS display variants) are deferred.
* Inline HTML is a `Raw(format="html")` node; HTML *entities* are decoded by
  pulldown-cmark and are not re-escaped.
* Strikethrough (no frozen node) maps to `Inline(classes="strike")`; task-list
  markers are skipped until a writer phase consumes them.
* `include:: … :parser: myst` records but does not dispatch to the MyST parser
  yet (Phase 08 owns source discovery/parsing dispatch).

---

## 12. Phase 08 build environment, discovery & incremental cache — FROZEN PUBLIC CONTRACT

This section is the contract Phases 09–25 (and the Phase 27 extension bus) code against.
It must not change without orchestrator approval. The frozen §7 AST is untouched:
`Document` is only *stored* as a serde snapshot, never modified.

### 12.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/project.py` — `Project.discover()`, `path2doc()`, `doc2path()`, and
  `EXCLUDE_PATHS = ['**/_sources', '.#*', '**/.#*', '*.lproj/**']`.
* `sphinx/sphinx/util/matching.py` — `_translate_pattern` (`*`→`[^/]*`, `**`→`.*`, `?`→`[^/]`,
  negated classes exclude `/`), `compile_matchers`, `Matcher` (extra `**/` prefix expansion),
  `get_matching_files` (include/exclude priority, directory pruning, NFC canonical relative paths).
* `sphinx/sphinx/util/osutil.py` — `canon_path`, `path_stabilize`, `_last_modified_time`
  (integer microseconds).
* `sphinx/sphinx/environment/__init__.py` — `ENV_VERSION = 66`, config status codes,
  `find_files()` (`exclude_patterns + templates_path + builder.get_asset_paths()`),
  `get_outdated_files(config_changed) -> (added, changed, removed)`, `clear_doc()`,
  `note_dependency()`, `note_included()`, `note_reread()`, `_has_doc_changed()`,
  `check_dependents()` (`env-get-updated` event), `_differing_config_keys()`.
* `sphinx/sphinx/builders/__init__.py::Builder.read()` —
  `config_changed = env.config_status != CONFIG_OK`; `added | changed` is the read set;
  added/removed force re-read of `glob_toctrees`; `check_dependents()` extends the write set.

**Refinements incorporated:** exclusions include `templates_path` and builder asset paths;
`UpdateReport` carries explicit `added` / `changed` / `removed` sets plus a
`files_to_rebuild()` union; a Phase 27 extension seam (`extra_outdated`) is reserved.

### 12.2 Crate layout and dependency direction

```
scribe-core ──► scribe-cache ──► blake3, bincode, serde, thiserror, parking_lot
     │
     ├──► scribe-ast   (frozen §7 AST; only stored, never edited)
     ├──► scribe-config (Config → RelevantConfig)
     └──► regex, walkdir, rayon
scribe-cli ──► scribe-core (build command prints the rebuild set)
```

`scribe-cache` is a **leaf**: it never sees documents or the environment. The environment
uses the generic `CacheStore<T>` to persist `EnvironmentSnapshot`. This keeps the graph
acyclic while satisfying "typed cache keyed by content digest".

### 12.3 `scribe-cache` public API

| Item | Purpose |
|---|---|
| `ContentDigest` | `Copy + Ord + Hash + Serialize` 256-bit BLAKE3 hash; `ZERO` sentinel; `to_hex` / `from_hex`; `combine(&[(&str, &ContentDigest)])` for unambiguous labelled hashing. |
| `hash_bytes`, `hash_str`, `hash_file` | Free helpers. Missing files map to `ZERO` at the environment layer. |
| `CacheError` | `thiserror` enum: `Io`, `Encode`, `InvalidDigest`, `InvalidKey`, `CreateDir`. |
| `CacheStore<T>` | Typed store: `new(root)`, `root()`, `path_for(key)`, `exists(key)`, `save(key, &T) -> ContentDigest`, `load(key) -> Option<T>`, `load_checked(key) -> T`, `remove`, `clear`. |
| `CACHE_MAGIC`, `CACHE_FORMAT_VERSION` | `b"SCRB"` and `1`; bumped when the envelope changes. |

Cache file layout (deterministic on every platform): `[magic 4][version u32 LE][payload length u64 LE][BLAKE3 digest 32][bincode payload]`.
Writes use a temporary file, `sync_all`, then `rename`. **Any** load problem — missing,
truncated, bad magic, wrong version, length mismatch, checksum failure, undecodable
payload — yields `Ok(None)` (cold cache); no panic, ever. `load_checked()` exists for
diagnostics and deliberately corrupt-cache tests.

### 12.4 Frozen `scribe-core` API

#### 12.4.1 Types

```rust
pub struct BuildEnvironment {
    pub found_docs: HashMap<String, PathBuf>,              // docname → source path
    pub all_docs: BTreeMap<String, DocMeta>,               // ordered by docname
    pub dependencies: HashMap<String, HashSet<String>>,    // docname → relative dep paths
    pub included: HashMap<String, HashSet<String>>,        // docname → relative included paths
    pub files_to_rebuild: HashMap<String, HashSet<String>>,// docname → docs to rewrite with it
    pub toctree_includes: BTreeMap<String, Vec<String>>,   // docname → toctree children (Phase 09)
    pub reread_always: BTreeSet<String>,
    pub glob_toctrees: BTreeSet<String>,                    // Phase 09 populates
    pub numbered_toctrees: BTreeSet<String>,                // Phase 09 populates
    pub documents: BTreeMap<String, DocumentSnapshot>,
    pub outdated_docs: BTreeSet<String>,                    // current update's added ∪ changed
    pub extra_outdated: BTreeSet<String>,                   // Phase 27 event seam
    // private: source_dir, relevant_config, parser_version
}

pub struct DocMeta {
    pub relative_path: String,
    pub mtime_micros: i64,
    pub source_digest: ContentDigest,
    pub config_digest: ContentDigest,
    pub parser_version: String,
    pub included_digests: BTreeMap<String, ContentDigest>,
    pub dependency_digests: BTreeMap<String, ContentDigest>,
    pub combined_digest: ContentDigest,
    pub title: Option<String>,
}

pub struct DocumentSnapshot {
    pub docname: String,
    pub doctree: Option<Document>,        // frozen §7 AST, serde round-trippable
    pub sections: Vec<String>,
    pub toctree_targets: Vec<String>,
    pub xrefs: Vec<String>,
    pub anchors: Vec<String>,
}

pub struct RelevantConfig {              // upstream "env" config category
    pub exclude_patterns: Vec<String>, pub include_patterns: Vec<String>,
    pub templates_path: Vec<String>,    pub source_suffix: Vec<(String, String)>,
    pub master_doc: String,             pub rst_prolog: String,
    pub rst_epilog: String,             pub extensions: Vec<String>,
    pub language: String,               pub source_encoding: String,
}

pub struct GlobSpec { pub include: Vec<String>, pub exclude: Vec<String> }

pub struct EnvironmentSnapshot { /* sorted Vec<(key, value)> fields; serde + bincode */ }

pub struct ExtractedMetadata {
    pub title: Option<String>, pub sections: Vec<String>, pub toctree_targets: Vec<String>,
    pub xrefs: Vec<String>, pub anchors: Vec<String>,
}

pub struct UpdateOptions {
    pub freshenv: bool,        // -E
    pub write_all: bool,       // -a
    pub cache_dir: Option<PathBuf>,
    pub persist: bool,         // default true in callers; Default = false
    pub asset_paths: Vec<String>,
    pub extra_excludes: Vec<String>,
    pub doctree_dir: Option<PathBuf>,
    pub extra_outdated: Vec<String>,   // Phase 27 seam
}

pub struct UpdateReport {
    pub added: BTreeSet<String>,
    pub changed: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub config_changed: bool,
    pub config_changed_reason: Vec<String>,
    pub forced_full: bool,
    pub warnings: Vec<String>,
}
```

`UpdateReport::files_to_rebuild()` is `added ∪ changed` (upstream `sorted(added | changed)`);
`docs_to_read()` returns the same set as sorted owned strings. `removed` docs are **purged**,
not rebuilt. `is_empty()` includes `removed`; `counts()` returns `(added, changed, removed)`.

**Naming caution for later phases:** `BuildEnvironment::files_to_rebuild` (plural, *field*) is
upstream's persistent **edge map** "docname → documents that must be rewritten with it",
populated by `note_file_to_rebuild()` (Phase 09 toctree collector). `UpdateReport::files_to_rebuild()`
(*method*) is the current rebuild **set**. They are intentionally both kept.

#### 12.4.2 Construction and configuration

* `BuildEnvironment::new(source_dir)` — empty env, default `RelevantConfig`, parser version
  `DEFAULT_PARSER_VERSION = "scribe-rst=1;scribe-myst=1;directives=55;roles=70"`.
* `with_config(source_dir, &Config)`, `source_dir()`, `set_source_dir()`,
  `relevant_config()`, `set_relevant_config()`, `parser_version()`, `set_parser_version()`.
* `glob_to_spec() -> GlobSpec` and `glob(path) -> bool` (`Matcher` semantics, includes
  `templates_path` and `EXCLUDE_PATHS`).
* `found_docs` field (exact spec shape), plus deterministic accessors `found_docs_sorted()`,
  `found_docnames()`, `doc2path(docname)`, `path2doc(path)`.

#### 12.4.3 Dependency / inclusion recording and reverse queries

| Method | Effect |
|---|---|
| `note_dependency(docname, path)` | Add a relative dependency path and hash it immediately into `DocMeta::dependency_digests`. |
| `note_dependencies(docname, paths)` | Batch form. |
| `note_included(docname, path)` | Add a relative included path to `included[docname]` and hash it into `DocMeta::included_digests`. |
| `note_toctree_edge(parent, child)` / `set_toctree_includes(parent, &[String])` | Phase 09 seam for toctree edges. |
| `note_file_to_rebuild(docname, dependent)` | Upstream `files_to_rebuild` edge ("rewrite `dependent` when `docname` changes"). |
| `note_reread(docname)`, `note_glob_toctree(docname)`, `note_numbered_toctree(docname)` | Force flags. |
| `add_extra_outdated(iter)` / `UpdateOptions::extra_outdated` | Phase 27 `env-get-outdated` / `env-get-updated` seam; drained each update. |
| `mark_outdated(docname)`, `purge_doc(docname)` | Transient marking / full removal (mirrors `clear_doc`). |

Reverse lookups (all cycle-safe and deterministic):

* `includers_of(name)` — docs whose `included` set contains the name, its source path, or
  `name + suffix` variants.
* `docs_depending_on(path)` — reverse of `dependencies`.
* `toctree_parents_of(docname)` — reverse of `toctree_includes`.
* `direct_dependents(docname)` — union of all four reverse maps (minus the seed).
* `dependent_closure(iter)` / `dependents_of(docname)` — transitive closure.
* `watched_paths_of(docname)` — all paths whose change invalidates that document.

#### 12.4.4 Doctree snapshots

* `record_document(docname, &Document, included_digests)` — stores a serde clone of the frozen
  AST, extracts title / section ids / toctree entries / pending-xref targets / anchors, refreshes
  source/config/parser/included digests and recomputes `combined_digest`.
* `set_document_snapshot(snapshot)`, `document(docname)`, `documents()`, `get_doctree(docname)`
  (the `environment.pickle` doctree equivalent), `extract_document_metadata(&Document)`.
* `collect_toctree_entries` reads `entries` / `includefiles` as string lists (the frozen AST has
  no tuple variant; Phase 09 should store docname strings).

#### 12.4.5 Persistence

* `to_snapshot()` / `from_snapshot()` — all maps become key-sorted `Vec<(K, V)>`, giving
  deterministic bincode bytes. A version mismatch or changed source dir is an
  `ScribeError::Environment`, which the loader maps to a cold cache.
* `save(cache_dir)` → `ContentDigest`; `load(cache_dir)` → `Option<Self>` (never panics on a
  corrupt artifact); `load_strict(cache_dir)` reports decode errors.
* Artifact name: `<cache_dir>/cache-environment.bin`.

#### 12.4.6 `BuildEnvironment::update(config, source_dir, &UpdateOptions) -> UpdateReport`

Exact algorithm (upstream-equivalent where possible):

1. `-E` → start from an empty env. Otherwise restore from `cache_dir`; a missing/corrupt
   artifact means a **cold** build (empty env). Extension `extra_outdated` inputs are merged
   *after* restore.
2. Replace `source_dir` / `relevant_config` from the new config. `config_changed` is true when
   `-E`, when any `RelevantConfig` field differs (report names in `config_changed_reason`),
   when the parser/directive registry version changed, or when starting cold.
3. Discover sources: `walkdir` + `rayon` hashing; `source_suffix` → docnames; apply
   `include_patterns`, `exclude_patterns`, `templates_path`, asset paths and
   `EXCLUDE_PATHS`; deterministic sort; directory pruning; duplicate-docname warning.
4. `removed = all_docs − found`; `purge_doc()` each.
5. If `config_changed || -a`: `added = all found`, `changed = ∅` (upstream puts everything
   in `added`). Otherwise a doc is `changed` when any of: source digest differs, mtime
   increased, config/parser generation differs, `reread_always`, a watched include/dependency
   digest differs, or (when `doctree_dir` is supplied) its `.doctree` file is missing.
6. Added/removed docs force re-read of `glob_toctrees ∩ found`.
7. Reverse-edge closure: every dependent of an added/changed doc is added to `changed`
   (included files, toctree children, `files_to_rebuild` edges, extension seam).
8. Refresh all metadata baselines, set `outdated_docs = added ∪ changed`, persist when
   requested, return the report.

Both unchanged-file skipping and content/mtime invalidation are asserted in
`tests/incremental.rs`.

### 12.5 Discovery details

`discover_sources(&DiscoveryOptions) -> DiscoveryResult` exposes `docs: BTreeMap<String,
DiscoveredDoc>` and `warnings: Vec<String>`. `DiscoveredDoc` carries docname, relative path,
absolute path, matched suffix, parser name, BLAKE3 digest and mtime. `DiscoveryOptions::from_config`
derives suffixes from `Config::source_suffixes`. Helper functions: `doc2path`, `path2doc`,
`match_any`, `Matcher`, `compile_matchers`, `translate_pattern`, `canon_path`, `path_stabilize`.
A missing source dir yields an empty result plus a warning (never an error).

### 12.6 Differential method (incremental decisions)

Location: `crates/scribe-core/tests/differential.rs`. It runs the real
`.oracle-venv/bin/sphinx-build` on a process-unique two-document fixture (`index.rst` with a
toctree to `other.rst`, minimal `conf.py`). Steps:

1. `sphinx-build -b html -E -v <src> <out>` must report both docs in `reading sources...`
   (full baseline).
2. Unchanged `sphinx-build -b html -v` must report **no** `reading sources...` docnames;
   our `update()` on an identical fixture with a cache dir must return an empty rebuild set.
3. Append a paragraph to `other.rst`; the oracle reports exactly `other` in
   `reading sources...`; our `UpdateReport::files_to_rebuild()` must equal `{"other"}`.
4. A second unchanged round-trip must again be empty on both sides.

`reading sources...` is upstream's authoritative read set: `Builder.read()` emits it for
`sorted(added | changed)`, exactly the union our method returns. Docnames are already
suffix-free and `/`-separated on both sides, so normalization is a direct set comparison
after parsing the bracketed progress label (`[ 50%] name`). The parser is unit-tested
separately. `-E` and `-a` force-full behavior is also verified via CLI smoke runs
(`scribe build …` prints the rebuild set before the Phase 01 unimplemented-builder diagnostic).

### 12.7 Tests and known limitations

New tests: `scribe-cache` 5 unit + 4 integration; `scribe-core` 9 unit + 8 discovery
integration + 15 incremental integration + 2 oracle differential. Total workspace: 329
library/integration tests + 3 doctests.

Limitations / seams for later phases:

* Phase 09 now populates `toctree_includes`, `glob_toctrees`, `numbered_toctrees` and
  `files_to_rebuild` edges (§13); the invalidation machinery is wired to all four.
* Phase 11–14 must write pending-xref targets into `DocumentSnapshot::xrefs` via
  `record_document()`; anchors come from `Target` node ids/names.
* Phase 27 must feed `UpdateOptions::extra_outdated` from the event bus.
* Doctree files themselves are not written by Phase 08; `UpdateOptions::doctree_dir` is the
  optional hook for the builder phases to restore upstream's "missing doctree forces read".
* The parser generation string is manual (`DEFAULT_PARSER_VERSION`); Phase 30 can derive it
  from crate versions automatically.
* `Config` itself is not serde-serializable, so only the `RelevantConfig` subset is hashed and
  persisted; unrelated settings intentionally do not invalidate doctrees (upstream `CONFIG_OK`).
* MyST discovery works (`.md` → parser `myst`), but no MyST oracle is installed; the
  differential test uses RST only.

---

## 13. Phase 09 toctree collector, hierarchy & numbering

### 13.1 Upstream sources inspected

Behavior was extracted from the pinned oracle (not guessed):

* `sphinx/sphinx/directives/other.py::TocTree` — `option_spec`, `run()`,
  `parse_content()` (explicit titles, `url_re`, `glob_re`, suffix stripping,
  `docname_join`, duplicate detection, `:reversed:` applied **after** glob
  expansion).
* `sphinx/sphinx/environment/collectors/toctree.py` —
  `TocTreeCollector.process_doc()` (local ToC tree from sections, `only`
  wrappers and toctrees), `assign_section_numbers()` (`_walk_toc` /
  `_walk_toctree`, shared `numstack`, `titlenode` hand-off, nested-numbered
  warning), `assign_figure_numbers()` (`numfig`, `numfig_secnum_depth`,
  per-figtype counters keyed by section prefix).
* `sphinx/sphinx/environment/adapters/toctree.py` — `note_toctree()` (the four
  environment seams), `_resolve_toctree()` / `_entries_from_toctree()`
  (hidden / `includehidden` / `titlesonly` / maxdepth rendering semantics).
* `sphinx/sphinx/environment/__init__.py` — `_traverse_toctree()` (pre-order,
  single-yield traversal with a `traversed` set) and `collect_relations()`
  (prev / next / parent assignment).
* `sphinx/tests/root/test-toctree*`, `test-circular`, `test-numbered-circular` —
  fixtures used for differential tests and adversarial cases.

### 13.2 Crate layout and directive placement

The `toctree` handler lives in **`scribe-rst`** (`src/toctree.rs`) and is
registered additively in `DirectiveRegistry::builtin()` (56 directives;
`DEFAULT_PARSER_VERSION` bumped to `directives=56`). The collector lives in
**`scribe-core`** (`src/toctree.rs`); `scribe-rst` does **not** depend on
`scribe-core`, so the dependency graph stays acyclic. The two halves communicate
through the frozen §7 attribute map, documented as a contract in
`scribe_rst::toctree`:

| Attribute | Meaning |
|---|---|
| `entries`, `includefiles` | resolved child docnames (rewritten by the collector after glob expansion) |
| `entry_refs` | every ref as written (docname, `Title <doc>`, glob, URL, `self`) |
| `entry_titles` | parallel explicit titles (`""` = use the target title) |
| `entry_kinds` | parallel classifier `doc` / `glob` / `external` / `self` |
| `parent`, `maxdepth`, `caption`, `rawcaption`, `glob`, `hidden`, `includehidden`, `numbered`, `titlesonly`, `reversed`, `name` | upstream node options/defaults |

External and `self` entries additionally get child `reference` nodes carrying
`refuri`, so a writer can render them without decoding the parallel lists.

The frozen `BuildEnvironment` shape is untouched. Its `note_toctree_edge()` /
`set_toctree_includes()` seams sort child lists, while upstream preserves
source order; the collector records edges through the seam methods and then
installs the ordered vector through the public `toctree_includes` field. The
other seams (`note_file_to_rebuild`, `note_glob_toctree`,
`note_numbered_toctree`, `note_reread`) are used directly.

### 13.3 Public API (`scribe-core::toctree`, re-exported from `scribe_core`)

| Item | Purpose |
|---|---|
| `EntryTarget` / `ToctreeEntry` / `ToctreeOptions` / `ParsedToctree` | structured entries and options with byte spans / line |
| `ToctreeWarning` | `thiserror` + `miette` diagnostic (`code`, span, `help`, source snippet via `collect_toctrees_with_sources`) |
| `ToctreeCollector` / `collect_toctrees` / `collect_toctrees_with_sources` | idempotent collection pass; populates env seams, writes resolved entry lists and numbers back into stored doctrees |
| `ToctreeHierarchy` | ordered cycle-free `edges`, `parents`, flattened pre-order `order`; `children` / `parent` / `subtree` / `global_toctree_for_doc` (structural) / `document_toc` / `unreachable` |
| `ToctreeCollection` | aggregate; `global_toctree_for_doc(docname, includehidden)` resolves renderable subtrees (hidden toctrees skipped unless `includehidden`), `relation`, `diagnostics` |
| `SectionNumbers` / `FigureNumbers` | `docname → anchor → Vec<i64>` and `docname → figtype → id → Vec<i64>`; dotted `display()` |
| `NavRelation` / `TocTreeNode` / `SectionTocEntry` | prev / next / parent, resolved global ToC tree, local section ToC |

### 13.4 Semantics

* **Glob expansion:** `:glob:` patterns are joined to the containing docname
  with Sphinx `docname_join` semantics, matched with Sphinx `translate_pattern`
  (`*`/`?` do not cross `/`, `**` does), filtered by `exclude_patterns` /
  `templates_path` / built-in exclusions, sorted and deduplicated. A pattern
  matching nothing emits `scribe::toc::empty_glob`. `:reversed:` reverses the
  *expanded* lists, as upstream does.
* **Cycle handling:** iterative DFS from `master_doc` finds back edges; each
  cycle emits `scribe::toc::circular` with the full cycle path and a labeled
  source span, and the back edge is removed. Self references and duplicate
  entries are detected separately. Traversal and numbering are therefore
  always finite; no panic on adversarial graphs.
* **Hierarchy / navigation:** `ToctreeHierarchy::order` is the iterative
  equivalent of `_traverse_toctree()`; `NavRelation` mirrors
  `collect_relations()`. Hidden toctrees contribute edges (upstream behavior);
  they only affect rendering through `:hidden:` / `:includehidden:`.
  Documents not reachable from `master_doc` emit `scribe::toc::not_included`
  (`:orphan:` field is honored).
* **Section numbering:** mirrors `assign_section_numbers()` — one shared
  `numstack` per numbered toctree, a fresh counter push at each nested
  `bullet_list`, empty number tuples beyond depth, `nested numbered toctree`
  warnings, and `env.titles[doc]` numbers for prev/next titles. Numbers are
  stored in `SectionNumbers` and written to `secnumber` attributes on Section /
  Title nodes and on `sectnum` transform placeholders.
* **Figure numbering:** when `numfig` is enabled, per-figtype counters keyed by
  the truncated section prefix (`numfig_secnum_depth`) number `figure`, `table`
  and captioned literal-block `container` nodes; numbers are written to
  `fignumber` attributes.
* **Spans:** the RST directive records exact byte spans; collector diagnostics
  reuse them. `collect_toctrees_with_sources()` attaches source text so miette
  renders labeled snippets.

### 13.5 Phased parser refinements

Two additive parser fixes were required by real `test-toctree*` fixtures:

* Phase 04 now records the option-free body lines on `pending` nodes
  (`body_lines`) and Phase 06 restores them for zero-argument directives. This
  makes `.. toctree::\n   child` (no blank after the marker) behave like
  Docutils instead of promoting `child` to an invalid argument.
* 3-character overline sections (`===\nFoo\n===`) are recognized before the
  `>= 4` transition heuristic, matching Docutils and the `test-toctree-maxdepth`
  fixture.

`.. sectnum::` placeholders carry an additive `__directive = "sectnum"` marker
so the numbering pass can attach `secnumber` without re-reading `__details`.

### 13.6 Differential method (`tests/toctree_differential.rs`)

Four real fixtures are compared against the pinned oracle:
`test-toctree`, `test-toctree-glob`, `test-toctree-maxdepth` and
`test-toctree-multiple-parents`. Each is copied to a temp dir and:

1. `sphinx-build -b html -E -q` produces pages; `<link rel="prev|next">`
   hrefs are normalized from relative paths with `docname_join` and compared
   to `NavRelation` (links from unreachable pages are asserted absent).
2. A small Sphinx-API query prints `env.toctree_includes`,
   `env.toc_secnumbers` and `env.collect_relations()` as JSON. Those are the
   authoritative hierarchy/numbering structures and are compared **exactly**
   with our ordered edge map, section-number map and navigation relations.
   Using the API avoids lossy scraping of the resolved ToC markup.
3. Our side parses the same sources with `scribe-rst` with the `html` tag
   enabled (Phase 06 evaluates `only` eagerly) and runs `collect_toctrees()`.

Normalization: docnames are suffix-free `/`-separated on both sides; section
anchors are Docutils ids; `null` relation fields become `None`; external URLs
and `self` remain entries but do not create edges. No scraped section-anchor
entries are compared, because document hierarchy is the owned scope.
Tests: 15 `scribe-core/tests/toctree.rs` integration tests + 4 differential
tests + 1 `scribe-rst/tests/toctree.rs` integration test + 4 directive unit
tests. Total workspace: 364 tests (361 library/integration + 3 doctests).

### 13.7 Known limitations

* `:numbered:` depth uses the full document hierarchy rather than the
  maxdepth-pruned render tree; this matches upstream numbering but means
  numbers can exist for entries not rendered at a given maxdepth.
* `numfig` recognizes `figure`, `table` and captioned literal-block containers;
  extension-defined enumerable node types are deferred to Phase 27.
* `global_toctree_for_doc()` resolves the document-level tree; section-anchor
  entries and `titlesonly` pruning are owned by the writer phases.
* The `only` directive is still evaluated eagerly at resolve time; the
  differential tests enable the `html` tag so its toctrees are present.
---

## 14. Phase 10 transform pipeline & post-transforms

### 14.1 Upstream sources inspected

Every priority and behavior below was extracted from the pinned oracle, not guessed:

* `sphinx/sphinx/transforms/__init__.py` — `DefaultSubstitutions`,
  `MoveModuleTargets`, `HandleCodeBlocks`, `AutoNumbering`, `SortIds`,
  `ApplySourceWorkaround`, `AutoIndexUpgrader`, `ExtraTranslatableNodes`,
  `DoctestTransform`, `FilterSystemMessages`, `SphinxSmartQuotes`,
  `GlossarySorter`, `ReorderConsecutiveTargetAndIndexNodes`, `DoctreeReadEvent`.
* `sphinx/sphinx/transforms/post_transforms/__init__.py` — `OnlyNodeTransform`,
  `SigElementFallbackTransform`, `PropagateDescDomain`, `ReferencesResolver`.
* `sphinx/sphinx/transforms/references.py` — `SphinxDomains` (priority 850).
* `sphinx/sphinx/transforms/compact_bullet_list.py` — `RefOnlyBulletListTransform`.
* `sphinx/sphinx/transforms/i18n.py` — `PreserveTranslatableMessages`, `Locale`,
  `TranslationProgressTotaliser`, `AddTranslationClasses`,
  `RemoveTranslatableInline`.
* `.oracle-venv/.../docutils/transforms/references.py` — `Footnotes` (620),
  `PropagateTargets` (260), `Substitutions` (220).
* `.oracle-venv/.../docutils/transforms/universal.py` — `SmartQuotes` and
  `docutils/utils/smartquotes.py` (`educate_tokens`, `educateQuotes`,
  `educateDashesOldSchool`, `educateEllipses`).
* `sphinx/sphinx/util/nodes.py` — `is_smartquotable`, `apply_source_workaround`,
  `process_only_nodes`, `_is_doctest_block`.

### 14.2 Public API (`scribe-core::transforms`, re-exported from `scribe_core`)

The frozen §7 AST and §12 `BuildEnvironment` shapes are untouched: transforms
only use their checked public APIs and never invent a node kind.

| Item | Purpose |
|---|---|
| `Transform` / `PostTransform` | Traits: `name()`, `default_priority()`, `apply(&mut Document, &TransformContext) -> TransformResult`. |
| `TransformContext<'a>` | `config`, `tags`, `builder_name`, `builder_format`, optional frozen `env`, `language`, `sig_element_fallback`, and a `RefCell` diagnostic sink (`push_diagnostic`, `take_diagnostics`). |
| `TransformResult` | `{ changed, diagnostics }` for one invocation; best-effort, never fatal. |
| `TransformRegistry` | `new`, `builtin`, `register_read(_with_priority)`, `register_post(_with_priority)`, `disable`, `enable`, `is_enabled`, `plan`, `read_plan_names`, `post_plan_names`. |
| `TransformPipeline` | `new`, `with_builtins`, `registry(_mut)`, `run`, `run_read`, `run_resolved`. |
| `Phase` | `Read` / `Resolved`, matching Sphinx's two application points. |
| `TransformReport` | `{ applied, skipped, diagnostics }` plus `applied_names`, `changed_names`, `skipped_names`, `has_diagnostics`. |
| `TransformDiagnostic` | `thiserror` + `miette::Diagnostic` with code `scribe::transform`, severity, and an optional labelled AST span. |
| `apply_transforms(&mut Document, &TransformContext, Phase) -> TransformReport` | **Stable builder seam** (Phase 17+): runs the built-in registry for one phase. |

The engine sorts by `(priority, name)`, never registration order. Two
transforms at the same upstream priority (e.g. the four at 210) therefore run
alphabetically, and the order is reproducible across runs and thread counts.
`disable`/`enable` are keyed by the upstream name; disabled entries appear in
`TransformReport::skipped` with `SkipReason::Disabled`.

### 14.3 Canonical priorities

| Priority | Read phase | Resolved phase |
|---|---|---|
| 10 | `ApplySourceWorkaround`, `ExtraTranslatableNodes`, `PreserveTranslatableMessages`* | `ReferencesResolver`* |
| 20–25 | `Locale`*, `TranslationProgressTotaliser`* | — |
| 50 | — | `OnlyNodeTransform` |
| 100 | `RefOnlyBulletListTransform` | — |
| 200 | — | `SigElementFallbackTransform`, `PropagateDescDomain` |
| 210 | `DefaultSubstitutions`, `MoveModuleTargets`, `HandleCodeBlocks`, `AutoNumbering`, `AutoIndexUpgrader` | — |
| 220 | `ReorderConsecutiveTargetAndIndexNodes` | — |
| 260 / 261 | `PropagateTargets`, `SortIds` | — |
| 500 | `DoctestTransform`, `GlossarySorter` | — |
| 620 | `Footnotes` (Docutils) | — |
| 750 | `SphinxSmartQuotes` | — |
| 850 / 880 | `SphinxDomains`*, `DoctreeReadEvent`* | — |
| 950 / 999 | `AddTranslationClasses`*, `FilterSystemMessages`, `RemoveTranslatableInline`* | `RemoveTranslatableInline`* |

`*` = registered seam/no-op (see §14.6).

### 14.4 Implemented transform semantics

* **DefaultSubstitutions** — `|version|`, `|release|`, `|today|` and
  `|translation progress|` are replaced by text from config when no explicit
  `SubstitutionDefinition` exists. `today` honors `config.today` or a
  deterministic UTC `today_fmt` formatter; translation progress is a documented
  Phase 26 placeholder.
* **Footnotes (620)** — autonumbered (`[1]`, `[#]`, `[#name]`), symbol (`[*]`)
  and manual footnotes/citations receive `<label>` children; references get
  `refid` and notes get `backrefs`; generated `idN` identifiers are stable and
  collision-free. Parser-only attributes (`number`, the `footnote` class) are
  removed and canonical `auto` values written back.
* **SphinxSmartQuotes** — quote pairs from a language table (English default,
  `de`, `fr`, `es`, `ja`, …), `---`→em / `--`→en (`qDe` old-school action),
  `...`→ellipsis, backslash-escape preservation, and exclusion of `Literal`,
  `LiteralBlock`, `DoctestBlock`, `Math`, `MathBlock`, `Raw`, `Image` and
  `Problematic` ancestors plus `support_smartquotes=False` nodes. Honors
  `smartquotes`, `smartquotes_action`, `smartquotes_excludes`.
* **AutoNumbering** — figures/tables/captioned code blocks without ids get an
  implicit `idN` so Phase 09's `assign_figure_numbers()` can write `fignumber`.
* **OnlyNodeTransform** — evaluates `expr` with `Tags::eval_condition`; matching
  wrappers are unwrapped, others become `comment` nodes carrying only the
  id-bearing attributes, exactly like upstream's id-transfer workaround.
  Re-walks so nested `only` nodes are handled.
* **Node normalization** — `SortIds`, `ReorderConsecutiveTargetAndIndexNodes`,
  `PropagateTargets`, `HandleCodeBlocks`, `MoveModuleTargets`,
  `AutoIndexUpgrader`, `DoctestTransform`, `GlossarySorter` (NFD case-fold sort,
  which puts `apple` before `Äpfel`), `FilterSystemMessages` and
  `RefOnlyBulletListTransform` follow the source line by line.

### 14.5 Tests

`crates/scribe-core/tests/transforms.rs` (17 tests) covers priority ordering
with registration order deliberately scrambled and stable name tie-breaking,
enable/disable + report contents, every read transform, nested `only`
filtering, footnote/citation numbering and backlinks, smart-quote language
variations and excluded nodes, and a malformed-document no-panic case.
Workspace total after Phase 10: **391 tests** (388 library/integration + 3
doctests; see `STATUS.md` for the exact gate count).

### 14.6 Seams for Phases 14 and 26

* `SphinxDomains` (read, 850) and `ReferencesResolver` (post, 10) are now wired
  to the Phase 11 `DomainRegistry` (§15.4). When a registry is attached through
  `TransformContext::with_domains`, the read pass registers objects and resolves
  every frozen `pending_xref` in place; the post pass resolves any remaining
  xrefs (idempotent). Without a registry both are no-ops, preserving the
  Phase 10 behaviour. Phase 14 will insert the intersphinx fallback between
  domain resolution and the missing-reference diagnostic (§15.8).
* `PropagateTargets`, `Footnotes`, `SphinxSmartQuotes` and the id-generation
  helper all write only frozen AST attributes, so Phase 14's resolver consumes
  the same arena ids after either phase.
* i18n slots (`PreserveTranslatableMessages`, `Locale`,
  `TranslationProgressTotaliser`, `AddTranslationClasses`, both
  `RemoveTranslatableInline` wrappers) were registered as documented no-ops in
  Phase 10 and are **activated in Phase 26** (§32). `ExtraTranslatableNodes`
  marks `gettext_additional_targets` nodes translatable, matching upstream.
* `DoctreeReadEvent` (880) is reserved for the Phase 27 extension bus.
* `sig_element_fallback` defaults to **false** because Sphinx's own translator
  supports `desc_sig_*` nodes; enabling it in `TransformContext` exercises the
  faithful `Inline` fallback for other translators.

### 14.7 Differential method (`tests/transform_differential.rs`)

Four fixtures are compared byte-for-byte after normalization:

1. **Smart quotes** — `"`/`'`/`--`/`...` education from a real `sphinx-build -b
   pseudoxml` build.
2. **Default substitutions** — `|version|`/`|release|`/`|today|` with explicit
   config values, same oracle build.
3. **Footnote numbering** — manual/auto/symbol footnotes with labels, `refid`
   and `backrefs`, same oracle build.
4. **Only post-transform** — an AST with `only` wrappers is built on both sides;
   the oracle side applies `sphinx.util.nodes.process_only_nodes` directly via
   `.oracle-venv/bin/python`, the Rust side runs `OnlyNodeTransform`.

Normalized away: the `<document>` attribute list (`source`,
`translation_progress`, `xmlns:*`), Sphinx's `docname` attribute on references,
`xml:space` on comments, and generated `idN` values renamed by first appearance
so `refid`/`backrefs` cross-links remain structurally comparable. Compared
verbatim: node kinds, nesting, all other attributes and all text content.
Known frozen-AST gaps (citation references represented as `citation_reference`
rather than `reference > inline`, missing `title_reference`) are excluded from
the byte-exact fixtures, consistent with §7.6/§9.5.
---

## 15. Phase 11 domain architecture & standard domain — FROZEN PUBLIC CONTRACT

This section is the contract Phases 12–14 (other domains, intersphinx), the
builders (17+) and the extension bus (27+) code against. The Phase 03 AST (§7)
and Phase 08 `BuildEnvironment` public shapes (§12) are not modified.

### 15.1 Crate layout and dependency direction

```
scribe-ast  ──────┐
scribe-config ────┼──► scribe-domains ──► scribe-core ──► scribe-cli
 thiserror/miette ┘         │                  │
 serde/json/dashmap/         │                  └──► scribe-builders (Phase 17+)
 parking_lot/indexmap/regex  │
                             └──► DomainEnv trait (implemented by BuildEnvironment)
```

`scribe-domains` deliberately does **not** depend on `scribe-core`: resolution
reads documents through the small [`DomainEnv`] adapter, which
`BuildEnvironment` implements. `scribe-core` depends on `scribe-domains` so the
Phase 10 transforms can dispatch. The only `scribe-core` changes are additive:

* `env.rs`: private side table `domain_data: BTreeMap<String, serde_json::Value>`
  with `domain_data`, `set_domain_data`, `domain_data_entries`,
  `set_domain_data_entries`; `EnvironmentSnapshot::domain_data` is a new
  key-sorted field and `ENV_FORMAT_VERSION` is `2` (old caches are cold).
* `transforms.rs`: `TransformContext::{domains, domain_diagnostics}` fields,
  `with_domains` / `domain_sink` / `domain_context` /
  `push_domain_diagnostic` / `take_domain_diagnostics` / `domain_diagnostics`
  methods; `TransformReport::domain_diagnostics` field. Existing constructors,
  transform names and priorities are unchanged.

### 15.2 The frozen `Domain` trait (`scribe_domains::Domain`)

```rust
pub trait Domain: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str;
    fn object_types(&self) -> &[ObjectType];
    fn directive_names(&self) -> &[&'static str];
    fn role_names(&self) -> &[&'static str];
    fn initial_data(&self) -> DomainData;
    fn data_version(&self) -> u32 { 1 }

    fn clear_doc(&mut self, docname: &str);
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData);
    fn process_doc(&mut self, ctx: &DomainContext<'_>, docname: &str,
                   document: &Document) -> DomainResult<()>;
    fn check_consistency(&self, ctx: &DomainContext<'_>) -> Vec<DomainDiagnostic>;

    fn resolve_xref(&self, ctx: &DomainContext<'_>, typ: &str, target: &str,
                    node_id: NodeId, contnode_id: NodeId, document: &Document)
        -> DomainResult<Option<ResolvedReference>>;
    fn resolve_any_xref(&self, ctx: &DomainContext<'_>, target: &str,
                        node_id: NodeId, contnode_id: NodeId, document: &Document)
        -> DomainResult<Vec<(String, ResolvedReference)>>;

    fn get_objects(&self) -> Vec<ObjectEntry>;
    fn get_type_name(&self, objtype: &str, primary: bool) -> String { /* default */ }
    fn get_full_qualified_name(&self, node: &Node) -> Option<String> { None }
    fn export_data(&self) -> DomainResult<DomainData>;
    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()>;
}
```

Contract details:

* **Ownership / mutation.** Mutating methods take `&mut self`; the registry
  serializes them with `parking_lot::Mutex` and read-only resolution with the
  same lock. Resolvers are pure with respect to the AST: they receive `&Document`
  and return an owned descriptor. This preserves the frozen arena-id invariant
  (§7.1) and avoids borrow conflicts in the pipeline.
* **`node_id` / `contnode_id`** are the live `pending_xref` and its first
  ordinary child (`pending_xref_condition` nodes are skipped; `resolved` / `*`
  conditions are preferred, mirroring `ReferencesResolver`). Domains may read
  typed fields through `Node::pending_xref()` but must never mutate the node.
* **`ResolvedReference`** is the replacement for upstream's `nodes.reference`
  subclass: `{node_kind, refid, refuri, reftitle, title, classes, docname,
  passthrough, extra_attrs}`. `node_kind` maps to `Reference`,
  `NumberReference` or `DownloadReference`. Exactly one of `refid` / `refuri`
  is normally set; `title: Some(_)` replaces the inner content with a fresh
  inline node, `None` reuses `contnode_id`. `passthrough = true` asks the
  pipeline to emit the content node and **suppress** the missing-reference
  diagnostic (upstream returns `contnode` for a disabled `:numref:`). The
  pipeline emits `reftitle` as `title=` on `number_reference`, matching
  `StandardDomain.build_reference_node`.
* **`None` from `resolve_xref`** means "no object matched". The registry then
  runs the `missing-reference` fallback: nitpicky warning (§15.6) or silent
  fallback to the content node.
* **`resolve_any_xref`** returns `(role, reference)` pairs where `role` is the
  *fully qualified* role that would have produced the reference (e.g.
  `std:ref`). Standard-domain first, then other domains sorted by name.
  More than one candidate emits the upstream
  `more than one target found for 'any' cross-reference …` warning.
* **`ObjectEntry`** is the six-uple `Domain.get_objects()` converted to an owned
  struct: `name`, `display_name`, `objtype`, `docname`, `anchor`, `priority`
  (`1` default, `0` important, `2` unimportant, `-1` hidden).
* **`DomainData`** is `{version: u32, payload: serde_json::Value}`. A version
  mismatch is a cold inventory (never a panic). `export_data` / `replace_data`
  are the only persistence hooks.
* **`DomainContext`** (cheap `Copy`) carries `env: Option<&dyn DomainEnv>`,
  `config: Option<&Config>`, `builder_name`, `builder_format`, current `docname`,
  `program: Option<&str>` (`std:program`) and a reference to a `DiagnosticSink`.
  `with_program` / `with_docname` create per-xref copies. `get_strings_flexible`
  handles Python tuple config values such as `nitpick_ignore`.
  `relative_uri` is a faithful port of `sphinx.util.osutil.relative_uri`
  (including the self-link `""` and directory `"./"` special cases);
  `uri_suffix` derives the output extension from `builder_format` / config.

### 15.3 `DomainEnv` adapter

```rust
pub trait DomainEnv: Send + Sync {
    fn docnames(&self) -> Vec<String> { vec![] }
    fn has_doc(&self, _docname: &str) -> bool { false }
    fn title(&self, _docname: &str) -> Option<String> { None }
    fn doctree(&self, _docname: &str) -> Option<&Document> { None }
    fn domain_data(&self, _domain: &str) -> Option<&serde_json::Value> { None }
}
```

`BuildEnvironment` implements it from `all_docs`, `DocMeta::title` (falling
back to the stored doctree's metadata title), `get_doctree` and the new side
table. Unit tests use a tiny in-memory `TestEnv`. This is the seam Phase 14
uses to consult inventory metadata without coupling domains to the environment.

### 15.4 Registry, resolution pipeline and transform wiring

```rust
let registry = DomainRegistry::with_standard();
registry.register(MyFutureDomain::new());          // Phase 12/13
let ctx = TransformContext::new(Some(&config), None, "html", "html")
    .with_env(&env)                                 // optional
    .with_domains(&registry);
apply_transforms(&mut document, &ctx, Phase::Read); // registers + resolves
apply_transforms(&mut document, &ctx, Phase::Resolved); // resolves remainder
let warnings = report.domain_diagnostics();        // precise miette codes
```

* `DomainRegistry` is `DashMap<String, Arc<Mutex<Box<dyn Domain>>>>` plus a
  derived `DashMap<String, Vec<ObjectEntry>>` symbol table keyed by lowercased
  name. `rebuild_object_table` sorts each bucket by
  `(priority, objtype, docname, anchor)` so the most important object wins;
  `symbol_lookup` returns the whole bucket for later fuzzy/suggestion phases.
* `get_objects()` is deterministic: all domains sorted by name, then each
  domain's own ordering, then a final stable sort by
  `(name, objtype, docname, anchor)`. `get_objects_with_env` additionally yields
  whole-document entries (`doc`, priority `-1`). `check_consistency` diagnostics
  are sorted by `(docname, target, code)`.
* **Resolution is two-phase by design:** (1) registry returns owned descriptors;
  (2) `resolve_pending_xrefs` walks the document and mutates it in place, then
  emits missing-reference diagnostics. Phase 14 inserts its intersphinx lookup
  between (1) and (2) — by trying external inventories before the registry
  decides to warn — without changing either signature.
* `SphinxDomains` (read, 850) calls `process_document` then
  `resolve_pending_xrefs`; `ReferencesResolver` (post, 10) calls only the latter.
  Both are no-ops when `ctx.domains` is `None`.
* Role precedence: the frozen §9/§10 `RoleRegistry` only *emits* nodes; Phase 06
  classifies `confval`, `label`, `ref`, `doc`, `numref`, `envvar`, `option`,
  `token`, `term`, `keyword`, `download`, `any` as xrefs. The domain registry is
  authoritative for *resolution* (dispatch on `refdomain`, defaulting to `std`).
  Domain-owned names (`std:*`, and later `py:*`, …) therefore always win over
  generic fallback because only domains produce `ResolvedReference`s; generic
  roles without a domain (PEP/RFC, abbr, …) never reach domain dispatch.

### 15.5 `StandardDomain` behavior (upstream `sphinx/domains/std`)

`name = "std"`, `label = "Default"`, `data_version = 1`. Object types:
`term`, `token`, `label` (roles `ref`, `keyword`), `confval`, `envvar`,
`cmdoption` (role `option`), `doc`. Directive names: `program`, `cmdoption`,
`option`, `confval`, `envvar`, `glossary`, `productionlist`. Roles: those plus
`numref`, `download`, `label`, `any`. Inventories: `objects`, `terms`,
`progoptions`, `labels`, `anonlabels`, `downloads` (all `BTreeMap`s, flattened
JSON keys `type \x1f name`), seeded with `genindex` / `modindex` / `search`.

| Role | Resolution |
|---|---|
| `ref` | Lowercases the target (upstream `XRefRole(lowercase=True)` at parse time; ours at resolve time). Explicit title → `anonlabels`; implicit → `labels`, whose section/caption title becomes the inline text. Anchor local (`refid`) or cross-document (`refuri`). |
| `doc` | `docname_join(refdoc, target)` relative to the xref's `refdoc`; implicit title from `env.title` or explicit `Title <doc>`; always a `refuri` (self-link `""`). `#fragment` targets are a documented Phase 11 extension (upstream 9.x warns `unknown document`). |
| `numref` | `labels` first (with caption) then `anonlabels`; reads `fignumber` / `secnumber` attributes written by Phase 09, determines `figure` / `table` / `code-block` / `section`, honors `numfig` (disabled → passthrough), substitutes both `Fig. %s` and `Fig. {number}` / `{name}` forms, emits `number_reference`. |
| `term` | Exact `term` object first, then case-insensitive `terms` fallback. Glossary aliases (multiple terms in one definition) share the first term's anchor. |
| `option` | `(program, option)` lookup with `std:program` context; strips `=value`, `[=value]`, ` value`; folds subcommands (`git commit --amend` → program `git-commit`). Generated anchors match upstream `cmdoption[-program]-name`. |
| `envvar`, `confval`, `token`, `label` | Generic object lookup via the object type's roles; `label` uses named labels. |
| `keyword` | Named labels only; reuses the original content node. |
| `download` | Absolute URLs pass through; relative paths resolve to `_downloads/<basename>` with `download_reference`. Upstream computes this in its asset collector; Phase 11 registers file objects in the domain instead (§15.9). |
| `any` | `doc` first via the registry, then std's `ref`, `option` and all object types, then other domains. |

`process_doc` extracts explicit labels from `Target` nodes and from nodes that
received a propagated target (implicit section titles are excluded, exactly
like upstream's `nametypes` test), glossary terms from `Term` descendants of
`glossary`, token names from `production` nodes, program/option/envvar/confval
objects from the lightweight `desc` nodes emitted by the new RST directives, and
download targets from `download_reference` nodes. Duplicate labels/objects emit
`scribe::domains::duplicate_object` diagnostics; the first declaration wins for
program options (upstream `add_program_option`), the latest for other objects.

### 15.6 Nitpicky diagnostics

`DomainDiagnostic` is a `thiserror` + `miette::Diagnostic` with a **dynamic code**
(implemented manually, not via derive). Codes: `scribe::domains::missing_reference`,
`duplicate_object`, `malformed_object`, `inconsistency`. Missing-reference
diagnostics carry the exact upstream message, structured `typ` / `target` /
`docname`, the frozen AST `Span` as a labelled `here` span, and a corrective
`help`. Exact messages ported from the oracle:

* `ref` (no anonlabel): `undefined label: 'x'`
* `ref` (anonlabel but no title): `Failed to create a cross reference. A title or caption not found: 'x'`
* `numref`: `undefined label: 'x'`; `term`: `term not in glossary: 'x'`;
  `keyword`: `unknown keyword: 'x'`; `doc`: `unknown document: 'x'`;
  `option`: `unknown option: 'x'`
* other domains: `{domain}:{typ} reference target not found: {target}`;
  domain-less: `'{typ}' reference target not found: {target}`

Warnings are emitted when `nitpicky = true` **or** the xref's `refwarn` flag is
set. `suppress_warnings = ["ref", "ref.<typ>"]` always wins;
`nitpick_ignore` / `nitpick_ignore_regex` are consulted only in nitpicky mode,
accepting both `"std:doc:target"` and Python tuples `("std:doc", "target")`;
for `std` types the plain `"doc:target"` spelling also matches. Python `%r`
quoting is reproduced with `py_repr` (single quotes).

Diagnostics reach builders twice: precisely as `report.domain_diagnostics()`
(with the `scribe::domains::*` code) and mirrored into
`report.diagnostics` as `TransformDiagnostic`s so legacy consumers still see a
warning. `TransformReport::has_diagnostics` covers both sinks.

### 15.7 Differential method (`tests/differential.rs`)

Five fixtures are run through the pinned oracle with
`sphinx-build -b pseudoxml -E -n -q`: explicit/implicit `:ref:` and `:doc:`,
glossary terms + program options, `:numref:` with `numfig = True`,
envvar/confval/token objects, and the oracle's own
`sphinx/tests/roots/test-domain-py-xref-warning` root. Oracle stderr warnings
are reduced to the message between `WARNING:` and ` [type]`, and compared to
our `DomainDiagnostic::message` verbatim. Pseudo-XML is reduced to
*resolution facts*: reference tags (`reference`, `number_reference`,
`download_reference`) and unresolved `xref` inline/literal nodes with only
`refid`, `refuri`, `reftitle`, `title`, `classes`, `internal` and the first inner
text line. Normalized away: the `<document>` attribute list, generated ids
(including case-only differences like `envvar-HOME` vs `envvar-home`), source
paths and namespace declarations. Phase 09 numbering metadata is injected by
the harness for the numref fixture because Phase 11 consumes, not computes,
`fignumber`. The frozen AST is never edited by domain code.

### 15.8 Phase 14 intersphinx extension seam

Phase 14 adds an `IntersphinxDomain` (or a resolver service) implementing the
same `Domain` trait, plus a `missing-reference` fallback between registry
resolution and diagnostic emission. It must (a) return the same
`ResolvedReference` shape, (b) register aliases with `priority = 2` so local
objects win, and (c) write diagnostics through the same `DiagnosticSink` with
new codes under the `scribe::domains::` prefix. No signature changes are needed.

### 15.9 Known limitations / deferred

* The RST parser's `productionlist` does not yet emit the internal token
  cross-references upstream creates for tokens *inside* productions; only the
  production name is registered as a token object. The differential fixture
  avoids backticked tokens in productions.
* `:download:` URIs are synthesized as `_downloads/<basename>`; copying files
  and the authoritative `env.dlfiles` map belong to a builder phase.
* Explicit labels attached to `Section` nodes by upstream's `PropagateTargets`
  cannot be distinguished from implicit section titles in the frozen AST and
  are not registered; use `.. _label:` before a non-section block or the new
  `.. label::` directive.
* Inventory data is exported/loaded explicitly via
  `DomainRegistry::export_domain_data` / `load_from_env`; automatic flush at
  the end of a build is a builder-phase responsibility.
## 16. Phase 12 Python domain (`py`)

This section documents the Python-domain signature model, resolution rules and
differential method. The frozen AST (§7), frozen `BuildEnvironment` (§12) and
frozen `Domain` trait (§15) are **unchanged**: Phase 12 only adds a new `Domain`
implementation, a `scribe-rst` directive/role seam and additive config keys.

### 16.1 Upstream sources inspected

Behavior was extracted from the pinned oracle sources before implementation:

* `sphinx/sphinx/domains/python/__init__.py` — `PythonDomain`, `PyFunction`,
  `PyClasslike`, `PyMethod`, `PyVariable`, `PyProperty`, `PyTypeAlias`,
  `PyModule`, `PyCurrentModule`, `PyXRefRole`, `PythonModuleIndex`,
  `find_obj`, `note_object`, `builtin_resolver`.
* `sphinx/sphinx/domains/python/_object.py` — `py_sig_re`, `PyObject`,
  `PyXrefMixin`, `PyTypedField`, `PyGroupedField`, `add_target_and_index`.
* `sphinx/sphinx/domains/python/_annotations.py` — `_parse_annotation`,
  `parse_reftarget`, `type_to_xref`, `_parse_arglist`, `_pseudo_parse_arglist`,
  `Optional`/`Union` PEP-604 rewriting.
* `sphinx/sphinx/util/nodes.py::_make_id` — dot/underscore/case-preserving
  anchors (`alias_module.Handler`, not `alias-module-handler`).
* Fixtures: `test-domain-py{,-python_use_unqualified_type_names,
  -xref-type-alias,-xref-warning}` and their oracle pseudoxml trees.

### 16.2 Crate layout and the skeleton seam

`scribe-rst` cannot depend on `scribe-domains` (that would reverse §15.1's
acyclic graph), so the split mirrors upstream's parse/registry boundary:

```
scribe-rst/py:object directive
  └─ emits [index, desc] with:
       objtype, py:directive, py:signature (raw argument),
       py:options (JSON PyOptions), py:sig_base,
       empty desc_signature placeholder + desc_content (parsed normally)
  │
  ▼  scribe_domains::python::prepare_document(&mut Document)
  ├─ parses the signature and fills desc_signature with desc_* / desc_sig_* nodes
  ├─ rewrites typed field bodies (:param/:type/:rtype/:raises:) with xrefs
  ├─ computes docutils-compatible anchors and index entries
  └─ annotates every py pending_xref with py:module / py:class read context
  │
  ▼  PythonDomain::process_doc(&Document) + resolve_pending_xrefs
```

`prepare_document` is called by the Phase 10 `SphinxDomains` read transform
(additive call site) and by all Phase 12 test harnesses. It is a no-op for
documents without Python directives. `DomainRegistry::builtin()` now registers
both `std` and `py`; `DomainRegistry::with_standard()` remains standard-only
for Phase 11 regressions.

### 16.3 Signature model

`parse_sig_re` is a hand-written equivalent of upstream's verbose regex and
captures `(prefix, name, type_params, arglist, retann)` without backtracking.
`build_desc_signature` emits, in order:

1. keyword prefix `desc_annotation` (`final`, `abstract`/`abstractmethod`,
   `async`, `classmethod`, `static`, `class`, `exception`, `type`, `property`);
2. `desc_addname` (`sig-prename descclassname`) for the displayed module/class
   qualifier when `add_module_names` holds;
3. `desc_name` (`sig-name descname`);
4. `desc_type_parameter_list` → `desc_type_parameter` for `[T]`, `[*Ts]`,
   `[T: bound = default]`;
5. `desc_parameterlist` → `desc_parameter` with `desc_sig_name`,
   `desc_sig_punctuation`, `desc_sig_operator`, `inline.default_value`, and
   PEP 570 `/` / PEP 3102 `*` separators;
6. `desc_returns` for `-> annotation`;
7. `desc_annotation` for `:annotation:`, `py:type` `:canonical:`, and the
   `:type:` / `:value:` options.

`desc_sig_*` CSS classes match upstream (`k` keyword, `w` space, `n` name,
`o` operator, `p` punctuation, `kt` keyword-type, `m` number, `s` string),
and every node carries the frozen `Span` of the directive marker. Annotation
tokenization is linear: identifiers become `pending_xref`s (`reftype` `class`
or `obj`, the latter for `None` and `typing.*`), punctuation/literals become
`desc_sig_*` leaves, and `Optional[X]` / `Union[X, Y]` are rewritten to
`X | None` / `X | Y` exactly like `_unparse_pep_604_annotation`.

### 16.4 Module/class context and full names

* `.. currentmodule::` sets the persistent module context; `None` clears it.
* `.. py:module::` emits an upstream-style `index` + `target` (no `desc`) and
  also sets the persistent module context; `module` entries are registered for
  `py-modindex` with synopsis/platform/deprecated metadata.
* A `:module:` option scopes a module override to that description's content.
* Class-like directives (`class`, `exception`) push a nested class context for
  their content; other object directives use the explicit signature prefix or
  the enclosing class. Context is restored on exit, so nested
  `NestedParentA.NestedChildA.subchild_2` names resolve as upstream.
* The object key is `module.fullname`; aliases from `:canonical:` (except on
  `py:type`, where canonical is displayed instead) use `aliased = true` and
  never override a real declaration.

### 16.5 Xref resolution rules

`PythonDomain::resolve_xref` ports upstream `find_obj` exactly: exact match,
then `class.name`, `module.name`, `module.class.name` (non-specific mode), or
module/class-scoped exact match plus a fuzzy `.name` suffix search (specific
mode, used by the `.` modifier and `:any:`). Additional upstream behavior:

* `func` / `meth` strip a trailing `()` before lookup (`fix_parens`) and append
  `()` to implicit display titles (`add_function_parentheses`);
* `~` shortens the *title* only, `.` sets `refspecific = true` and shortens the
  *target*; explicit `Title <target>` titles are preserved;
* `class` falls back to `data` then `attr` (type aliases), `attr` to `meth`
  (legacy properties), `meth` to the hidden `_prop` role;
* multiple matches prefer the non-aliased one; genuine ambiguity emits the
  exact upstream `more than one target found for cross-reference …` warning;
* builtin/typing/`None` targets are passed through without a nitpicky warning
  (`builtin_resolver`), as are type-annotation xrefs documented as data/attr;
* `resolve_any_xref` searches all types in specific mode, skips aliases when
  ambiguous and returns `py:<role>` pairs with `xref any py py-<role>` classes;
* `python_use_unqualified_type_names` emits `pending_xref_condition`
  `resolved` (short name) / `*` (full name) children; §7's two-phase resolver
  now uses `resolved` content on success and `*` content on fallback.

`py-modindex` is exposed as `PythonDomain::py_modindex(docnames, prefixes)`
returning `ModuleIndexEntry` groups with upstream's `subtype` heuristics and
collapse flag, ready for the Phase 17 HTML writer.

### 16.6 Differential method (`tests/python_differential.rs`)

Real oracle builds (`sphinx-build -b pseudoxml -E -n -q`) for four fixture roots
are compared with our pipeline. Both trees are parsed into indentation trees;
comparison uses two reductions:

1. **signature facts** — each `desc_signature`'s kept attributes (`fullname`,
   `class`, `module`, `classes`, reference attrs) and the ordered descendant
   tag/text sequence;
2. **resolution facts** — reference tags and `refid` / `refuri` / `reftitle` /
   `classes` / inner text, plus fallback `xref` inline/literal nodes (Phase 11
   §15.7 normalization);
3. **warnings** — exact `WARNING:` message lines, compared verbatim.

Normalized away: `py:*` skeleton attributes, `_toc_*` hints, `xml:space`,
`multi_line*` flags, generated ids (case-insensitive, `.pseudoxml` suffix
removed), document attributes and namespace declarations, and the toctree-only
`index.rst` of `test-domain-py` (Phase 09 owns those links). The frozen AST is
never edited by domain code.

### 16.7 Known limitations / deferred

* Multi-line signature wrapping (`python_maximum_signature_line_length`) is
  parsed but always rendered single-line; the config flag is registered for
  forward compatibility.
* `python_display_short_literal_types` and `strip_signature_backslash` are
  accepted but do not alter rendering yet.
* Field-list rendering covers the upstream `param`/`type`/`rtype`/`raises`
  groupings and preserves existing role references in typed bodies; exotic
  Docutils field customizations are out of scope.
* Default-value inline nodes keep raw source text rather than Sphinx's
  `sphinx.util.inspect` reformatting for complex expressions.
* Signature nodes carry the directive marker's `Span` (plus `py:sig_base` on
  the skeleton); per-token byte spans inside the raw signature are not yet
  materialized. Spans are valid and contained, but coarser than upstream's
  source positions.
## 17. Phase 13 C, C++, JavaScript, reST & Math domains

Phase 13 adds eight `Domain` implementations on top of the frozen §15 contract:
`c`, `cpp`, `js`, `rst`, `math`, `changeset`, `citation` and `index`. The frozen
§7 AST, §12 `BuildEnvironment` and §15 `Domain` trait public shapes are
**unchanged**: this phase only adds new implementations, an additive RST
directive/role seam and one root-level `prepare_document` dispatcher. All nine
prior domains (`std`, `py` + the eight here) are registered by
`DomainRegistry::builtin()`; `with_standard()` remains std-only for Phase 11
regressions.

### 17.1 Upstream sources inspected

Behavior was extracted from the pinned oracle before implementation:

* `sphinx/sphinx/domains/c/{__init__,_ast,_parser,_symbol,_ids}.py` — object
  types, `CDomain.directives/roles`, `Symbol` nesting, `DefinitionParser`.
* `sphinx/sphinx/domains/cpp/{__init__,_ast,_parser,_symbol,_ids}.py` — object
  types, templates, operators, overload sets, Itanium-style IDs.
* `sphinx/sphinx/domains/javascript.py` — `JSObject`/`JSCallable`/
  `JSConstructor`/`JSModule`/`JSXRefRole`, module context and `this` nesting.
* `sphinx/sphinx/domains/{rst,math,changeset,citation,index}.py` — smaller
  domain surfaces.
* Fixtures: `test-domain-c`, `test-domain-cpp`, `test-domain-js` and their
  oracle `pseudoxml` trees.

### 17.2 Crate layout and the skeleton seam

`scribe-domains` gains modules `c/`, `cpp/`, `javascript.rs`, `rst.rs`,
`math.rs`, `changeset.rs`, `citation.rs`, `index.rs` and the shared
`sigbuild.rs` emitter. `scribe-rst` gains `domain_directives.rs`.

```
scribe-rst domain_directives.rs        (raw signature + options in <domain>:* attrs)
   └─ desc skeleton (index + desc + empty desc_signature + desc_content)
   ▼
scribe_domains::prepare_document(&mut Document)                    (root dispatcher)
   ├─ python::prepare_document  (Phase 12)
   ├─ c::prepare_document       (declarator parser → desc_sig_* tree)
   ├─ cpp::prepare_document     (C++ parser + Itanium-ID subset)
   ├─ javascript::prepare_document
   └─ rst::prepare_document
   ▼
DomainRegistry::builtin()  ── process_document ── resolve_pending_xrefs
```

The root `prepare_document` replaces Phase 12's direct Python re-export but
calls the Python pass first, so all Phase 12 callers and the Phase 10
`SphinxDomains` read seam behave identically. Each sub-pass is additive and
no-op for documents without its skeleton.

### 17.3 Shared signature emitter (`sigbuild.rs`)

`SigToken` is the shared atomic signature token (`KeywordType`, `Keyword`,
`Name`, `Punctuation`, `Operator`, `Space`, `Number`, `Literal`, `Ref`).
`SigBuilder` materializes tokens as the frozen `desc_sig_*` nodes with exactly
Sphinx's CSS classes (`kt`, `k`, `n`, `p`, `o`, `w`, `m`, `s`) and emits type /
qualifier cross-references as `pending_xref` nodes annotated with the domain
read scope. This guarantees identical structure across C/C++/JS and avoids
triplicating node-construction code.

### 17.4 Per-domain coverage

| Domain | Object types | Directives | Roles | Inventory | Resolution |
|---|---|---|---|---|---|
| `c` | 10 (incl. generated `functionParam`) | 13 (object + namespace/push/pop + alias) | `member,data,var,func,macro,struct,union,enum,enumerator,type,expr,texpr` | `c.<scope>.<name>` anchors | scope + suffix `identifier` fallback, duplicate warnings |
| `cpp` | 10 (incl. `functionParam`, `templateParam`) | 16 | `any,class,struct,union,func,member,var,type,concept,enum,enumerator,expr,texpr` | 4 `_CPPvN` aliases + readable fallback | scope lookup, overload buckets, `.`/`~` modifiers |
| `js` | 6 | 6 | `func,meth,class,data,attr,mod` | fullname + `module-<name>` | module/object nesting, `.`/`~`, `:any:` |
| `rst` | 3 | 3 | `dir,role` | `(objtype, name)` | role→type lookup, `:any:` |
| `math` | 0 | 0 | `numref,eq` | label → `(docname, eqno)` | `number_reference` with `math_eqref_format` |
| `changeset`, `citation`, `index` | 0 | 0 | 0 | none | no-op (event/inventory completeness) |

**C declarator parser** handles primitive/named types, cv-qualifiers, pointers,
arrays, function parameter lists, function pointers (`(*cb)(...)`), dotted
names and anonymous fields (`@data` → `[anonymous]` in display). Signature
xrefs are resolved in the enclosing namespace/nested scope; `:c:expr:` /
`:c:texpr:` pass through as code without lookup.

**C++ declarator/type parser** covers `::` names, `template<...>`, references
(`&`/`&&`), cv-qualifiers, builtin/template types, operators (`operator+`,
`operator()`, `operator[]`, literal operators), constructors/destructors and
`type X = Y` aliases. IDs follow the `_CPPv4/3/2` + readable subset of
`_ids.py`; overloads share a canonical-name bucket and `resolve_overload`
selects by argument count.

**JS** tracks `js:module` and `js:object` read context on every pending xref;
`:js:meth:` lookup prefers `module.object.name`, then `object.name`, then
`.name` suffix in specific mode. Classes push their name onto the object
scope for nested methods.

**Math** consumes Phase 09/10 numbering via labelled `math_block` nodes: a
`fignumber` attribute (written by the toctree/numfig collector) supersedes the
sequential equation counter. It emits `number_reference` nodes with
`reftitle` = `math_eqref_format` (default `({number})`). Standard-domain
`:numref:` owns figure/table numbering; the math domain owns equation numbering.

### 17.5 Tests

* `tests/domains13.rs` — 22 tests: registry/metadata, every C/C++/JS declarator
  form, scope lookup, overload buckets, Itanium-ID subset, `:any:` dispatch,
  duplicate warnings, module/nesting context, persistence and two adversarial
  corpora (no panics).
* `tests/domain_differential.rs` — 6 tests: real oracle `sphinx-build -b
  pseudoxml -E -n -q` for `test-domain-c`, `test-domain-cpp`, `test-domain-js`;
  per-signature ordered leaf structure, reference resolution facts and exact
  cross-reference warnings.

### 17.6 Differential method and parity

Both trees are reduced to an ordered sequence of `(tag, text)` leaves per
`desc_signature`; attributes, `xml:space`, `_toc_*`, `is_multiline` /
`multi_line*`, generated ids and document/namespace attributes are normalized
away. Cross-reference wrappers reduce to their inner `desc_sig_*` leaf so a
resolved `reference` and a pending `pending_xref` compare identically.

Measured parity on the three gate fixtures:

| Fixture | Signature structural parity | Resolution facts | Warnings |
|---|---|---|---|
| `test-domain-c` (index) | **16/16 = 100%** | all oracle facts present | `c:identifier` messages exact |
| `test-domain-cpp` (index) | **17/19 = 89.5%** | all oracle facts present | exact messages for resolved types |
| `test-domain-js` (roles) | **11/11 = 100%** | all oracle facts present | — |

Assertions require ≥ 95% (C, JS) and ≥ 80% (C++) structural parity.

**Documented divergences / tracked gaps** (intentional, not gate failures):

* **C++ `_CPPvN` id text** is canonicalized in resolution-fact comparison: the
  subset mangles names, nested names, cv/pointers/references, builtin types and
  simple parameter lists but not full Itanium ABI substitutions, SFINAE,
  `decltype`, lambda/closure types or pointer-to-member syntax.
* **C++ two signatures** (the `protected MyScopedVisibilityEnum : ...`
  underlying-type declaration and the `std::vector<int> List` template
  punctuation) differ only in visibility/angle-bracket placement; both still
  register and resolve correctly.
* **Expression roles** `:c:expr:` / `:cpp:texpr:` render as code but do not
  emit the oracle's internal token xrefs for sub-expressions.
* **Multi-line signature wrapping** (`c_maximum_signature_line_length`,
  `cpp_maximum_signature_line_length`, `javascript_maximum_signature_line_length`)
  is parsed but always rendered single-line.
* **Per-token byte spans** inside domain signatures carry the directive
  marker's `Span` (valid and contained, but coarser than upstream).
* `changeset` / `citation` / `index` expose empty inventories: the changes
  builder (Phase 25) and citation transform (Phase 10) own their data.
---

## 18. Phase 14 cross-reference resolution engine & `intersphinx`

Phase 14 adds the structured resolution engine and the `intersphinx` subsystem
in `crates/scribe-domains`. The frozen §7 AST, §12 `BuildEnvironment` and §15
`Domain` trait / `ResolvedReference` shapes are **unchanged**: every addition is
a new module, an additive method on `DomainRegistry`, or an additive config key.

### 18.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/util/inventory.py` — `InventoryFile.loads` / `_loads_v1` /
  `_loads_v2` / `dump`, `_InventoryItem`, the entry regex with embedded-space
  handling, `$`-suffix expansion, `posixpath.join` location joining, the
  case-insensitive `std:label` / `std:term` ambiguity tracking, and the
  `zlib.compressobj(9)` writer loop.
* `sphinx/sphinx/ext/intersphinx/{__init__,_load,_resolve,_shared}.py` —
  `validate_intersphinx_mapping`, `load_mappings`, `_fetch_inventory_group` /
  `_fetch_inventory_url` / `_fetch_inventory_file`, `InventoryAdapter`,
  `_resolve_reference` (role → object-type mapping, disabled reftypes,
  full-qualified-name fallback, case-insensitive matching),
  `_create_element_from_result` (`internal=False`, `reftitle`, display-name
  handling) and the `IntersphinxRole` / `IntersphinxRoleResolver` for
  `:external:` and `:external+<name>:`.
* `sphinx/tests/test_util/test_util_inventory.py` and
  `sphinx/tests/test_util/intersphinx_data.py` — canonical `INVENTORY_V1/V2`
  fixtures; `sphinx/tests/test_ext_intersphinx/test_ext_intersphinx.py::test_intersphinx_role`
  — the offline local-inventory mapping pattern and exact warning text.

### 18.2 Crate layout

```
crates/scribe-domains/src/
├── intersphinx/
│   ├── mod.rs        # IntersphinxError / IntersphinxResult + public re-exports
│   ├── inventory.rs  # Inventory / InventoryItem / DumpConfig: v1/v2 reader + byte-exact v2 writer
│   ├── cache.rs      # InventoryCache: BLAKE3-keyed envelope, deterministic expiry
│   ├── load.rs       # IntersphinxMapping / FetchOptions / parallel + async fetching
│   └── resolve.rs    # IntersphinxResolver / ExternalRole / intersphinx diagnostics
└── xref.rs           # XrefResolver / XrefResolutionReport / fuzzy suggestions
```

New workspace dependencies: `flate2` (**zlib backend**, so the compressed
bytes match the oracle's zlib-ng exactly), `reqwest` (blocking + `rustls-tls`),
`tokio` (`rt-multi-thread`, `macros`, `time`), `url`, `strsim`, `blake3`,
`rayon`. Additive config keys: `intersphinx_timeout`,
`intersphinx_resolve_self`.

### 18.3 `objects.inv` format and byte-compat method

`Inventory::loads(bytes, base_uri)` accepts v2 (and gracefully reads v1). The
v2 header is exactly four lines; the body is zlib level 9 of lines matching
`(.+?)\s+(\S+)\s+(-?\d+)\s+?(\S*)\s+(.*)`. Types without `:` are skipped;
duplicate `py:module` entries keep the first; `std:label` / `std:term` names
are case-insensitive and genuine ambiguity is recorded in
`Inventory::ambiguities`. A location ending in `$` appends the object name
*after* any `#`, then is joined to the base URI with POSIX semantics (an
absolute location replaces the base).

`Inventory::dump(objects_by_domain, DumpConfig{project,version}, builder_uri)`
iterates domains sorted by name and each domain's objects sorted by the full
six-uple, shortens an anchor ending in the full name to `$`, collapses
`dispname == fullname` to `-`, escapes `\s+` in header fields and compresses
the single concatenated body with `Compression::best()`.

**Byte-compat evidence:** `tests/inventory.rs::dump_is_byte_exact_against_oracle_zlib`
runs `.oracle-venv/bin/python crates/scribe-domains/tests/fixtures/gen_inventory.py`,
which reproduces upstream's `InventoryFile.dump` loop with the pinned zlib-ng,
and asserts `Vec<u8>` equality with our writer. It passes on a fixture with
embedded spaces, three domains, `$` anchors, display-name collapse and
whitespace in the project/version header. `flate2`'s `zlib` backend links the
same system libz-ng as the oracle (`ZLIB_RUNTIME_VERSION = 1.3.1.zlib-ng`),
which is required for byte equality; the `miniz_oxide` backend is not.

### 18.4 Resolver order, report and seam

`XrefResolver::resolve_document` (single document) and
`XrefResolver::resolve_documents` (rayon over independent documents) walk every
frozen `pending_xref` and apply this exact order:

1. **local domain** — `DomainRegistry::resolve_xref` on the xref's `refdomain`;
2. **`:any:`** — `DomainRegistry::resolve_any_xref` across all domains
   (standard domain first), with upstream's "more than one target" warning;
3. **intersphinx** — `IntersphinxResolver::resolve_reference`: role → object
   types via the owning domain, backwards-compat aliases (`std:option`,
   `py:method`), `intersphinx_disabled_reftypes`, case-insensitive std lookup,
   and a second attempt with `Domain::get_full_qualified_name`. Named
   `:external:` inventories bypass disabled reftypes and are searched
   exclusively; `:any:` searches the merged inventory across all domains;
4. **suggestions** — Jaro–Winkler (`strsim`) ranking of local + inventory names
   with threshold `0.8`, appended as ` Did you mean: 'x', 'y'?` to the
   diagnostic's **help** only (the Phase 11 `message` is never modified);
5. **standard Phase 11 diagnostic** — `missing_reference_diagnostic`, or the
   upstream-specific `external {domain}:{typ} reference target not found`
   message for `:external:` refs.

The structured `XrefResolutionReport` carries total / local / any / intersphinx
counts, unresolved and passthrough counts, per-document `ResolveReport`s,
suggestions and deterministic diagnostics.

**Frozen-seam integration (§15.8):** `DomainRegistry::set_intersphinx` /
`attach_intersphinx` store an `Arc<IntersphinxResolver>` on the registry. The
existing `resolve_pending_xrefs` entry point (used by the Phase 10
`SphinxDomains` and `ReferencesResolver` seams) consults it in its `None`
branch before emitting a warning. No signature changed: builders merely
attach the service once.

**`:external:` roles:** `ExternalRole::parse` handles
`external[:]<domain>:<role>` and `external+<inv>:<domain>:<role>`;
`ExternalRole::annotate` writes the two upstream attributes (`intersphinx`,
`inventory`) onto a pending xref plus its `refdomain` / `reftype`. The RST
parser's static role table (§9.4) does not yet recognise these names — that is
the documented Phase 27 extension seam; tests construct the equivalent
pending xrefs directly.

### 18.5 Cache design

`InventoryCache` stores `[magic "SCRI"][version u32][timestamp i64][name len
u32][name][raw objects.inv]` under `<doctreedir>/__intersphinx_cache__` at
`blake3(target_uri).hex().inv`. Writes are atomic (temp + `sync_all` + rename).
Freshness mirrors upstream: `timestamp >= now - cache_limit_days * 86400`, with
a negative limit meaning "never expire". Corruption, truncation, version skew
and I/O errors are all cold-cache misses. `FetchOptions.now` is injectable so
expiry is tested deterministically. Any location containing `://` (including
`file://`) is cacheable; plain paths are always read from disk.

`fetch_all` uses `rayon` across projects; `fetch_all_async` offloads that work
to `tokio::task::spawn_blocking` for async callers. `reqwest` applies
`intersphinx_timeout` with retry and strips basic auth from target URIs.
`file://` and plain directory paths work fully offline. All failures collect
warning strings; a missing inventory never panics or fails a build.

### 18.6 Differential approach and tests

`tests/inventory.rs` (6 tests): oracle byte-exact writer, v2/v1 reader
fixtures, `$` expansion, embedded spaces, case-insensitive ambiguity, corrupt
input, cache envelope.

`tests/resolution.rs` (17 tests): local xref, `:any:`, file/directory and
`file://` inventories, fuzzy suggestions, nitpicky message stability,
`:external:` roles, disabled reftypes, cache expiry, malformed/network/
ambiguous-mapping no-panic, rayon multi-document resolution, the async wrapper
and the registry-attached seam.

`tests/intersphinx_differential.rs` (3 tests) follows the task's offline
fallback: the upstream `test-ext-intersphinx-role` / `test-domain-c-intersphinx`
/ `test-domain-cpp-intersphinx` roots require remote inventories, so a fixture
script writes the oracle's canonical `INVENTORY_V2` bytes locally and both
sides use the same file as `intersphinx_mapping['inv'][1]`. Real
`sphinx-build -b pseudoxml -E -n -q` output is reduced to resolution facts
(`refuri`, `reftitle`, `internal`, display text) and exact warning messages.
Normalizations: callable roles' `()` suffix is reproduced in the resolver,
the `external ` warning prefix is stripped where the oracle fixture uses
plain roles (the RST parser cannot parse `external+...` yet), and inner
`<inline classes="xref">` nodes carrying a replaced display name are not
counted as separate fallbacks. The C/C++ domain fixtures' local objects are
already covered by §17.6; this suite adds the intersphinx dimension.

### 18.7 Known limitations / deferred

* `:external:` roles are not parsed from RST text yet (Phase 27 extension
  bridge); tests annotate the frozen `pending_xref` shape directly.
* Inventory fetching does not follow HTTP redirects that rewrite the target
  URI, and does not honor `tls_cacerts` (rustls uses system roots).
* Self-referential `intersphinx_resolve_self` prefixes are stripped before
  lookup but do not re-run local domain resolution (upstream mutates the node
  and retries); documented as a low-impact divergence.
* Fuzzy suggestions are a scribe enhancement, not upstream behavior; they only
  alter the help label and are disabled via `XrefResolver::with_suggestions(false)`.
* `intersphinx_disabled_reftypes` defaults to `[]` here (upstream ships
  `['std:doc']`) so plain `:doc:` refs can resolve against inventories; set it
  explicitly in `conf.py` to match upstream defaults.
---

## 19. Phase 15 syntax highlighting & Pygments compatibility (`scribe-builders`)

### 19.1 Upstream sources inspected

Behaviour (node shapes, option specs, defaults, warning text) was read from the
pinned oracle rather than guessed:

* `sphinx/sphinx/highlighting.py` — `PygmentsBridge.get_lexer` alias handling
  (`py`/`python`/`py3`/`python3`/`default`, `pycon3`→`pycon`), the `none`/`python`/
  `pycon`/`rest`/`c` custom lexer table, `ClassNotFound` fallback to `TextLexer`
  with the `Pygments lexer name %r is not known` warning, and `raiseonerror`
  retry logic.
* `sphinx/sphinx/directives/code.py` — `Highlight`, `CodeBlock`,
  `LiteralIncludeReader`, `LiteralInclude`, `dedent_lines`, `container_wrapper`,
  `INVALID_OPTIONS_PAIR`, the filter pipeline (pyobject → start → end → lines →
  dedent → prepend → append) and every option conversion.
* `sphinx/sphinx/transforms/post_transforms/code.py` — `HighlightSetting`,
  `HighlightLanguageTransform` and `linenothreshold` semantics
  (`newline_count >= threshold - 1`).
* `sphinx/sphinx/util/_lines.py` — `parse_line_num_spec` (open ranges, ranges,
  comma lists, `ValueError` text).
* `sphinx/sphinx/pygments_styles.py` and Pygments 2.21 `get_style_defs` — CSS
  selectors and the `sphinx`/`friendly`/`none`/`default` style definitions.
* `sphinx/sphinx/writers/html5.py:605` — `visit_literal_block`: passes
  `linenos` (default `html_codeblock_linenos_style = 'inline'`) and
  `highlight_args` to `HtmlFormatter`, wraps in
  `<div class="highlight-{lang} notranslate">`, and appends Pygments' inner
  `<div class="highlight">`.
* Fixtures `test-directive-code`, `test-highlight_options`,
  `test-reST-code-block`, `test-reST-code-role` (source + real
  `sphinx-build -b html -E -q` output).

### 19.2 Public API

| Item | Purpose |
|---|---|
| `highlight_code(code, lang, &HighlightOptions) -> HighlightedCode` | Total, pure tokenizer. Unknown languages degrade to plain text with a warning; empty input, bad line specs and malformed UTF-8 never panic. |
| `highlight_many(&[(String, String, HighlightOptions)])` | `rayon`-parallel multi-block highlighting, input order preserved. |
| `HighlightedCode` | `language`, `css_classes`, `lines: Vec<HighlightLine>`, `warnings`, `options`. Serializes to/from deterministic JSON. |
| `HighlightLine` / `HighlightToken` | `tokens` + `line_number` + `line_classes` (`hll`, `gd`, `gi`, `gu`); token `text` + Pygments `classes`. |
| `HighlightOptions` | `force`, `linenos`, `lineno_start`, `emphasize_lines` (1-based, Pygments `hl_lines`), `lineno_match`. |
| `resolve_language` / `alias_info` / `LanguageResolution` | Alias table (`py`/`python`, `c++`/`cpp`, `js`/`javascript`, `rst`, `udiff`, …), syntect syntax name, known/unknown + warning. |
| `parse_line_num_spec(spec, total)` | `1,2,4-6` / `-10` / `10-` → zero-based indices; upstream-compatible error text. |
| `pygments_css(style) -> String` | Pure, deterministic stylesheet lookup. `available_styles()` lists the 16 embedded styles. An empty/unknown style falls back to `sphinx`. |
| `register_code_directives(&mut DirectiveRegistry) -> CodeDirectiveInfo` | Replaces the Phase 06 placeholders for `code`/`code-block`/`sourcecode`/`literalinclude` and adds `highlight`. |
| `code::highlighted_nodes(&Document)` | Reads every `HighlightedCode` payload from a resolved document (tests and text writers). |
| `HighlightError` | `thiserror` + `miette` diagnostics for payload decoding and config errors. |

### 19.3 Token model, storage choice and Phase 17 rendering contract

`HighlightedCode` is stored as a **single deterministic JSON string** in the
`__highlight_json` attribute of the owning `NodeKind::LiteralBlock`. This choice
was made over a side table because `Document` is `serde`-serialized for the
incremental cache (§12): attribute storage survives `to_json()`/`from_json()`
round-trips and arena-id tombstones without any extra plumbing. Ordinary
Docutils attributes (`language`, `force`, `linenos`, `xml:space`, `classes`,
`ids`, `names`, `source`, plus `hl_lines`/`linenostart` for writer convenience)
are populated alongside it. The frozen AST (`NodeKind`, `Document`,
`BuildEnvironment`, `Domain`) was **not modified**.

Phase 17 can render a block with:

1. read `HighlightedCode::from_node(&node)`,
2. emit `<div class="{css_classes joined}">` when `css_classes` is non-empty,
   then `<div class="highlight"><pre><span></span>` (the empty leading span is
   Pygments' own output),
3. for each `HighlightLine`: if `line_number` is `Some`, emit
   `<span class="linenos">{n:>width}</span>` (width = digits of the largest
   number); if `line_classes` is non-empty, open a `<span class="…">` wrapper;
   emit each `HighlightToken` as escaped text (bare, or wrapped in
   `<span class="{classes joined}">`); append `\n`; close the line wrapper,
4. close `</pre></div>` and the optional outer `</div>`.

`HighlightedCode::pygments_html()` implements exactly this and is used by the
differential tests; Phase 17 may reuse it or reimplement the same structure.

### 19.4 Scope → Pygments class mapping and CSS generation

`syntect`'s default Sublime syntax set and pre-parsed scope selectors are
cached in `once_cell::sync::Lazy` statics. Sublime scopes are matched
most-specific-first (`string.quoted.double` before `string`) and mapped to
Pygments classes: `.k/.kd/.kn/.kp/.kr/.kt`, `.s/.s1/.s2/.sc/.si/.se/.sr`,
`.c/.c1/.cm/.cp`, `.nf`, `.nc`, `.nb/.bp`, `.n/.nv`, `.o/.ow`, `.p`,
`.m/.mi/.mf`, `.kc`, `.err`, and diff `.gd/.gi/.gu/.gh`. Unscoped non-whitespace
text is emitted as `.n` to match Pygments' `Name` token; whitespace-only text
is left unclassed. Adjacent equal-class tokens are coalesced. Function-call
targets (`meta.function-call` + `entity.name.function`) are emitted as `.n`,
matching Pygments' Python lexer rather than Sublime's `.nf`.

`pygments_css()` embeds the exact `pygments.css` text produced by the pinned
Pygments 2.21 / Sphinx 9.3 `HtmlFormatter.get_style_defs('.highlight')` for 16
styles (files under `crates/scribe-builders/src/css/`). Generation at runtime
requires no Python, filesystem or clock, so output is byte-for-byte
deterministic.

### 19.5 Directives and per-document context

* **Options**: `code-block`/`sourcecode`/`code` accept `force`, `linenos`,
  `dedent`, `lineno-start`, `emphasize-lines`, `caption`, `class`, `name`.
  `literalinclude` additionally accepts `lineno-match`, `tab-width`, `language`,
  `encoding`, `pyobject`, `lines`, `start-after`, `end-before`, `start-at`,
  `end-at`, `prepend`, `append`, `diff`. Invalid option pairs and missing files
  become miette `DirectiveError::Failed` diagnostics; out-of-range emphasis and
  dedent warnings are recorded in `HighlightedCode.warnings` and the block is
  still produced.
* **`highlight` directive** emits a `Highlightlang` node (`lang`, `force`,
  `linenothreshold`) and writes `__highlight_lang` / `__highlight_force` /
  `__highlight_linenothreshold` onto the document root. Because
  `resolve_pending` walks the arena in document order, subsequent code blocks
  observe the setting — the native equivalent of Sphinx's
  `env.current_document.highlight_language` plus the post-transform.
  `linenothreshold` (default `i64::MAX`) enables line numbers when the block has
  at least that many lines.
* **Captions** wrap the literal in a `Container` (`literal-block-wrapper`) with
  a `Caption` child parsed through the Phase 05 inline parser.
* **Diff** reads both files, produces a unified diff and classifies `+`/`-`/`@@`
  lines as `.gi`/`.gd`/`.gu` (with `.w` for the context marker column).
* **Phase 04 remediation**: `explicit.rs` no longer promotes body lines into a
  directive argument when a blank line follows the option block. Without this,
  `.. code-block::` with `:dedent:`/`:class:` and no language silently moved the
  first code lines into the argument slot (reproduced by
  `test-directive-code/dedent.rst`).

### 19.6 Differential method (`tests/oracle_differential.rs`)

Five tests cover four real upstream fixtures. For each fixture the oracle runs
`sphinx-build -b html -E -q` and the generated highlight blocks are reduced to
a **per-character token-class stream**: HTML tags are stripped, entities
decoded, line-number text discarded, classes normalised to a canonical
vocabulary (`.k` for every keyword flavour, `.s` for strings, `.c` for comments,
`.n` for names, …), then compressed into `(class, text)` runs. The same reducer
is applied to `HighlightedCode::pygments_html()` after parsing the identical
fixture source through scribe. This compares exact source text (including blank
lines and indentation) *and* every token class while tolerating harmless
differences in how syntect and Pygments split adjacent spans. A class that
cannot be mapped is reported as `other:<name>` and fails the test, so new token
kinds cannot slip through.

| Test | Fixture | Scope |
|---|---|---|
| `differential_test_rest_code_block` | `test-reST-code-block` | 1 block, inline linenos — exact stream. |
| `differential_test_highlight_options` | `test-highlight_options` | 3 blocks (`default`/`python`/`java`) — exact stream. |
| `differential_test_directive_code_index` | `test-directive-code/index.rst` | code-block + literalinclude — exact stream. |
| `differential_test_directive_code_options` | `test-directive-code` | `:linenos:`, `:lineno-start:`, `:lines:` + `:lineno-match:` — exact stream. |
| `differential_test_rest_code_role_code_block` | `test-reST-code-role` | text + agreed classes (tracked gap below). |

Unit/integration coverage adds per-token-class, alias, line-number, emphasis,
dedent-warning, include-range, diff, caption, CSS-determinism and no-panic
tests (`tests/highlight_api.rs`, `tests/code_directives.rs`).

### 19.7 Tracked divergences & known limitations

* **Invalid Python in call signatures** (`test-reST-code-role`): syntect does
  not sub-tokenize numbers/operators/strings inside `def foo(1 + 2 + None + "x")`
  parameter lists, so those spans arrive as `.n`; Pygments emits `.m`/`.o`/`.kc`/
  `.s2`. The differential test compares exact text plus agreed classes only.
* **Whitespace spans**: Pygments' Ruby/diff lexers emit `.w` spans for leading
  whitespace; syntect leaves it unscoped. Text is identical; only the (unstyled)
  `.w` wrapper differs.
* **`storage.type.function` → `.k`** for Python `def` (Pygments emits `.k`);
  a hard-coded alias keeps the streams aligned. Other grammars may map a
  declaration to `.kd` where Pygments uses `.k`.
* **`pycon` console lexing**: mapped to the Python grammar; prompt-specific
  `.gp` tokens are not reproduced.
* **`:encoding:`** is validated and accepted, but file decoding uses the
  injected `IncludeResolver` (UTF-8); arbitrary codecs are not re-decoded.
* **`:pyobject:`** uses a lightweight indentation-aware tag finder covering
  classes, methods and decorators; it does not run Python's tokenizer for
  multi-line signatures or conditional definitions.
* **Unified diff** uses an LCS implementation with three context lines. It
  matches the oracle fixtures but can choose different hunk boundaries than
  Python's Ratcliff/Obershelp `difflib` on unrelated inputs.
* **`:lineno-match:` star marker**: the pinned oracle only adjusts
  `lineno_start`; no `*` is rendered in HTML. The flag is retained in
  `HighlightOptions` as metadata for Phase 17.
* **Extra node attributes** `hl_lines` / `linenostart` and `__highlight_json`
  appear in pseudo-XML for code blocks; no frozen-AST differential test covers
  them, and the JSON is required for cache round-trips.
* **Inline code roles** (`:code:`, custom code roles) remain Phase 05/27 scope;
  only their containing `code-block` is compared here.
---

## 20. Phase 16 Jinja2 templating & theme system (`scribe-theme`)

Phase 16 provides the **rendering context and template API** the Phase 17 HTML
writer consumes; it does not itself emit complete pages from doctrees. The
frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` shapes are untouched.

### 20.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/theming.py` — `Theme`, `HTMLThemeFactory`, `_find_themes`,
  `_load_theme_with_ancestors` (10-ancestor cap, cycle detection),
  `_load_theme_toml` / `_validate_theme_toml` / `_convert_theme_toml`,
  `_load_theme_conf` / `_convert_theme_conf` (legacy INI), `get_options`.
* `sphinx/sphinx/jinja2glue.py` — `SandboxedEnvironment`, `_tobool`, `_toint`,
  `_todim`, `_slice_index`, `accesskey`, `idgen`, `warning`, `debug`,
  `SphinxFileSystemLoader` (the `.jinja` → `_t` legacy-suffix fallback and the
  `!`-prefix skip of user template paths).
* `sphinx/sphinx/builders/html/__init__.py` — `globalcontext` (§518) and the
  closures installed in `handle_page`: `pathto`, `hasdoc`, `css_tag`, `js_tag`;
  `get_doc_context` (`parents`/`prev`/`next`/`rellinks`/`toc`/`display_toc`).
* `sphinx/sphinx/util/osutil.py::relative_uri` — exact `/`-separated algorithm.
* `sphinx/sphinx/themes/{basic,classic,nature}/theme.toml` and their
  `layout.html` / `page.html` / sidebar templates.
* `.oracle-venv/.../alabaster/{theme.conf,layout.html,about.html,
  navigation.html,relations.html,donate.html}` and `static/alabaster.css_t`.

### 20.2 Crate layout and dependency direction

```
scribe-theme ──► scribe-config (Config, html_theme*, templates_path, html_context)
     ├──► minijinja 2.x (features: builtins, loader, json, urlencode)
     ├──► toml, serde_json, thiserror, miette, indexmap, once_cell,
     │    walkdir, dashmap
     └──► themes/{basic,classic,nature,alabaster}/  (vendored assets)
```

`scribe-theme` is a leaf with respect to the other scribe crates except
`scribe-config`. No AST / environment / domain types are imported or changed.

### 20.3 Public API

| Item | Purpose |
|---|---|
| `ThemeManager::new()` | Manager with the four bundled themes registered. |
| `ThemeManager::with_config(&Config, Option<&Path>)` | Adds `html_theme_path` / `templates_path` and seeds `html_theme_options`. |
| `ThemeManager::add_theme_path(&Path)` / `add_template_dir(PathBuf)` | Discover user themes / prepend user template dirs. |
| `ThemeManager::get_theme(name)` / `theme_from_config(&Config)` | Resolve a theme; returns the flattened `Theme`. |
| `ThemeManager::render_template(name, &RenderContext)` | Render with the theme named by `ctx.theme_name`. |
| `ThemeManager::environment(&Theme)` | Cached (`DashMap`) compiled environment. |
| `TemplateEnvironment::new(&Theme)` / `with_search_paths(&Theme, Vec<PathBuf>)` | Build a sandboxed environment. |
| `TemplateEnvironment::register_sphinx_globals()` | Register all globals/filters (idempotent). |
| `TemplateEnvironment::render(name, &RenderContext)` / `render_string(name, src, ctx)` | Render one template / inline expression. |
| `theme_static_files(&Theme) -> Vec<PathBuf>` | Deterministic static-asset list for Phase 17 copying. |
| `RenderContext` | Per-page data (see §20.5). |
| `ThemeError` / `ThemeResult<T>` | `thiserror` + `miette::Diagnostic` errors with stable `scribe::theme::*` codes. |

Modern `theme.toml` and legacy `theme.conf` are both supported. `[options]`
values are stringified (bool → `true`/`false`, int → decimal) so the CSS
templates' `|tobool` / `|todim` filters behave as upstream. Inheritance is
resolved child-first (matching `get_theme_dirs`), then flattened root-first so
the nearest ancestor wins for `stylesheets`, `sidebars`, `pygments_style` and
every option; `html_theme_options` are applied last. Cycles, >10 ancestors,
missing parents and malformed TOML/INI are typed errors — never panics.

### 20.4 Bundled themes and attribution

`crates/scribe-theme/themes/` vendors `basic`, `classic`, `nature` (BSD-2-Clause,
© the Sphinx team) and `alabaster` 1.0.0 (BSD, © Jeff Forcier and contributors),
including templates, stylesheets, scripts and metadata. Provenance and the
list of adaptations are recorded in `crates/scribe-theme/themes/NOTICE`.
Template files are byte-identical to upstream; only the loader preprocessing
below changes how they are parsed.

### 20.5 Global / filter inventory

Globals: `pathto`, `hasdoc`, `css_tag`, `js_tag`, `sidebar`, `relbar`, `toctree`,
`get_theme_option`, `accesskey`, `idgen`, `warning`, `debug`, `_`, `gettext`,
`translation`, `ngettext`, `tobool`, `toint`, `todim`, `slice_index`.

Filters: Sphinx-specific `tobool`, `toint`, `todim`, `slice_index`, `striptags`;
Jinja/MarkupSafe `e`/`escape` (`/` deliberately **not** escaped); built-ins
`safe`, `attr`, `lower`, `slice`, `urlencode`, `tojson`, `pprint`.

`pathto` implements upstream's `relative_uri` exactly, including the `#`
current-page special case, resource URI pass-through for `://` and the
`urlquote(docname) + link_suffix` fallback. `accesskey` is stateful per render
via `State::get_or_set_temp_object`; `idgen` is an object with `current()` /
`next()` / `__next__`. The scalar helpers are both filters and globals.

### 20.6 Phase 17 integration contract

Phase 17 constructs `RenderContext` **before** rendering:

1. `let mut ctx = RenderContext::for_builder("html", docname, config.clone())`
   (or `RenderContext::new(docname)` for default config). `theme_name` is taken
   from `config.html_theme`; override with `.with_theme(name)`.
2. Fill `all_docs` (from `env.all_docs`), optional `target_uris` (builder-specific
   link suffixes), `theme_options` (merged `Theme::options`), and the per-page
   data `title`, `body`, `toc`, `metatags`, `sidebar_html`, `relbar_html`,
   `toctree_html`, `rellinks`, `parents`, `prev`, `next`, `css_files` /
   `script_files` (use `RenderContext::css_file` / `js_file` for tag objects).
   `body`, `toc` and the three HTML fragments are inserted as **safe** values;
   everything else is escaped with MarkupSafe rules.
3. `manager.render_template("page.html", &ctx)` — the manager picks the theme
   from `ctx.theme_name` and returns the final HTML string. For a standalone
   asset call `manager.environment(&theme)?.render("basic.css.jinja", &ctx)`;
   `theme_static_files(&theme)` lists the files to copy into `_static/`.
4. Any extra per-page template variables go through `ctx.set_var(name, value)`,
   which has highest precedence (e.g. to override `sidebars`).

`html_context` from `Config` is merged before `ctx.vars`; `theme_<option>`
variables and `get_theme_option(name, default)` both read the same option map.

### 20.7 Template preprocessing and autoescaping

minijinja lacks the Jinja2 `i18n` extension and rejects `{% block %}` inside
`{% macro %}`. `preprocess_template` therefore performs two source rewrites
before compilation:

* `strip_macro_blocks` unwraps `{% block … %}` / `{% endblock %}` tags inside
  macros (Sphinx's `basic/layout.html` uses them for `rootrellink`,
  `relbaritems`, `sidebarlogo`, …). Unwrapping is behaviorally identical for
  bundled themes, none of which override those inner blocks.
* `preprocess_trans` rewrites `{% trans var=expr, … %}message{% endtrans %}` into
  a one-iteration `{% for %}` scope with `{% set %}` assignments and the inline
  message; `trimmed` joins non-empty trimmed lines with spaces. English output
  is unchanged.

Autoescaping is enabled for `.html`/`.htm`/`.xml` (and `.html.jinja`), disabled
for `.js.jinja`, `.css.jinja`, `_t` and plain text. A custom formatter supplies
MarkupSafe escaping (no `/` escaping, `"` → `&#34;`, `'` → `&#39;`) so explicit
`|e` and autoescaping agree with Jinja2. The loader maps `name.jinja` to the
legacy `name_t` file (alabaster's `alabaster.css_t`) and searches theme dirs
before their parent dirs, so `alabaster/layout.html` can
`{% extends "basic/layout.html" %}` while still overriding `relations.html`.
The environment is a minijinja sandbox: no arbitrary attribute mutation or
imports are available to templates.

### 20.8 Differential method and normalization

`tests/differential.rs` runs `.oracle-venv/bin/python` with a script that
re-implements the exact upstream helper bodies (`relative_uri`, `pathto`,
`_tobool`, `_toint`, `_todim`, `accesskey`, `idgen`, `striptags`) on top of the
oracle's Jinja2 3.1.6. Both engines render the same 13 expressions (paths,
booleans, dimensions, tags, id generation, access keys, arithmetic, filters).
Output is normalized by trimming and lowercasing Python's `True`/`False` only;
all other characters are compared byte-for-byte. The oracle script is inline,
process-unique state is avoided, and a failure reports the offending expression.

### 20.9 Tests and known limitations

`tests/theme.rs` (10), `tests/globals.rs` (10), `tests/render.rs` (5) and
`tests/differential.rs` (2), plus 7 unit tests in `src/` and 1 doctest.
Render tests assert that `basic/page.html`, `alabaster/layout.html` (via
`page.html`), `classic` and `nature` render with no missing filter/global and
with structural markers (`<!DOCTYPE html>`, `<head>`, `rel="stylesheet"`,
sidebar blocks, `Powered by`). CSS templates render without HTML escaping.

Limitations / seams for Phase 17:

* `sidebar()`, `relbar()` and `toctree()` return **pre-rendered** safe fragments;
  Phase 17 must build them (the translator owns sidebar/toctree markup).
* `css_tag` / `js_tag` accept string entries or `{filename, body, attributes}`
  maps; query-string cache busting (`?v=<checksum>`) is not computed in Phase 16.
* Built-in `gettext` translation tables are empty by default; `RenderContext`
  carries a message-id → translation map for Phase 26 to populate.
* `warning()` currently renders nothing without emitting a log record; hooking
  it into scribe's diagnostic sink is a Phase 17/27 concern.
* The `!` template-prefix escape of `SphinxFileSystemLoader` is not implemented;
  none of the vendored templates use it.
## 21. Phase 17 HTML5 writer & standalone HTML builder (`scribe-builders`)

Phase 17 turns the frozen doctree into complete Sphinx-compatible HTML pages.
It is the first *writer* phase: it consumes every seam delivered by Phases 10
(transforms), 11–14 (domains / xrefs / inventory), 15 (highlight payload) and
16 (themes / templates). Nothing in the frozen §7 AST, §12 `BuildEnvironment`
or §15 `Domain` shapes was modified.

### 21.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/writers/html5.py` — all 69 `visit_*`/`depart_*` visitor pairs,
  in particular `visit_section` / `visit_title` (`<hN>` + `headerlink`),
  `visit_literal_block`, `visit_bullet_list`, `visit_reference`, `visit_image`,
  `visit_footnote*`, `visit_math*`, `add_secnumber` / `add_fignumber`.
* `docutils/writers/_html_base.py` — `starttag()` attribute ordering,
  `is_compactable` / `SimpleListChecker` (the `simple` list class),
  `visit_paragraph` (always emits `<p>` in the HTML5 writer), `visit_list_item`.
* `sphinx/sphinx/builders/html/__init__.py` / `_assets.py` — `handle_page`
  closures (`pathto`, `css_tag`, `js_tag`), `get_doc_context`, the ordered
  `css_files`/`script_files` lists, `objects.inv` writing and `.buildinfo`.
* `sphinx/sphinx/themes/basic/{layout,page,localtoc,sourcelink,searchbox}.html`
  — the exact skeleton the Phase 16 templates already vendor byte-identically.
* Fixtures `sphinx/tests/roots/test-basic` and `test-root` — oracle builds run
  with `.oracle-venv/bin/sphinx-build -b html -E -q`.

### 21.2 Crate layout and dependency direction

```
scribe-builders ──► scribe-ast (frozen), scribe-config, scribe-rst
     ├──► scribe-core   (BuildEnvironment, transform pipeline, UpdateOptions)
     ├──► scribe-domains (DomainRegistry, xrefs, objects.inv writer)
     ├──► scribe-theme   (ThemeManager, RenderContext, pathto/css_tag/js_tag)
     ├──► scribe-cli?    NO — scribe-cli depends on this crate (no cycle)
     └──► rayon (parallel page rendering / asset copy), minijinja, blake3

scribe-cli ──► scribe-builders (CLI wiring only)
```

New modules under `crates/scribe-builders/src/html/`: `mod.rs`, `translator.rs`,
`builder.rs`, `assets.rs`, `index.rs`. Errors live in the crate's `error.rs`
(`HtmlError`, thiserror + miette `scribe::builders::html::*` codes).

### 21.3 Public API

| Item | Purpose |
|---|---|
| `HTML5Translator::translate(&Document, &HtmlContext) -> String` | Render a document body fragment. |
| `HtmlContext` | Owned per-page data: `docname`, `config`, cloned `env`, Phase 16 `theme_ctx`, `target_uris`, `all_docs`, permalink settings, `image_uris`, `language`. Provides `relative_uri`, `target_uri`, `resolve_refuri`. |
| `render_local_toc(&Document, maxdepth) -> String` | Sidebar local-TOC fragment. |
| `escape_html` / `escape_attr` | MarkupSafe-compatible escaping helpers. |
| `BuildOptions` (`BuilderOptions`) | Clap-independent builder inputs: source/output dirs, `force_all`, `freshenv`, `doctree_dir`, `filenames`, `worker_count`, `really_quiet`. The CLI converts `scribe_cli::BuildOptions` into this to avoid a dependency cycle. |
| `BuildResult` (`BuilderResult`) | `written` files, `built_docnames`, `warnings`. |
| `StandaloneHtmlBuilder::build(&BuildOptions, &Config, &mut BuildEnvironment) -> BuildResult` | Run the full build (associated function, as specified). |
| `generate_genindex` / `generate_modindex` / `generate_search_page` | Index/search page generators. |
| `copy_theme_assets` / `render_asset_templates` / `write_source_dumps` / `write_buildinfo` / `copy_images` | Deterministic asset helpers. |

The CLI wires `scribe build -b html SOURCE OUT` to the builder; unknown builders
still produce the existing `scribe::unimplemented_builder` miette diagnostic.
Missing source directories and template/IO failures are typed `HtmlError`s
wrapped in `scribe::builder::failed` at the CLI boundary — never panics.

### 21.4 Build pipeline

1. `BuildEnvironment::update` (Phase 08) discovers sources and computes the
   rebuild set; `-E` discards the cache, `-d` selects the doctree directory,
   `-a` forces every doc, `-j` is passed to rayon's worker pool.
2. Pass A parses every source with `scribe-rst` + the Phase 15 directive
   registry, resolves directives/substitutions, and registers each document's
   domain objects **before** any xref is resolved.
3. Pass B applies `Phase::Read` then `Phase::Resolved` transforms with the
   domain registry and environment attached (Phase 14 resolution), then renders
   each page in parallel: translator body → Phase 16 `page.html` wrapper.
4. Outputs are collected, sorted by docname and written sequentially, so bytes
   are deterministic regardless of worker count.

### 21.5 Node → HTML mapping (representative)

| Node(s) | HTML |
|---|---|
| `document` | no wrapper (theme supplies `<div class="body" role="main">`) |
| `section` / `title` | `<section id="…">…<hN>title<a class="headerlink" href="#…" title="Link to this heading">¶</a></hN>` (`hN` from nesting depth, clamped to 1–6) |
| `paragraph` / `text` | `<p>…</p>` / escaped text (text inside `literal` is wrapped in `<span class="pre">`) |
| `emphasis` / `strong` / `literal` / `inline` | `<em>` / `<strong>` / `<code class="docutils literal notranslate">` / `<span class="…">` |
| `reference` | `<a class="reference internal|external …" href="…">`, links via `pathto`-style relative URIs |
| `pending_xref` | resolved to `reference` by Phase 14; unresolved degrades to its content |
| `literal_block` | Phase 15 payload → `<div class="highlight-{lang} notranslate"><div class="highlight"><pre><span></span>…`; plain blocks get a `highlight-none` shell |
| `bullet_list` / `enumerated_list` | `<ul>` / `<ol>` with `class="simple"` when Docutils' SimpleListChecker would compact them |
| `definition_list` / `field_list` / `option_list` | `<dl>` / `<dt>` / `<dd>`, `<dl class="option-list">` |
| `table` / `tgroup` | `<table class="docutils align-default">`, `<colgroup>`, `<thead>`/`<tbody>`, `<tr class="row-odd|row-even">`, `<td>` with `rowspan`/`colspan` |
| admonitions | `<div class="admonition note"><p class="admonition-title">Note</p>…` (generic `Admonition` reads its `title` child) |
| `versionmodified` | inline `<span class="versionmodified …">` (no admonition wrapper), matching upstream |
| `toctree` | nested `<ul>`/`<li>` with caption, doc/external entries and current-page marker |
| `image` / `figure` | `<img class="align-…" src="_images/…">` / `<figure>…<figcaption>` |
| `footnote` / `citation` / references | `<aside class="footnote|citation" id="…">` + backlinked `<a class="reference internal footnote-reference" href="#…">` |
| `math` / `math_block` | `\(…\)` / `\[…\]` (MathJax inline delimiters; no server-side rendering) |
| `raw` | passthrough only for `format=html`/`html5`/`html4`; other formats dropped |
| `desc*` | `<dl class="sig sig-object"><dt class="sig sig-object">…` with `sig-param`/`o`/`p`/`k` spans |
| `target` / `comment` / `system_message` | `<span class="target" id="…">` / dropped / `<div class="system-message">` |

### 21.6 Determinism and cache busting

`?v=` values are deterministic BLAKE3 prefixes derived from the asset name and
a digest of the resolved configuration; they are stable across runs and worker
counts. Static assets, source dumps and doctree files are copied/written in
sorted order. No timestamps are embedded in generated content.

### 21.7 Differential method and normalization

`tests/html_differential.rs` does the following:

* **test-basic body equality.** Both engines build the fixture; the
  `<div class="body" role="main">…` fragment is extracted and normalized by
  (a) replacing `?v=<hash>` with `?v=HASH`, (b) un-doubling Phase 16's
  `&amp;#…` escaping, and (c) collapsing whitespace runs to single spaces.
  The normalized fragments are asserted byte-equal.
* **test-root structural invariants.** A curated marker set is asserted in
  every generated page from both engines: DOCTYPE, document/body wrappers,
  section ids, `headerlink`, `simple` lists, admonitions and highlight blocks;
  the set of generated HTML page names must match. No full byte comparison is
  attempted (see divergences).
* **Link integrity.** Every internal `href` in a generated page resolves to a
  written file (after stripping `?v=`) or to an id present in that page.
* **Robustness.** Empty sources, missing sources and unresolved xrefs build
  without panics; missing sources return `HtmlError::MissingSource`.

### 21.8 Intentional divergences (relative to the oracle)

* **Phase 16 theme shell.** minijinja does not preserve Jinja2 `Markup`
  through `" &#8212; "|safe + docstitle|e`, so title separators appear as
  `&amp;#8212;` / `&amp;#187;` instead of `&#8212;` / `&#187;`. The second
  viewport `<meta>` Sphinx injects is absent, and the version string is
  `9.3.0` rather than `9.3.0+/035a07c32`.
* **Parser-level (earlier phases) differences on complex fixtures.** scribe's
  parser does not emit Sphinx's `toc-backref` anchors, section numbers
  (`<span class="section-number">`), or `backrefs` on footnotes; some block
  constructs (nested line blocks, epigraph `{title, attribution}` subtrees,
  field-list document metadata such as `:tocdepth:`) lower to a flatter AST.
  The translator renders the AST faithfully; those differences are owned by
  earlier phases and are deliberately excluded from the test-root byte
  comparison.
* **`!layout.html` user-template escape.** `SphinxFileSystemLoader`'s leading
  `!` prefix is not implemented in Phase 16; fixtures using it fall back to the
  bundled-only theme manager instead of failing the build.
* **Search index.** `search.html` is a structural placeholder in Phase 17;
  `searchindex.js` and stemmer assets arrive with the Phase 18 indexer.
* **Math rendering.** MathJax delimiters are emitted directly; there is no
  server-side math-to-SVG/PNG pipeline.

### 21.9 Tests

`tests/html_translator.rs` (19 unit tests, one per node family) and
`tests/html_differential.rs` (6 differential/integration tests). Workspace
total: **665 tests** (661 library/integration + 4 doctests).
---

## 22. Phase 18 client-side search indexer (`scribe-builders`)

Phase 18 turns the Phase 17 placeholder into a working Sphinx-compatible
client-side search: a `searchindex.js` payload, language_data/stemmer assets
and a functional `search.html`. Nothing in the frozen §7 AST, §12
`BuildEnvironment` or §15 `Domain` public shapes changed.

### 22.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/search/__init__.py` — `IndexBuilder.feed`, `freeze`,
  `get_objects`, `get_terms`, `WordCollector` / `_feed_visit_nodes`,
  `_JavaScriptIndex` (`PREFIX = "Search.setIndex("`, `SUFFIX = ")"`).
* `sphinx/sphinx/search/en.py` — `SearchEnglish` (Snowball English stemmer,
  English stop-word set).
* `sphinx/sphinx/search/_stopwords/en.txt` and `_stopwords/en.py` — the
  174-word Snowball English stop list vendored with attribution in
  `search.rs::ENGLISH_STOPWORDS`.
* `sphinx/sphinx/builders/html/__init__.py` — `IndexBuilder` wiring in
  `handle_page`/`dump_search_index`, `copy_stemmer_js`.
* `sphinx/themes/basic/{search.html,static/language_data.js.jinja,
  static/searchtools.js}` — the exact client contract (`Search.setIndex`,
  `Search.init`, `_parseQuery`, `performTermsSearch`, `makeSearchSummary`).
* Vendored `crates/scribe-theme/themes/basic/static/searchtools.js`
  (Phase 16), byte-identical to the oracle's.

### 22.2 Crate layout and dependency direction

New module `crates/scribe-builders/src/search.rs`, re-exported from
`scribe_builders::lib.rs`. Vendored minified stemmers live in
`crates/scribe-builders/src/html/js/{base,english}-stemmer.js` with a
`README.md` attribution note; they are `include_str!`-embedded into
`language_data.js` and copied verbatim into `_static/`.

New workspace dependencies (declared in `[workspace.dependencies]` first):
`rust-stemmers = "1.2"` (Snowball algorithms in Rust). `regex` was already in
the workspace and is now consumed by `scribe-builders` for raw-HTML tag
stripping. No other crates, no cycle (`scribe-cli` still only wires the
builder).

### 22.3 Public search API

| Item | Purpose |
|---|---|
| `SearchIndex::new(language, envversion)` | Create an empty, ordered index. |
| `feed(docname, title, &Document)` / `feed_with_filename(docname, filename, title, &Document)` | Extract words/titles from a frozen doctree; mirror upstream's title/body de-duplication. |
| `add_object(&ObjectEntry)` / `add_object_for_domain(domain, type_name, &ObjectEntry)` / `add_domain_objects(&DomainRegistry)` | Populate `objects` / `objtypes` / `objnames` with upstream's prefix/anchor rules. |
| `add_index_entry(docname, entry, id, is_main)` | Add an explicit `.. index::` entry (mirrors `IndexBuilder.feed`). |
| `to_json_value() -> serde_json::Value` | The 11-key schema as ordered JSON. |
| `to_js() -> String` | `Search.setIndex({...})` — compact, sorted-key bytes. |
| `len`, `is_empty`, `language` | Introspection. |
| `tokenize`, `stem_word`, `algorithm_for`, `is_filtered`, `stopwords_for` | Tokenizer/stemmer/stop-word primitives. |
| `scribe_envversion()` | Deterministic scribe-specific version map. |
| `search_language_stemming_code(language)`, `search_language_stop_words_json(language)` | Values injected into `language_data.js.jinja`. |
| `parse_js_index(source)` | Test/tooling helper that parses a `Search.setIndex(...)` payload. |
| Constants `ENGLISH_STOPWORDS`, `FULLY_SUPPORTED_LANGUAGES`, `STEMMABLE_LANGUAGES`, `BASE_STEMMER_JS`, `ENGLISH_STEMMER_JS`, `IDENTITY_STEMMER_JS`, `JS_INDEX_PREFIX`, `JS_INDEX_SUFFIX` | Vendored data and format markers. |

### 22.4 Index schema and escaping

The emitted object has exactly Sphinx's 11 keys (verified against the oracle):

```
alltitles docnames envversion filenames indexentries objects
objnames objtypes terms titles titleterms
```

* `terms` / `titleterms` map a stemmed token to a single integer doc index
  (when unique) or a sorted list of indices (upstream `get_terms`).
* `alltitles` maps each title to `[[doc_index, anchor_or_null], ...]`; the
  first title encountered is the main document title and gets `null`.
* `objects` groups by dotted prefix; each row is
  `[doc_index, typeindex, priority, shortanchor, name]`, with `shortanchor`
  `""` when the anchor equals the full name, `"-"` for
  `<type>-<fullname>`, else the raw anchor (upstream `get_objects`).
* `indexentries` maps a lower-cased entry to
  `[[doc_index, entry_id, is_main], ...]`.
* JSON is serialized by `serde_json` with all maps `BTreeMap`-ordered, so
  emitted bytes are reproducible (equivalent to Python's
  `json.dumps(..., sort_keys=True, separators=(',', ':'))`).
* **No HTML escaping** is applied to index JSON (it is a JS file), except
  inside object names where upstream itself calls `html.escape` — scribe
  reproduces that detail in `search.rs::html_escape`.

### 22.5 Build integration

`StandaloneHtmlBuilder::build` calls `write_search_index` after `index.rs`
generation. It feeds every built document from its post-transform doctree
(matching upstream's `index_page` call site), uses the real source suffix for
`filenames`, adds all domain objects, writes `searchindex.js`, and copies the
two minified stemmer assets. `render_asset_templates` now injects
`search_language_stop_words` and `search_language_stemming_code` into
`language_data.js.jinja`, so the page defines `window.Stemmer` and the
stop-word set. `generate_search_page` renders the bundled byte-faithful
`search.html` (which loads `searchtools.js`, `language_data.js`, defers
`searchindex.js` and provides `#search-results`) instead of the Phase 17
placeholder. `searchtools.js` calls `_ready(Search.init)` itself.

### 22.6 Language support and graceful degradation

* **Full support (Snowball stemmer + vendored stop words):** English (`en`).
* **Stemming only (Snowball algorithm in `rust-stemmers`, no stop-word list):**
    `ar`, `da`, `de`, `el`, `es`, `fi`, `fr`, `hu`, `it`, `nl`, `no`, `pt`,
    `ro`, `ru`, `sv`, `ta`, `tr` (plus `nb`/`nn` → Norwegian).
* **Graceful degradation (identity stemmer, no stop words):** every other
    configured language, notably `ja` and `zh`. Upstream serves the English
    stemmer JS to `zh`; scribe does the same in `language_data.js`.
* Regional codes (`en_US`, `pt_BR`, …) fall back to the base language code.

Raw HTML nodes are indexed only when their `format` includes `html`, after
stripping `<style>`, `<script>` and all remaining tags (upstream's
`WordCollector` behavior). Comments, nodes with a `no-search` class and
non-HTML raw nodes are skipped. Math nodes carry their text in the frozen AST
and are indexed as ordinary text (upstream also indexes math text through its
text nodes); there is no special LaTeX/Unicode normalization.

### 22.7 Divergences (relative to the oracle)

* **`envversion` values.** Upstream uses a Sphinx module→integer map
  (`sphinx: 66`, …); scribe uses `{scribe: 1, scribe.builders: 1,
  scribe.domains: 1, scribe.search: 1}`. The shape is structurally compatible
  and the JSON contract only requires stability across builds: stock
  `searchtools.js` never inspects it.
* **`test-basic` is byte-identical** for all fields except `envversion`
  (verified by the differential test).
* **`test-root` normalizations.** scribe's earlier-phase RST parser produces
  slightly different document titles on four files (`bom`: keeps the UTF-8
  BOM; `index`: straight vs smart quotes; `markup`: `.. title::` not applied;
  `otherext`: bare generated section has no title). Section anchor slugs and
  some body-token sets also differ. The differential test therefore compares
  normalized `alltitles` (title → doc-index mapping, anchors ignored) and a
  sample of term *presence*, asserting exact weights only where the doc-index
  sets happen to agree. These are owned by the parser phases, not the indexer.
* **JS index terminator.** The pinned oracle emits `Search.setIndex({...})`
  with no trailing `;`; scribe matches those bytes exactly.
* **Preview snippets** are fetched from the built page by `searchtools.js`
  at query time; they are not stored in the index, so headless tests validate
  the index data/query contract plus snippet *generation* from matching text,
  not real browser DOM behavior.

### 22.8 Tests

`crates/scribe-builders/tests/search_index.rs` adds **15 tests**:

* Unit: tokenizer (Unicode/punctuation/camelCase), Snowball stemming, stop
  words, title-vs-body weighting/deduplication, deterministic byte output,
  escaping (quotes/backslashes/Unicode, no HTML escaping), unsupported
  language fallback, empty/non-ASCII documents.
* Query simulation: a Rust port of `_parseQuery` + `performTermsSearch`
  asserts known-term → correct doc/title, `running` matches `run`, stop-word
  exclusion and title-score precedence (15 vs 5), plus snippet windows.
* Differential: oracle builds of `test-basic` and `test-root` with normalized
  comparisons, and a `search.html` asset-loading test (`searchtools.js`,
  `language_data.js`, `searchindex.js`, `#search-results`, `Search.init`,
  stemmer assets).

Workspace total after Phase 18: **680 tests** (675 library/integration + 5
doctests). No panics on empty documents, non-ASCII text or missing domain
metadata.
---

## 25. Phase 19 alternate HTML builders (`scribe-builders`)

Phase 19 adds the four `StandaloneHTMLBuilder` subclasses Sphinx ships as
separate builders, plus the directory-style URL variant. None of the frozen
§7 AST, §12 `BuildEnvironment` or §15 `Domain`/`ObjectEntry` public shapes
changed; all work happens on cloned documents inside the builder.

### 25.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/builders/dirhtml.py` — `get_target_uri` (`index → ''`,
  `…/index → …/`, otherwise `<docname>/`) and `get_output_path` (drop a trailing
  `index` component, write `<parts>/index.html`).
* `sphinx/sphinx/builders/singlehtml.py` — `get_target_uri` returns
  `#document-<docname>` for docs in `env.all_docs`, `get_relative_uri` ignores
  the source page, `assemble_doctree` starts at `root_doc`, and `finish()`
  writes only assets/buildinfo/inventory (no index page or search index).
* `sphinx/sphinx/util/nodes.py::inline_all_toctrees` — depth-first toctree
  traversal, `traversed` de-duplication, replacement of toctree nodes with
  inlined document bodies, and `start_of_file(docname=…)` wrappers.
* `sphinx/sphinx/writers/html5.py` — `visit_start_of_file` emits
  `<span id="document-<docname>"></span>`; `visit_reference` uses `refuri`
  verbatim, so the double-anchor case collapses to a plain in-page anchor.
* `.oracle-venv/.../sphinxcontrib/{serializinghtml,htmlhelp,qthelp,devhelp}/__init__.py`
  and their Jinja templates — per-page `.fjson` context dumps, `.hhp/.hhc/.hhk`,
  `.qhp/.qhcp`, gzipped `.devhelp.gz`, basename defaults and locale constants.
* Fixtures `test-builder-dirhtml`, `test-toctree`, `test-basic`, plus oracle
  builds of all six builders (`sphinx-build -b <builder> -E -q`).

### 25.2 Public API

| Item | Purpose |
|---|---|
| `DirectoryHTMLBuilder::build(&BuildOptions, &Config, &mut BuildEnvironment)` | Run the `dirhtml` build. |
| `dirhtml::{target_uri, output_path, relative_resource, is_page_output}` | Exact upstream URI/output-path rules and resource relativity. |
| `SingleFileHTMLBuilder::build(...)` | Run the `singlehtml` build. |
| `singlehtml::{target_uri, traversal_order, rewrite_anchors, global_toc, PreparedPage}` | Inlining/anchor primitives. |
| `JsonBuilder::build(...)`, `HtmlHelpBuilder::build(...)`, `QtHelpBuilder::build(...)`, `DevHelpBuilder::build(...)` | Metadata builder entry points. |
| `metadata::{make_filename, make_filename_from_project, write_htmlhelp_files, write_qthelp_files, write_devhelp_files}` | Deterministic project-file generators. |
| `html::builder::BuildFlavor` / `build_with_flavor(...)` | Shared pipeline parameterization (name, URI style, suffix, embedded/serializing/single-file flags). |
| `html::translator::SingleFileContext` | `(docname#anchor) → unique anchor` table used during single-page link rewriting. |

The CLI dispatches `html`, `dirhtml`, `singlehtml`, `json`, `htmlhelp`,
`qthelp` and `devhelp`; any other name still raises `scribe::unimplemented_builder`.

### 25.3 Build pipeline reuse and link-rewriting rules

All six builders call the same `build_with_flavor` pipeline as Phase 17: parse →
domain registration → read/resolved transforms → parallel body rendering →
sorted deterministic writes. Differences are confined to `BuildFlavor` knobs:

* **URI / path.** `dirhtml` and `json` use the directory-style target URI and
  output path above; the others use `<docname><out_suffix>`. The theme's
  `pathto` reads the flavor's `target_uris`, `link_suffix` and `baseuri`, so
  relbar/sidebar/toctree links, image/static paths and search-result links are
  relative to the *page's directory* (e.g. `foo/index.html` uses `../_static/…`).
  Generated index pages get their own `docname` so their links are relative too.
* **`dirhtml` self-links.** `allow_sharp_as_current_path` stays `true` (matching
  upstream), so `pathto(current)` → `#`.
* **Single-file.** `traversal_order` follows toctree entries depth-first in
  source order with a `traversed` set, starting at `root_doc`. `rewrite_anchors`
  assigns each node id a globally unique anchor (collisions get
  `-<docname>[-N]`) and records `(docname, original-id) → final-id`.
  `SingleFileContext` rewrites `refuri`/`refid` at translate time: an inlined
  doc with no fragment becomes `#document-<docname>`; a link with a fragment
  becomes the single, unique `#<final-anchor>` (this is upstream's
  double-anchor collapse). Every inlined doc — including the root — emits a
  `<span id="document-<docname>"></span>` marker so all such links resolve.
  Toctree wrapper nodes are suppressed because their bodies are spliced into
  the page; navigation comes from the assembled global TOC.
* **Embedded builders.** `htmlhelp`, `qthelp` and `devhelp` disable permalinks,
  source dumps, the search index and the search page, then emit their project
  files from the written file list, the config and the toctree hierarchy.
* **Serializing (`json`).** Page contexts are dumped to `<docname>.fjson`
  instead of rendered HTML; `globalcontext.json`, `searchindex.json`, the
  environment marker and `last_build` are written on finish.

### 25.4 Determinism and divergences

* No timestamps or Sphinx version strings are embedded; gzip uses `mtime=0`.
  `py-modindex` output is retained from Phase 17 (Sphinx omits it when there
  are no Python module objects), so scribe's page set may be a superset of the
  oracle's; differential tests assert the oracle's pages are all present.
* The single-file global TOC is a flat skeleton with `#document-*` links;
  upstream's section numbers and nested `<li>` classes are not reproduced (a
  documented normalization). Cross-document links resolve to in-page anchors
  and no `.html` link to an inlined document remains.
* `devhelp` writes both the oracle's gzipped `.devhelp.gz` and an uncompressed
  `.devhelp2` (the name used by the task spec) from the same XML payload.
* `render_index_body` object links and generated-page `pathto` calls are made
  relative to each generated page's base URI; the earlier
  `alabaster.css_t → alabaster.css.css` asset-name bug was corrected as part of
  making dirhtml asset links resolvable.

### 25.5 Tests

`crates/scribe-builders/tests/alternate_builders.rs` adds **14 tests**
(unit URI/path rules, filename helpers, single-page/external-link robustness,
duplicate-anchor renumbering, dirhtml layout + link integrity against
`test-builder-dirhtml` and `test-basic`, singlehtml single-file + rewritten
anchors against `test-toctree`, metadata file structure against `test-basic`,
and determinism across two runs for all four metadata builders), plus 4
in-module unit tests (`dirhtml` 3, `singlehtml` 1).

Workspace total after Phase 19: **698 tests** (693 library/integration + 5
doctests), all green on three consecutive runs. No panics on documents with no
toctree, a single page or external links.
---

## 26. Phase 20 built-in core extensions suite (`scribe-core` + `scribe-builders`)

Phase 20 adds native Rust implementations of the seven built-in extensions the
task spec names. The frozen contracts from §7 (`Document`/`NodeKind`), §12
(`BuildEnvironment`) and §15 (`Domain`) are **unchanged**. All work happens in
`crates/scribe-core/src/extensions/`, small additive seams in `scribe-rst`,
the HTML build pipeline in `scribe-builders/src/html/builder.rs`, and one new
writer special case in `html/translator.rs`.

### 26.1 Extension registry and activation seam

`scribe_core::extensions` is the single registry/activation point:

| Item | Purpose |
|---|---|
| `BUILTIN_EXTENSIONS` | The seven fully-qualified names implemented natively. |
| `ActivatedExtensions::from_config(&Config)` | Reads `extensions` and returns active short names plus warnings. Unknown names produce `extension <name> not found`, never a panic. |
| `register_config_keys(&mut ConfigRegistry)` | Idempotent declaration of every extension-owned config key. |
| `register_directives(&mut DirectiveRegistry, &ActivatedExtensions)` | Installs only the directives of active extensions (`todo`, `todolist`, `ifconfig`, `graphviz`, `graph`, `digraph`). |
| `extlink_roles(&Config) -> (ExtLinkRoles, Vec<String>)` | Builds the dynamic role map consumed by the RST inline recogniser. |
| `TodoCollector`, `ViewcodeCollector`, `GraphvizRender`, `DurationReport`, `GithubPagesFiles` | Build-phase data holders / hook implementations. |

**Phase 27 seam.** Upstream wires these through the `sphinx.application` event
bus. Today the builder calls the same logical hooks in build order, with no
event machinery:

| Upstream event | Phase 20 hook |
|---|---|
| `config-inited` | `register_config_keys`, `ActivatedExtensions::from_config` |
| `builder-inited` | `register_directives`, `extlink_roles` |
| `doctree-read` | `ifconfig::process`, `TodoCollector::collect`, `ViewcodeCollector::collect` (the latter after the Phase 10 domain transforms that enrich `desc_signature`) |
| `doctree-resolved` | `TodoCollector::expand`, `hardcoded_link_warnings`, `GraphvizRender::render_document` |
| `html-collect-pages` | `write_viewcode_pages` (`_modules/*.html` + overview) |
| `env-updated` | `GithubPagesFiles::from_config` + `.nojekyll`/`CNAME` emission |
| `build-finished` | `DurationReport::render` / `to_json` |

A future event emitter can subscribe these functions directly; no frozen type
changes are needed. The Python shim / third-party bridge lands in Phase 27.

### 26.2 Extension-node representation (frozen-AST compatible)

Upstream's new Docutils node classes (`todo_node`, `todolist`, `ifconfig`,
`graphviz`, `viewcode_anchor`) cannot become new `NodeKind` variants without
breaking §7. They are represented by a `NodeKind::Generated` node carrying the
`EXT_MARKER = "scribe_ext_node"` attribute with values `todo`, `todolist`,
`ifconfig`, `graphviz`. Most are lowered to ordinary Docutils nodes before
rendering (todo → admonition with `classes=["admonition-todo"]` and a `Todo`
title; viewcode → `Reference` with `classes=["viewcode-link"]`; ifconfig → its
children or a `system_message`). Only graphviz needs a writer special case,
keyed by node id in `HtmlContext::graphviz`.

### 26.3 Per-extension behaviour

* **`sphinx.ext.todo`** — `.. todo::` lowers to `NodeKind::Admonition` with the
  `admonition-todo` class, an explicit target id (`idN`), the docname, source
  path/line and an optional first-paragraph argument. `.. todolist::` is a
  placeholder expanded after all documents are read: every collected todo is
  cloned with cleared ids, followed by a `paragraph.todo-source` backlink
  (`<doc>.html#<id>`, or `#<id>` for a self-link). `todo_link_only` shortens the
  backlink text to `original entry`; `todo_include_todos` gates both the
  admonition and the list; `todo_emit_warnings` logs `TODO entry found: …`.
* **`sphinx.ext.ifconfig`** — `.. ifconfig:: <expr>` stores the raw expression
  on an extension node; `ifconfig::process` evaluates it with a hand-written
  tokenizer + recursive-descent parser (no Python `eval`). Supported grammar:
  `and`/`or`/`not`, parentheses, string/number/bool/`None` literals,
  config-name lookups, `==`/`!=`/`<`/`<=`/`>`/`>=`, `in`/`not in`/`is`/`is not`
  and tuple/list literals. True nodes are replaced by their children, false
  nodes removed, invalid expressions become a `system_message` with a
  Python-shaped exception string. The builder name is available as `builder`.
  Unknown config names emit `NameError` diagnostics.
* **`sphinx.ext.extlinks`** — the `extlinks` dict is parsed into
  `ExtLinkSpec { name, base_url, caption }` and installed into
  `ParseOptions.extlinks` / `RstContext.extlinks` (additive `scribe-rst` seams).
  `emit_role` checks these before the built-in role table and produces a
  `Reference` with `internal=false`, `refuri = base_url % payload`, an
  `extlink-<name>` class and a caption from `caption % payload` (or the full
  URI when `None`). `%s` repetition and `%%` escaping work; malformed entries
  are skipped with diagnostics. `extlinks_detect_hardcoded_links` adds
  upstream's priority-500 post-transform warning (presence, not truth, of the
  `internal` attribute is the exclusion rule).
* **`sphinx.ext.viewcode`** — after the domain transform pipeline enriches
  `desc_signature` nodes, `ViewcodeCollector` reads `module`/`fullname`,
  locates `source_dir/<module>.py` (or `/__init__.py`), parses definition tags
  with a light indentation-aware scanner and injects a `[source]` reference to
  `_modules/<module>.html#<fullname>`. When the module attribute is empty
  (earlier-phase module-context gap), the defining module is inferred by
  scanning local `.py` files. `scribe-builders::html::builder::write_viewcode_pages`
  highlights the source with Phase 15 `highlight_code`, inserts
  `<div class="viewcode-block" id="name">` + `[docs]` backlinks, and writes
  `_modules/<module>.html` plus `_modules/index.html`. `viewcode_line_numbers`
  enables inline line numbers; `viewcode_import` is accepted as a no-op.
* **`sphinx.ext.graphviz`** — `graphviz`/`graph`/`digraph` directives store the
  dot code and options (`alt`, `align`, `layout`/`graphviz_dot`,
  `output-format`, `caption`, `class`, `name`) on a generated node; a caption
  wraps it in a `figure`/`caption` pair. `GraphvizRender` invokes `dot` with the
  configured executable and args, hashes the inputs with BLAKE3, writes
  `_images/graphviz-<hash>.{png,svg}` and reads the `.map` cmapx file. Missing
  or failing `dot` emits one warning and stores a text fallback — never a build
  failure. The translator emits `<object>` for SVG and `<img>` (+ usemap) for
  PNG, relative to the current page.
* **`sphinx.ext.githubpages`** — for HTML-format builders always writes
  `.nojekyll`; writes `CNAME` with `urllib.parse`-equivalent hostname from
  `html_baseurl`, unless the host is empty or ends in `.github.io` (in which
  case an existing `CNAME` is removed). No trailing newline, matching upstream.
* **`sphinx.ext.duration`** — per-document parse durations are accumulated in
  `DurationReport`. At build end the report prints the total and the N slowest
  documents (config-gated); `duration_write_json` writes a sorted, rounded JSON
  payload.

### 26.4 Differential method and normalization

`crates/scribe-builders/tests/extensions_differential.rs` builds both engines
with `-E` into separate temp dirs and compares the `<div class="body"
role="main">` fragment after normalization:

1. `?v=<hash>` values, graphviz image hashes and absolute todo source paths are
   replaced with stable placeholders;
2. whitespace runs collapse to single spaces;
3. curated structural markers are asserted in both bodies instead of raw-byte
   equality (earlier-phase parser differences — toctree `compound` class,
   section numbers, substitution directives — remain owned by their phases);
4. extlink warnings are compared by their replacement suggestion shape;
5. oracle unavailability skips differential assertions while scribe-only
   invariants still run;
6. a missing `dot` binary is exercised explicitly in the core tests and
   degrades to escaped source text + a warning.

Fixtures: `test-ext-todo` (with `-D todo_include_todos=1`), `test-ext-ifconfig`,
`test-ext-extlinks-hardcoded-urls{,-multiple-replacements}`,
`test-ext-githubpages`, `test-ext-graphviz`, a minimal `test-ext-viewcode`
derivative using explicit py directives (autodoc is Phase 21), plus an
all-extensions build that constructs `Config` entirely in Rust to assert there
is no Python runtime dependency.

### 26.5 Tests and known limitations

`crates/scribe-core/tests/extensions.rs` adds 22 integration tests (activation,
config registration, ifconfig grammar/robustness, extlink parsing/roles/
hardcoded warnings, todo collection/expansion, viewcode collection/injection,
graphviz node specs and missing-`dot` degradation, githubpages rules, duration
report, and oracle-fixture no-panic checks). `crates/scribe-builders/tests/
extensions_differential.rs` adds 8 differential/integration tests. In-module
unit tests bring the Phase 20 total to **52 new tests**; workspace total
**750 tests** (745 library/integration + 5 doctests), green on three consecutive
runs. `cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` are clean.

Known divergences: graphviz uses a BLAKE3 hash prefix instead of upstream's
SHA-1 filename (normalized in tests); SVG relative-link rewriting and cmap
`<map>` clickability parsing are best-effort; the substitution-form
`.. |name| digraph::` fixture is affected by a pre-existing substitution
parser gap; viewcode module pages use a slightly wider highlight wrapper and
rely on local source scanning when the Python-domain module context is empty;
ifconfig reports `NameError` for config values registered only by a fixture's
Python `setup()` (the rendered body still matches). Phase 17's `skip_depth`
visitor bookkeeping was corrected so self-rendering nodes (toctree, literal
blocks, graphviz) no longer mis-nest following siblings — the existing 665+
Phase 17 tests remain green.
---

## 27. Phase 21 native Python static & dynamic autodoc + napoleon (`scribe-autodoc`)

Phase 21 adds a native autodoc/napoleon engine in `crates/scribe-autodoc`.
The frozen contracts from §7 (`Document`/`NodeKind`), §12 (`BuildEnvironment`)
and §15 (`Domain`) are **unchanged**. The only cross-crate edits are additive:
two extension names in `scribe_core::extensions::BUILTIN_EXTENSIONS`, one call
each to `scribe_autodoc::register_config_keys` and
`scribe_autodoc::register_directives` from the Phase 20 extension registry, and
new workspace dependencies (`rustpython-parser`, `rustpython-ast`).

### 27.1 Upstream sources inspected

Behavior was extracted from the pinned oracle before implementation:

* `sphinx/sphinx/ext/autodoc/{__init__,_directive,_generate,importer,_directive_options}.py` —
  the `AUTODOC_DEFAULT_OPTIONS` / `_OPTION_SPECS` tables, `members_option`,
  `exclude_members_option`, `inherited_members_option`, `member_order_option`,
  `class_doc_from_option`, `merge_members_option`, and the import-failure path.
* `sphinx/sphinx/ext/napoleon/docstring.py` — `GoogleDocstring` /
  `NumpyDocstring`, `_sections`, `_consume_fields`, `_format_fields`,
  `_parse_{parameters,returns,yields,raises,attributes,examples,notes,see_also,warns}_section`.
* Fixtures `test-ext-autodoc`, `test-ext-napoleon`, `test-ext-napoleon-paramtype`
  and their oracle `-b pseudoxml` trees. Both engines were also observed through
  a small controlled `automodule` fixture to confirm child ordering
  (`index` → `desc` with `desc_signature` before `desc_content`).

### 27.2 Dependency direction

```
scribe-ast ──┐
scribe-config ─┼──► scribe-rst ──┐
              │                  ├──► scribe-autodoc ──► scribe-core ──► cli
scribe-domains ─┘                  │        │
                                   └────────┘ (signature delegation)
```

`scribe-autodoc` depends on `scribe-ast`, `scribe-config`, `scribe-rst` and
`scribe-domains`, but **not** on `scribe-core`; `scribe-core` depends on
`scribe-autodoc` so the extension registry can install the directives.
Signature parsing is *never duplicated*: the crate emits the same skeleton
`desc` nodes as the Phase 12 `py:*` directives and delegates to
`scribe_domains::python::prepare_document` / `build_desc_signature`.

### 27.3 Static model and extractors

`model::{ModuleInfo, ClassInfo, FunctionInfo, AttributeInfo, Member, ImportInfo,
SourceSpan, ParameterInfo}` form a serde-serializable tree with byte spans.
`parser::parse_source` uses `rustpython-parser` (`Mode::Module`, with `.py` and
`.pyi` accepted) and extracts:

* module/class/function docstrings (PEP 257 dedent, adjacent literal concatenation);
* `__all__` from list/tuple/set/`+` expressions;
* functions, async functions, nested classes, annotated assignments and plain
  assignments;
* decorators (dotted expression names) → `property`, `staticmethod`,
  `classmethod`, `abstractmethod`, `final`, `overload`;
* parameter annotations and default values **from exact source byte slices**,
  so the rendered signature preserves source spelling;
* base classes, import aliases (for cross-file inheritance), and implicit
  `self`/`cls` removal for methods;
* `@overload` handling: all overload declarations are kept and the concrete
  implementation following them is hidden, matching upstream.

`parser::parse_sources` parses a module tree in parallel with `rayon`; results
are ordered by input. `discovery::load_modules` resolves dotted names against
the source dir plus `autodoc_path` roots, walks package `__init__.py` trees with
`walkdir`, and returns one warning per unresolvable/unparsable module. A
`ModuleSet` indexes all parsed classes so `resolve_class` can follow `from .base
import Base` aliases across files for `inherited-members`. A content-hash-keyed
`BTreeMap` cache (`path | BLAKE3(source)`) avoids re-parsing unchanged modules.

### 27.4 Dynamic fallback worker protocol

`dynamic::introspect` is an explicitly documented, optional bridge used only
for modules the static parser cannot cover (generated code, C extensions,
heavy import side effects) or when a future config demands it. The Rust side
spawns `<python> -c WORKER_SCRIPT <module> <root...>`, where the worker
prepends roots to `sys.path`, imports the module, walks `vars(module)` with
`inspect`, and prints one compact JSON object (`name`, `docstring`, `members`
with `objtype`/`signature`/`docstring`/`annotation`/`bases`/`line`). Failures
(non-zero exit, invalid JSON, missing interpreter) produce
`AutodocError::Dynamic`; callers warn and degrade to the static (often empty)
tree. The bridge is never required for a build and is exercised against the
oracle venv in `tests/performance.rs`.

### 27.5 Napoleon converter

`napoleon::{convert_docstring, NapoleonOptions}` translates Google-style
(`Args:`/`Arguments:`/`Parameters:`, `Keyword Args:`, `Returns:`, `Yields:`,
`Raises:`, `Attributes:`, `Examples:`, `Note(s):`, `See Also:`, `Warnings:`,
`Other Parameters:`, `Methods:`, `Receives:`, `Warns:`, `References:`) and
NumPy-style underlined sections into RST matching upstream napoleon:
`:param:`/`:type:`, `:keyword:`, `:returns:`/`:rtype:`, `:Yields:`,
`:raises Type:`, `.. attribute::`, `.. method::`, `.. rubric::`, `.. note::`,
`.. seealso::` (NumPy emits `:py:obj:` targets), and `.. warning::`. The RST is
fed through `DirectiveContext::parse_block_text` into the frozen AST, and the
Python domain's `rewrite_content_fields` groups `:param:` fields into the
*Parameters* field list. `napoleon_google_docstring` /
`napoleon_numpy_docstring` and the `use_*` toggles are read from `Config`.

### 27.6 Auto directives and options

`directives::register_directives` installs `automodule`, `autoclass`,
`autoexception`, `autofunction`, `automethod`, `autoproperty`, `autoattribute`,
`autodata`, `autodecorator` plus `attribute`/`method` aliases for
napoleon-emitted markup. `config::auto_options` resolves `:members:`,
`:undoc-members:`, `:private-members:`, `:special-members:`,
`:inherited-members:`, `:show-inheritance:`, `:exclude-members:`,
`:member-order:`, `:no-index:`, `:noindex:`, `:no-index-entry:`,
`:ignore-module-all:`, `:class-doc-from:`, `:annotation:`, `:no-value:` with
upstream precedence (directive > `autodoc_default_options` > config defaults).
`autoclass_content` selects class / init / both docstrings. Missing modules emit
an upstream-style `autodoc: failed to import module …` warning paragraph rather
than failing the build. Every generated object is a skeleton `desc`, so
signatures, anchors and cross-references are produced by the frozen Python
domain pipeline exactly as for hand-written `py:*` directives.

### 27.7 Differential method and normalization

`crates/scribe-autodoc/tests/differential.rs` builds the three real fixture
roots with `.oracle-venv/bin/sphinx-build -b pseudoxml -E -q` and parses the
same `.rst` sources through scribe's RST → directive → `prepare_document`
pipeline. Both trees are reduced to the documented subset:

1. **object identities** — `(module, class, fullname)` triples from every
   `desc_signature`, compared as a sorted set *and* in document order;
2. **signature text** — ordered `desc_addname` + `desc_name` text tokens;
3. **docstring field names** — `field_name` text, lowercased and sorted;
4. **targeted presence assertions** — parameter names and annotation tokens
   inside signatures.

Normalized away: generated ids, source paths, XML namespaces, CSS classes,
`py:*` skeleton attributes, index-entry payloads, `xml:space`, and whitespace
runs. The frozen AST is never edited by autodoc code.

### 27.8 Tests, gates and divergences

New tests: 6 extractor/discovery (plus 4 in-module), 5 napoleon section (plus 4
in-module), 6 member filtering/ordering, 5 oracle differentials, 4
performance/dynamic-fallback (plus 10 in-module) — **40 new tests**. Workspace
total **790 tests**, green on three consecutive runs; `cargo check --workspace
--all-targets` and `cargo clippy --workspace --all-targets -- -D warnings` are
clean. A 48-module / 1,152-function parse completes in milliseconds (timing is
reported, not gated); malformed and NUL-containing sources never panic.

Known static-vs-dynamic divergences (asserted/documented, not hidden):

* `test-ext-autodoc` reaches identical object identities and order, but
  dynamic-only synthesized text such as `alias of Foo` is approximated from a
  bare identifier assignment, and imported-member following is limited to
  statically resolvable aliases.
* `test-ext-napoleon-paramtype`: the frozen Python domain groups `:keyword:`
  fields under **Parameters** (upstream uses **Keyword Arguments**) and does
  not convert `:kwtype:`/`paramtype` aliases, so those appear as extra fields.
  Parameter *names* and type values still match; this is a domain-level gap,
  deliberately left out of the frozen Phase 12 code.
* `:returns:` fields produced by plain RST docstrings keep their lowercase
  label; napoleon-generated `Returns:` sections can be emitted with a
  capitalized field name to match upstream's *Returns* grouping.
* Napoleon's `.. attribute::` / `.. method::` aliases are mapped to the
  Python-domain skeleton emitter; exotic `napoleon_custom_sections` and
  `.. index::` directive sections are not interpreted.
* Dynamic introspection is a fallback only; dynamic-only values (runtime
  computed attributes, `__slots__` descriptors, dataclass-generated members)
  are absent from the static tree by design.
---

## 28. Phase 22 autosummary & doctest (`scribe-autodoc` + `scribe-builders`)

Phase 22 adds the two remaining built-in extension/builders from the Sphinx
core distribution: `sphinx.ext.autosummary` (in `scribe-autodoc`) and
`sphinx.ext.doctest` (in `scribe-builders`). The frozen contracts from §7
(`Document`/`NodeKind`), §12 (`BuildEnvironment`) and §15 (`Domain`) are
**unchanged**: autosummary emits ordinary `Table`/`Tgroup`/`Row`/`Entry` and
`Toctree` nodes, while doctest reuses `LiteralBlock`/`Comment` nodes carrying
the upstream `testnodetype` marker attribute. No new `NodeKind` variant was
introduced.

### 28.1 Upstream sources inspected

Behavior was transcribed from the pinned oracle before implementation:

* `sphinx/sphinx/ext/autosummary/__init__.py` — `Autosummary.run`,
  `get_items`, `get_table`, `extract_summary`, `mangle_signature`,
  `limited_join` and the `autosummary_toc`/`autosummary_table` wrappers.
* `sphinx/sphinx/ext/autosummary/generate.py` — `AutosummaryRenderer`
  (`escape`/`e`/`underline` filters), `generate_autosummary_content`,
  `generate_autosummary_docs`, `find_autosummary_in_lines` (including its
  `currentmodule`/`automodule` tracking and bare-name qualification).
* `sphinx/sphinx/ext/autosummary/templates/autosummary/{base,module,class}.rst`
  — vendored verbatim into `src/autosummary/templates.rs` with the upstream
  BSD-3-Clause attribution in `src/autosummary/NOTICE`.
* `sphinx/sphinx/ext/doctest.py` — `TestDirective.run`, `TestGroup.add_code`,
  `DocTestBuilder.test_doc` / `test_group` / `finish`, `SphinxDocTestRunner`
  and the `setup()` config table.
* Real roots `test-ext-autosummary{,-filename-map,-template,-module_all,
  -module_empty_all,-recursive}` and `test-ext-doctest{,-skipif}` (the latter
  two as self-contained derivatives, see §28.6).

### 28.2 Public API

`scribe_autodoc::autosummary` (re-exported from `scribe_autodoc`):

```
AutosummaryDirective, AutosummaryItem, AutosummaryOptions, StubRenderer,
StubReport, generate_stubs, escape_rst, mangle_signature, parse_names,
first_summary, limited_join, items_from_names, emit_table, filename_map,
register_config_keys, register_directives
```

`scribe_builders::doctest` (re-exported from `scribe_builders`):

```
DoctestBuilder, DoctestResult, DoctestError, TestDirective,
DoctestDirectiveKind, TestCode, TestGroup, extract_codes, group_codes,
doctest_block_group, normalize_whitespace, ellipsis_match, outputs_match,
parse_option_string, option_flag, failure_diff, format_summary,
DoctestDocument, DoctestRunResult, DoctestDocResult, RunnerOptions,
DoctestRunnerError, find_python, run_worker, WORKER_SCRIPT,
register_directives, register_config_keys
```

The CLI dispatches `scribe build -b doctest` to `DoctestBuilder`; the HTML
builder calls `generate_stubs` before `BuildEnvironment::update` whenever
`sphinx.ext.autosummary` is active and `autosummary_generate` is on (default
`true` upstream). The Phase 20 extension registry lists both new extension
names and installs the `autosummary` directive.

### 28.3 Autosummary table and stub generation

`get_items` is replaced by `items_from_names`, which resolves the longest
module prefix with the Phase 21 `resolve_module`/content-hash cache, then
reads signatures from `FunctionInfo.signature` and summaries from the first
sentence/stanza via `first_summary`. `mangle_signature` reproduces upstream's
`_cleanup_signature` + `opt_re` behavior, including its greedy last-default
quirk (`f(a, b=2, *args, **kwargs)` → `(a[, b])`). Missing names produce a
visible warning paragraph; they are returned in `run`'s produced-ID list so
the Phase 06 resolver splices them into the doctree.

`get_table` maps to `emit_table`: a `Table(classes=[autosummary, longtable])`
→ `Tgroup(cols=2)` → two `Colspec(colwidth=10/90)` → `Tbody` → `Row` →
`Entry`/`Paragraph`. The first column is a `:py:obj:` pending xref plus the
`rst.escape`-d signature. Additive `:list-ordered:` emits an `EnumeratedList`
instead. `:toctree:` additionally emits a hidden `NodeKind::Toctree` with the
Phase 09 parallel entry lists so the collector populates `env.toctree_includes`.

`generate_stubs` scans `.rst` sources for `.. autosummary::` blocks, tracks
`currentmodule`/`automodule` to qualify bare names exactly like upstream,
renders with a dedicated sandboxed minijinja `StubRenderer` (reusing Phase
16's `{% trans %}` preprocessor), writes `source_encoding`-aware bytes (UTF-8
BOM for the default `utf-8-sig`), and rescans newly generated files for
`:recursive:` package trees. Phase 21's `discover_package` supplies submodules;
imported `__all__` members are resolved through the same cache so stub member
lists, inherited `self.*` attributes and ordering match upstream.

### 28.4 Doctest extraction and execution

The five directives emit `testnodetype`, `groups`, `options` (JSON bitmask),
`skipif`, `pyversion`, `language` and (when presentation differs) `test`
attributes on `LiteralBlock`/`Comment` nodes. `extract_codes` walks the frozen
AST in document order, including plain `doctest_block` nodes for free-standing
`>>>` examples; `group_codes` ports `TestGroup.add_code` including the `*`
"every group" marker and `testcode`/`testoutput` pairing.

Execution is delegated to a vendored Python worker (`WORKER_SCRIPT`) that
mirrors upstream `DocTestBuilder.test_group`: it builds `doctest.DocTest`
objects with `DocTestRunner(optionflags=...)`, shares one `_fakeout` namespace
per document, swaps `doctest.compile` to `exec` for setup/cleanup and to
`single` for examples, applies the directive/flag option maps, evaluates
`:skipif:` with `doctest_global_setup` and treats `:pyversion:` mismatches as
SKIP *examples* (counted as tries, like upstream). A fresh namespace is used
per group; `doctest_global_setup`/`cleanup` are prepended/appended. The worker
returns JSON with the runner's captured output, per-doc results and totals;
Rust writes the upstream-shaped date header, body and `Doctest summary` to
`output.txt` and reports per-doc PASS/FAIL on stdout.

Interpreter discovery order: `SCRIBE_DOCTEST_PYTHON`, `SCRIBE_PYTHON`, the
workspace `.oracle-venv/bin/python`, then `python3`/`python` on `PATH`. A
missing interpreter, a non-zero worker exit or a protocol error is a warning
plus "examples skipped" in `output.txt`, never a panic and never a test
failure. The default flags bitmask is `41` (DONT_ACCEPT_TRUE_FOR_1 | ELLIPSIS
| IGNORE_EXCEPTION_DETAIL), matching the pinned `doctest` module.

### 28.5 Differential method and normalization

* **autosummary** (`tests/autosummary_differential.rs`): the oracle builds
  five real fixture roots with `-b html -E -q` from each fixture directory;
  generated `.rst` stubs are compared **byte-for-byte** (after stripping the
  UTF-8 BOM) against scribe's `generate_stubs` on an identical copy. All eight
  stubs of `test-ext-autosummary` and the stubs of the four variants match
  exactly. The summary table is reduced to display-name facts from the oracle
  `<table>` (`<span class="pre">`) and scribe's `PendingXref` rows.
* **doctest** (`tests/doctest_differential.rs`): oracle and scribe run the same
  self-contained fixtures; `output.txt` is compared byte-for-byte after
  dropping only the first two timestamp header lines. A third test removes
  the pytest-dependent cleanup from the real `test-ext-doctest` root and
  asserts the oracle's 22-example count equals scribe's.

Normalized away: the timestamp header, generated ids, source paths beyond the
relative file name and the explicitly documented static-mode divergences in
§28.6. Real Python execution is asserted (both engines execute 16/22 examples
in the self-contained fixture), so this is not a string-only comparison.

### 28.6 Tests, gates and divergences

New tests: 8 autosummary unit, 9 integration, 2 oracle differentials; 10
doctest unit, 9 integration, 2 oracle differentials — **40 new tests**,
workspace total **830** (790 prior + 40), green on three consecutive runs;
`cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` are clean. Malformed directive bodies, invalid
option strings, NUL input and a missing interpreter are all covered by
no-panic assertions.

Known divergences (documented and asserted, not hidden):

* `autosummary_importfail` resolves statically (the module exists on disk)
  while the oracle's real import raises `SystemExit`; scribe therefore emits
  one extra stub and one extra table row for that fixture root.
* Dynamic-only instance attributes (`Foo.value`, inherited `self.value`) are
  recovered by scanning `self.x = ...` assignments and statically resolving
  base classes through imports; values computed at runtime remain invisible.
* `:autolink:` is not registered as a separate role; `:py:obj:` xrefs in the
  generated table resolve through the frozen Phase 14 engine.
* Doctest filename/line reporting uses the relative source path and upstream's
  `line - 1` convention; docstring-sourced examples are not yet supported.
* The doctest worker counts `:pyversion:`-skipped examples the way upstream
  does (SKIP flag on the example), while `:skipif:` drops the node entirely.
---

## 29. Phase 23 LaTeX writer & PDF builder (`scribe-builders`)

Phase 23 turns the frozen doctree into Sphinx-compatible, compilable LaTeX and
adds a `latex` / `pdf` builder. Like Phase 17 it is a *writer*: it consumes the
frozen §7 AST, the §12 `BuildEnvironment`, the §14 transform/xref pipeline and
the §15 domain engine. Nothing in the frozen AST, environment or domain shapes
was modified. EPUB/man/texinfo/text builders are Phase 24 and are deliberately
out of scope here.

### 29.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/writers/latex.py` — all 112 `visit_*` / `depart_*` visitor
  pairs; in particular `visit_document`, `visit_section`, `visit_title` (the
  `LATEXSECTIONNAMES` mapping `part/chapter/section/...`), `visit_paragraph`
  (`\sphinxAtStartPar`), `visit_literal_block`, `visit_table`
  (`tabularx`/`longtable`/`tabularity`), the `sphinxadmonition` family,
  `visit_reference` (`\hyperref`/`\href`), `visit_math`, `visit_index`,
  `visit_image` (`\sphinxincludegraphics`) and `hypertarget`/`idescape`.
* `sphinx/sphinx/builders/latex/{__init__,constants,nodes,transforms,util,theming}.py`
  — the `latex_documents` 5-tuple, `latex_engine`, `latex_elements` keys and
  defaults (`DEFAULT_SETTINGS`), and the support-file copy list.
* `sphinx/sphinx/templates/latex/latex.tex.jinja` — the exact preamble slot
  order (`\documentclass`, `inputenc`/`fontenc`, `amsmath`, `sphinx`,
  `hyperref` last, `\makeindex`, title/author/date, `\begin{document}`,
  `\sphinxmaketitle`, `\sphinxtableofcontents`, body, `\printindex`).
* `sphinx/sphinx/util/texescape.py` — the TeX special-character map, the
  Unicode symbol map and the `tex_replace_map` id escaping.
* `sphinx/sphinx/texinputs/` — `sphinx.sty`, `sphinxhowto.cls`,
  `sphinxmanual.cls`, `sphinxlatex*.sty`, `python.ist` (vendored, see §29.4).
* Fixtures `sphinx/tests/roots/test-latex-*` with
  `.oracle-venv/bin/sphinx-build -b latex -E -q`.

### 29.2 Crate layout and dependency direction

```
scribe-builders ──► scribe-ast (frozen), scribe-config, scribe-rst
      ├──► scribe-core   (BuildEnvironment, transform pipeline)
      ├──► scribe-domains (DomainRegistry, xrefs)
      ├──► rayon (available; rendering is sequential today for determinism)
      └──► thiserror + miette (LatexError codes scribe::builders::latex::*)

scribe-cli ──► scribe-builders (CLI wiring only, no cycle)
```

New modules under `crates/scribe-builders/src/latex/`: `mod.rs`, `escape.rs`,
`translator.rs`, `builder.rs`, `support.rs` plus `texinputs/` (vendored
support files) and `texinputs/ATTRIBUTION.md`. Errors live in the crate's
`error.rs` (`LatexError`).

### 29.3 Public API

| Item | Purpose |
|---|---|
| `escape_latex` / `escape_latex_default` / `escape_latex_unicode` / `escape_id` / `LatexEngine` | TeX escaping and Unicode transliteration with `pdflatex`/`xelatex`/`lualatex` modes. |
| `LatexContext` | Owned per-document data: `docname`, `config`, `engine`, `title`, `author`, `release`, `date`, `theme`, `extra_preamble`, `use_sphinx_sty`, `numfig`, `language`, `logo`. `from_config` derives it from a `Config`. |
| `LaTeXTranslator::translate(&Document, &LatexContext) -> String` | Render a complete `.tex` document (preamble + body + closing). |
| `LaTeXTranslator::translate_body` / `render_document` / `render_preamble` | Body-only / assembly / preamble helpers used by tests and the builder. |
| `BuildOptions` (`BuilderOptions` alias analogue) | Clap-independent builder inputs: source/output dirs, `builder_name`, `force_all`, `freshenv`, `doctree_dir`, `filenames`, `worker_count`, `really_quiet`. |
| `BuildResult` | `written` files, `built_docnames`, `warnings`. |
| `LatexBuilder::build(&BuildOptions, &Config, &mut BuildEnvironment) -> BuildResult` | Run the full LaTeX/PDF build (associated function, as specified). |
| `LatexBuilder::compile_pdf(&Path, LatexEngine) -> Result<PathBuf>` | Invoke `latexmk` on one `.tex`; returns the PDF path. |

CLI wiring: `scribe build -b latex SOURCE OUT` writes `.tex` (and compiles a
PDF only when `latex_use_pdf = true`); `-b pdf` always compiles. Unknown
builders keep the existing `scribe::unimplemented_builder` diagnostic. Missing
source directories are typed `LatexError::MissingSource` wrapped in
`scribe::builder::latex_failed` at the CLI boundary — never panics.

### 29.4 Preamble strategy and vendored support files

Two preamble modes are provided:

* **Self-contained (default).** `\documentclass{report}` (`manual`) or
  `{article}` (`howto`), a carefully chosen set of widely available TeX Live
  packages (`fontenc`, `inputenc`, `amsmath`, `graphicx`, `geometry`,
  `longtable`, `tabularx`, `multirow`, `booktabs`, `fancyvrb`, `framed`,
  `textcomp`, `lmodern`/`fontspec`, `hyperref`, `makeidx`), plus definitions
  for the Sphinx macros the body emits (`\sphinxAtStartPar`,
  `\sphinxincludegraphics`, `\sphinxtableofcontents`, `sphinxVerbatim`,
  `sphinxadmonition`). This makes the generated `.tex` compile with a standard
  TeX Live installation, which is what the PDF compilation test asserts.
* **Sphinx-fidelity (`latex_elements["use_sphinx_sty"] = true`).** Loads the
  vendored `sphinxmanual.cls` / `sphinxhowto.cls` and `sphinx.sty`, requiring a
  full TeX Live. The vendored `.sty`/`.cls`/`.ist` files are copied to the
  output directory in *both* modes with attribution in
  `src/latex/texinputs/ATTRIBUTION.md` (BSD-3-Clause, Sphinx team).

### 29.5 Node → LaTeX mapping (representative)

| Node(s) | LaTeX |
|---|---|
| `document` | preamble + `\begin{document}`/ `\maketitle`/`\tableofcontents` + body + `\printindex`/`\end{document}` |
| `section` / `title` | `\chapter{…}\phantomsection\label{…}` (manual) or `\section{…}` (howto); deeper levels walk `section`/`subsection`/`subsubsection`/`paragraph` |
| `paragraph` / `text` | `\sphinxAtStartPar` + escaped text |
| `emphasis` / `strong` / `literal` | `\emph{…}` / `\textbf{…}` / `\texttt{…}` |
| `literal_block` / `doctest_block` | `sphinxVerbatim` environment (Phase 15 payload plain-text, transliterated for `pdflatex`) |
| `bullet_list` / `enumerated_list` / `definition_list` | `itemize` / `enumerate` / `description` with `\item` |
| `table` / `tgroup` | `tabularx` (`|X|X|…`) or `longtable` with `\hline`, `&`, `\\`, caption and colspec-derived column count |
| admonitions | `\begin{sphinxadmonition}{<type>}{<Title>} … \end{sphinxadmonition}` |
| `versionmodified` | `\emph{New in version …:}` etc. |
| `image` / `figure` | `\sphinxincludegraphics[width=…]{uri}` / `figure` (`center` when nested in an admonition) with `\caption` |
| `reference` | `\hyperref[label]{…}` / `\href{uri}{…}`; `footnote`/`citation` use `\footnote[…]`/`\ref{…}` |
| `math` / `math_block` | `$…$` / `\[…\]` (or `equation` when numbered) |
| `index` | `\index{…}` |
| `toctree` | `\sphinxtableofcontents` marker |
| `raw` | passthrough only for `format=tex|latex` |
| `desc*` | `\texttt`-styled signature fragments inside a `description` list |

### 29.6 Determinism and escaping

No timestamps are embedded: the `date` field is `config["today"]` (empty by
default) rather than the wall clock, so byte-identical `.tex` files are written
across runs and worker counts. Text escaping follows `texescape.py` (see §29.1):
TeX's ten special characters, bracket grouping and a curated Unicode symbol
map; `pdflatex` additionally transliterates accented Latin and Greek code
points to ASCII commands while `xelatex`/`lualatex` keep Unicode. Image widths
are normalised from CSS units (`px`, `ch`, `vw`, `Q`, …) to TeX `bp`.

### 29.7 Differential method and normalization

`tests/latex_differential.rs`: run the oracle with `-b latex -E -q` on
`test-latex-{index,unicode,table,numfig,equations}`, run the scribe builder on
the same roots, and reduce both `.tex` files to a set of canonical *structural
markers*. Aliases are collapsed (e.g. `sphinxmaketitle` → `MAKETITLE`,
`tabularx`/`longtable`/`tabularity` → `TABLE_ENV`,
`equation*`/`split`/`\[` → `MATH`); each fixture's curated marker set must be
present in both outputs. Full byte comparison is deliberately avoided: the
preambles differ by design and the shared parser lowers some constructs
differently (see §29.8). When the oracle venv is absent the scribe-side
assertions still run.

`tests/latex_pdf.rs`: a generated `.tex` from `test-latex-unicode` is compiled
with `latexmk -pdf -interaction=nonstopmode -halt-on-error` and the result is
asserted to exist, exceed 1 KiB and start with `%PDF`. A second test builds
with `-b pdf` and asserts a recorded PDF. Both skip (not fail) when `latexmk`
is missing, but they run for real in this environment.

### 29.8 Intentional divergences (relative to the oracle)

* **Self-contained preamble.** Upstream loads `sphinx.sty` plus `fncychap`,
  Babel, `textalpha` and xindy; our default preamble is minimal and defines
  the used macros inline for guaranteed compilability. The fidelity mode
  (`use_sphinx_sty`) is available but needs a full TeX Live.
* **Deterministic date.** Sphinx renders today's date; scribe uses `today`
  (empty by default) so `.tex` output is reproducible.
* **Preamble customizations.** Only a subset of `latex_elements` is honoured
  (`preamble`, and `use_sphinx_sty` as an scribe extension); `fvset`,
  `fncychap`, `sphinxsetup`, `babel`, geometry options and xindy are not
  individually mapped.
* **Parser-level differences.** The shared parser lowers inline `:index:`
  roles to text rather than `Index` nodes (so the `test-latex-index`
  differential does not require `INDEX_ENTRY`); `tocdepth`, section numbering
  and some admonition subtrees are flatter. The translator renders the AST it
  receives faithfully.
* **Fixture-specific preambles.** `test-latex-includegraphics` injects a custom
  `latex_elements["preamble"]` that redefines `\sphinxincludegraphics` in terms
  of `\sphinxsafeincludegraphics` (only defined by upstream's `sphinx.sty`);
  the self-contained preamble cannot satisfy that override. Fixtures
  referencing images absent from their source tree (e.g. `logo.jpg`) warn and
  leave the image unresolved.
* **`latex_logo`** is captured in `LatexContext` but not yet wired into a
  `\sphinxlogo` definition in the default preamble.

### 29.9 Tests

`src/latex/{escape,translator}.rs` carry 12 inline unit tests;
`tests/latex_translator.rs` adds 9, `tests/latex_differential.rs` 7 and
`tests/latex_pdf.rs` 3 — **31 new tests**, workspace total **861**, green on
three consecutive runs. `cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` are clean. PDF
compilation is *asserted* (toolchain present), not string-only.
---

## 30. Phase 24 EPUB3, Manpage, Texinfo & Plaintext builders (`scribe-builders`)

### 30.1 Upstream sources inspected

Behavior was extracted from the pinned oracle (not guessed):

* `sphinx/sphinx/builders/{_epub_base,epub3,manpage,texinfo,text}.py` — builder
  config keys, default `man_pages` / `texinfo_documents` tuples, EPUB metadata
  and the `handle_finish()` file order.
* `sphinx/sphinx/writers/{manpage,texinfo,text}.py` — `.TH` header, section
  macros, `.IP`/`.TP`/`.nf` blocks, the Texinfo `TEMPLATE` and `@node` pointer
  scheme, and the text writer's `new_state` / `end_state` frame machine.
* `sphinx/sphinx/templates/epub3/{container.xml,content.opf.jinja,nav.xhtml.jinja,
  toc.ncx.jinja,mimetype}` — exact EPUB container/metadata structure.
* `sphinx/sphinx/util/osutil.py::make_filename_from_project` — EPUB basename.

### 30.2 New public API (all in `crates/scribe-builders/src/`)

| Module | Public surface | Purpose |
|---|---|---|
| `build_common.rs` | `prepare()`, `PreparedBuild` | Shared read phase: environment refresh, parallel parse/resolve (rayon), domain registration, read/resolved transforms, toctree collection. Used by all four builders; does not change HTML/LaTeX behavior. |
| `epub.rs` | `EpubBuilder`, `EpubMetadata`, `NavPoint`, `MIMETYPE`, `EPUB_CSS`, `escape_xml`, `strip_external_refs`, `wrap_xhtml`, `render_nav_xhtml`, `render_toc_ncx`, `write_epub` | XHTML5 emission via the Phase 17 `HTML5Translator` with `.xhtml` target URIs, inlined CSS, external `href`s neutralised; `META-INF/container.xml`, `OEBPS/{content.opf,nav.xhtml,toc.ncx}`, per-doc `.xhtml`, deterministic ZIP. |
| `man.rs` | `ManBuilder`, `ManHeader`, `ManPageSpec`, `Authors`, `parse_man_pages`, `render`, `escape_man` | `.TH`/`.SH`/`.SS`/`.PP`/`.IP`/`.TP`/`.nf`/`.fi` writer, `.B`/`.I`/`.BR`-equivalent font escapes, backslash/hyphen/quote escaping, `man_pages` config parsing. |
| `texinfo.rs` | `TexinfoBuilder`, `TexinfoHeader`, `TexinfoSpec`, `parse_texinfo_documents`, `render`, `escape_texinfo` | Full `@setfilename`/`@titlepage` header, `@node`/`@chapter`/`@section`/`@subsection` with sibling-aware `@menu` pointers, `@example`, `@itemize`, `@table`, `@deftypefn`, `@ref`/`@xref`/`@uref`, `@emph`/`@strong`, `@cindex`, `@bye`. |
| `text.rs` | `TextBuilder`, `TextOptions`, `ToctreeContext`, `render`, `render_with_numbers`, `wrap`, `column_width` | Sphinx state-machine port: wrapped paragraphs (`text_width`, default 80; 70 = oracle `MAXWIDTH`), Unicode-width-aware wrapping, `text_newlines`/`text_sectionchars`/`text_add_secnumbers`/`text_secnumber_suffix`, `text_unicode_tables`, bullet/enum/definition lists, literal indentation, admonitions, box-drawn tables, toctree-derived link lists. |
| `error.rs` | `OutputError`, `OutputResult` | One `thiserror`/`miette` vocabulary (Io, MissingSource, Environment, InvalidConfig, Zip, Xml) tagged with the builder name; CLI diagnostic code `scribe::builder::output_failed`. |

`crates/scribe-cli/src/lib.rs` dispatches `epub`, `epub3`, `man`, `texinfo` and
`text` through `run_output_build`; unknown builders keep the Phase 01
`scribe::unimplemented_builder` diagnostic. New `Config` registry keys are
additive (`text_*`, `man_make_section_directory`, `texinfo_appendices`,
`texinfo_no_detailmenu`, `epub_{uid,publisher,language,basename,description,
contributor,identifier,scheme,version,tocdepth,writing_mode,exclude_files}`).

### 30.3 Fidelity and normalization method

Differential tests run both engines on real oracle fixtures:

* **text** — `test-build-text`, normalized plain text (§30.6).
* **man** — `test-manpage_url`, structural markers plus the `.TH`/`.SH NAME`
  header line; `.SH Author` capitalization is compared case-insensitively.
* **texinfo / epub** — `test-toctree`, structural marker sets for the OPF,
  navbar and `.texi` skeleton plus byte-identical EPUB archives across builds.

Normalization documented for the text differential: CRLF→LF, drop
box-drawing/underline-only lines, collapse all whitespace runs to one space.
It compares the **content token stream**, not byte-exact layout, because the
shared parser's section-number and nested-toctree rendering is still flatter
than Sphinx's fully resolved writer input (see §13/§25 divergences).

### 30.4 Validator gates (real tools, asserted in tests)

* `groff -man -Tutf8 <page>` — exit 0 **and empty stderr** for the generated
  `test-manpage_url` page (`tests/man_writer.rs`).
* `makeinfo --no-split -o /dev/null <file>` — exit 0 **and empty stderr** for
  the generated `test-toctree` `.texi` (`tests/texinfo_writer.rs`).
* `zip -T <archive>` — exit 0; additionally the local file header is parsed to
  assert the first entry is `mimetype`, STORED (method 0), with exactly
  `application/epub+zip`; every `.opf`/`.ncx`/`.xml`/`.xhtml` member is parsed
  with `roxmltree` (`allow_dtd = true`) for well-formedness
  (`tests/epub_builder.rs`).

**`epubcheck` is not installed** in this environment. Structural ZIP + XML
well-formedness is used instead; full EPUB conformance checking remains a
documented gap (no oracle EPUB checksum comparison is performed either —
Sphinx embeds a wall-clock `dcterms:modified` timestamp).

### 30.5 Determinism

EPUB entry ordering is sorted by path, every ZIP entry uses the fixed DOS
timestamp `1980-01-01 00:00:00`, and `mimetype` is always written first and
STORED. Man/Texinfo dates use `today` when configured and a fixed epoch
otherwise. The text builder honours `SOURCE_DATE_EPOCH` indirectly via the
shared config bridge. Two consecutive EPUB builds are asserted byte-identical.

### 30.6 Intentional divergences

* **EPUB** uses document-level nav points from the toctree hierarchy rather
  than Sphinx's full section-anchor nav tree; `nav.xhtml` is emitted with the
  EPUB3 `epub:type="toc"` namespace. CSS is inlined instead of linked, and
  external `href`s are rewritten to `#` to keep the archive offline.
* **Man** emits a compact but valid groff subset; tables are preformatted
  blocks (`.nf`) rather than `tbl` `.TS` regions so plain `groff -man` works.
* **Texinfo** emits one node per section with sibling next/prev pointers;
  chapter titles come from the section's `Title` child only. `toctree_only`
  currently emits an empty body (no appendix assembly).
* **Text** defaults to `text_width = 80` (scribe extension) and ASCII box
  drawing to match Sphinx; set `text_width = 70` for upstream wrap fidelity
  and `text_unicode_tables = 1` for Unicode box drawing.
* **Phase 06 remediation:** `seealso` now accepts one optional whitespace
  argument (Docutils `BaseAdmonition` behavior) so `.. seealso:: text` from the
  text fixture produces a paragraph instead of an unresolved `Pending` node.
  This is additive and leaves the frozen AST / env / domain shapes untouched.
* Section-number injection and toctree rendering reuse the Phase 09 collector
  via `env.record_document` + `collect_toctrees_with_sources`; no frozen
  `BuildEnvironment` field was added.

### 30.7 Tests

`tests/{text_writer,man_writer,texinfo_writer,epub_builder}.rs` add **34 new
tests** (13 + 7 + 7 + 7). Workspace total **895**, green on three consecutive
runs. `cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` are clean. Frozen §7
AST, §12 `BuildEnvironment` and §15 `Domain` shapes are untouched. No commits
were created.
---

## 31. Phase 25 linkcheck, XML, pseudoxml & changes builders (`scribe-builders`)

### 31.1 Upstream sources inspected

Behavior was transcribed from the pinned checkout/venv (not guessed):

* `sphinx/sphinx/builders/linkcheck.py` — `_Status` enum,
  `HyperlinkCollector` (reference/image/raw URI collection, dedup by URI),
  `HyperlinkAvailabilityChecker.check/is_ignored_uri`, `_check` (non-network
  conditions for `#`/`mailto:`/scheme-less/local paths), `_retrieval_methods`
  (HEAD first, GET for anchors), `_check_uri` (Retry-After, 401/429/503,
  redirects, `contains_anchor`, normalized URL comparison), `limit_rate`,
  `_get_request_headers`, `AnchorCheckParser` (`id`/`name` start-tag
  attributes), `write_entry` / `write_linkstat` report formats, and the full
  `linkcheck_*` config default set.
* `sphinx/sphinx/builders/xml.py` — `XMLBuilder` / `PseudoXMLBuilder`;
  `write_doc` injects `translation_progress` and `xmlns:<domain>` attributes;
  `_translate` sets `xml_declaration` / `doctype_declaration` and `xml_pretty`.
* `.oracle-venv/.../docutils/writers/docutils_xml.py::XMLTranslator` —
  declaration, DOCTYPE, generator comment, `simple_nodes` /
  `FixedTextElement` / `Inline` formatting rules and `visit_Text` escaping.
* `.oracle-venv/.../docutils/nodes.py::starttag` — sorted `attlist()`,
  `serial_escape` for list values, `xml.sax.saxutils.quoteattr`.
* `sphinx/sphinx/builders/changes.py` and
  `sphinx/sphinx/themes/basic/changes/{versionchanges,frameset,rstsource}.html`
  — change grouping, entry HTML, source-frame highlighting and exact template
  layout. `sphinx/sphinx/domains/changeset.py` supplies `ChangeSet` and the
  version-label strings.

### 31.2 Public API

| Item | Purpose |
|---|---|
| `XmlBuilder` / `PseudoXmlBuilder` | `-b xml` / `-b pseudoxml`; one `<docname>.xml` / `.pseudoxml` per document via the shared `build_common::prepare` read phase. |
| `render_xml(&Document, pretty) -> Result<String, XmlRenderError>` | Additive real-XML translator (the frozen §7 `to_xml()` is a simpler optional helper). |
| `XMLNS_DOMAINS`, `DOCTYPE` | Upstream's injected domain namespaces and Docutils DOCTYPE. |
| `LinkcheckBuilder` | `-b linkcheck`; returns `LinkcheckReport`. |
| `collect_hyperlinks`, `Hyperlink` | Deterministic URI collection from `reference` / `image` / `raw` nodes and internal `refid` links. |
| `LinkcheckConfig::from_config`, `LinkStatus`, `CheckResult`, `LinkcheckReport` | Config snapshot, upstream status vocabulary, JSON report row and aggregate counts. |
| `is_ignored`, `prefix_match`, `unquote`, `contains_anchor`, `retry_after_delay`, `write_linkcheck_report` | Pure helpers (regex prefix matching mirrors Python `re.match`; conservative HTML anchor scanner; Retry-After parsing; Sphinx-format `output.txt`/`output.json`). |
| `ChangesBuilder`, `ChangeSet`, `ChangesContext`, `Entry` | `-b changes`; `changes.html` + `index.html` + `rst/<docname>.html` grouping. |
| `collect_changesets(_with_sources)`, `render_changes_html`, `render_index_html`, `render_rst_source`, `highlight_source` | Pure collection/rendering helpers. |

CLI dispatch adds `linkcheck`, `xml`, `pseudoxml` and `changes` to
`scribe build -b`. A broken/timeouts linkcheck report surfaces the structured
miette diagnostic `scribe::linkcheck::broken_links` (non-zero exit) when `-W`
is set; without `-W` the build succeeds after writing the report.

### 31.3 Real XML translator

`render_xml` reproduces `XMLTranslator` structurally: declaration, DOCTYPE,
`<!-- Generated by Docutils 0.23 -->`, 4-space indentation when `xml_pretty`
(default true), sorted attributes through `serial_escape` + SAX `quoteattr`
(single quotes when the value contains `"` but no `'`), inline formatting for
Docutils `TextElement` / `meta` / `image` / `colspec` / `transition`, no
indentation increase for `Inline` nodes, and verbatim newlines inside
`FixedTextElement` / `literal`. The document root gets `source` (absolute
path), `translation_progress` and the ten `xmlns:*` attributes at build time,
so the emitted files match the oracle byte-for-byte apart from the source path
and generator version comment (normalized in tests).

### 31.4 Concurrency, determinism and anchoring

Link checking uses one `tokio` task per URI, a `reqwest::Client` with
connection pooling and a `Semaphore` per domain (permits =
`linkcheck_workers`), plus an optional `Retry-After` back-off per domain.
HEAD is tried first and GET is used when an anchor must be inspected or the
HEAD request fails. Redirects are followed manually (max 10 hops) so the last
redirect status code is available for the `redirected` report row. Results are
awaited in collection order and finally sorted by `(filename, lineno, uri)`,
making `output.json` deterministic. Internal `#anchor` / `refid` links and
scheme-less local paths are verified without network access via the parsed
doctrees and the source tree.

### 31.5 Differential method

* `tests/xml_differential.rs` runs the real oracle `-b xml` and `-b
  pseudoxml` on `test-basic` and asserts byte equality after normalizing only
  the absolute `source` path and the Docutils version comment. XML is also
  parsed with `roxmltree` (`allow_dtd`) as a well-formedness gate.
* `tests/changes_differential.rs` runs the oracle `-b changes` on
  `test-changes` and compares `changes.html`, `index.html` and all
  `rst/*.html` files byte-for-byte. The fixture exercises hyphenated and
  canonical change directives, C-API grouping, module-level grouping and the
  empty-content label case. Source-derived absolute lines and `py:module`
  context are recovered from the raw sources because nested directives in the
  frozen AST carry content-relative lines.
* `tests/linkcheck_differential.rs` starts an ephemeral tokio HTTP server
  serving good/anchor/redirect/broken/ignored paths, builds the same fixture
  with scribe and the oracle and compares each URI's `status` from
  `output.json`. **No external network is used**, so the test is offline and
  deterministic. Additional tests assert report ordering, JSON field order,
  per-domain permit sharing, Retry-After parsing and no-panic behavior on
  malformed URLs/network errors.

### 31.6 Intentional divergences / risks

* Upstream linkcheck always exits with status 1 when broken links are found.
  Scribe gates that on `-W` per the Phase 25 contract; the report files are
  written in both cases.
* Anchor checking uses a conservative start-tag attribute scanner rather than
  `html.parser`: it never misses a genuine `id=`/`name=` attribute but can
  accept an anchor appearing only in text or JavaScript.
* `linkcheck_allowed_redirects` / auth / request-header configuration is
  parsed and applied, but only the typed config subset registered in
  `scribe-config` is available (complex Python tuple values degrade to string
  lists from the static conf bridge).
* `linkcheck_ignore` and all other linkcheck regexes use Python `re.match`
  prefix semantics (`prefix_match`), not `re.search`.
* `changes_titles_*` keys do not exist in the pinned oracle; they are ignored.
  `linkcheck_anchor_re` is registered as an empty list for compatibility with
  older Sphinx configurations but is unused by the pinned oracle (which has
  no such setting).
* CSS files emitted by the changes builder are deterministic placeholders,
  not verbatim copies of the Sphinx theme stylesheets; they are not part of
  the byte-exact differential comparison.
* Internal `#anchor` / `refid` and scheme-less local links are verified against
  the parsed doctrees and source tree (no network); external HTTP anchors use
  the HTML attribute scanner described above.
* The frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` public shapes are
  untouched. Additions are confined to new builder modules, the shared read
  pipeline's upstream directive aliases, and new config keys.

### 31.7 Tests and gates

`tests/{xml_differential,changes_differential,linkcheck_differential}.rs`
plus unit tests in the three modules add **25** new tests (workspace total
**920**). `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings` and three consecutive
`cargo test --workspace` runs (920 passed each) are green. No commits were
created.
---

## 32. Phase 26 Internationalization (gettext builder & `.mo` pipeline)

### 32.1 Upstream sources inspected (behavior extracted, not guessed)

* `sphinx/sphinx/builders/gettext.py` — `Message`, `Catalog`, `MsgOrigin`,
  `I18nBuilder.write_doc`, `should_write` (header-preservation) and
  `MessageCatalogBuilder.finish`; the POT header variables and `gettext_*`
  config registrations.
* `sphinx/templates/gettext/message.pot.jinja` — the exact POT header block and
  the `e` escape filter (`\` → `\\`, `"` → `\"`, `\n` → `\\n"\n"`).
* `sphinx/sphinx/util/nodes.py` — `is_translatable`, `IGNORED_NODES`,
  `LITERAL_TYPE_NODES`, `extract_messages`, `get_node_source` and
  `_clean_extracted_message`.
* `sphinx/sphinx/util/i18n.py` — `CatalogInfo` (`po_path`/`mo_path`,
  `is_outdated`, `write_mo`), `CatalogRepository` (`locale_dirs`, `pofiles`)
  and `docname_to_domain`.
* `sphinx/sphinx/transforms/i18n.py` — `PreserveTranslatableMessages`,
  `Locale` (two-phase translation, `translated` attribute),
  `TranslationProgressTotaliser`, `AddTranslationClasses`,
  `RemoveTranslatableInline`.
* `sphinx/sphinx/transforms/__init__.py::_calculate_translation_progress` —
  the `|translation progress|` percentage format.
* `babel.messages.mofile.write_mo` (oracle venv, inspected via `inspect.getsource`)
  — the binary layout: 7×u32 LE header, no hash table, NUL-terminated strings
  whose NUL is not counted in the length, key index before value index.

### 32.2 Crate layout and public API

**`scribe-core`** gains `i18n` (no file-format code):

| Item | Purpose |
|---|---|
| `TranslationCatalog` | In-memory `domain` + deterministic `BTreeMap<msgid, msgstr>`; `get`, `contains`, `insert`, `entries`. |
| `TranslationProgress` | `{total, translated}`; `format()` reproduces Sphinx's `f'{x:.2%}'` / "no translated elements!"; `store_on_document` / `from_document` use two additive integer attributes on the frozen root. |
| `ExtractedMessage { msgid, source, line, node }` | One extracted message with its location and originating `NodeId`. |
| `extract_messages`, `is_translatable`, `is_in_substitution_definition`, `clean_extracted_message`, `apply_locale` | Frozen-AST port of upstream's extraction / translation helpers. |

`TransformContext` gains an additive `translations: Option<&TranslationCatalog>`
field plus `with_translations()`. The Phase 10 seams are now live:
`PreserveTranslatableMessages` (10) records `__original_message` on explicitly
translatable nodes; `Locale` (20) applies the catalog; `TranslationProgressTotaliser`
(25) persists progress; `AddTranslationClasses` (950) appends `translated` /
`untranslated` classes from `translation_progress_classes`;
`RemoveTranslatableInline` (999, read + post) unwraps placeholder `Inline`
nodes. `DefaultSubstitutions` now resolves `|translation progress|` from the
stored progress instead of returning the Phase 10 placeholder string.

**`scribe-builders::i18n`** owns the file formats and discovery:

| Module / item | Purpose |
|---|---|
| `po::{Message, PoCatalog, PotContext}` | Defensive PO parser (comments, locations, flags, plurals, quoted continuations), deterministic POT emitter matching upstream's template, `.po` merge preserving `msgstr`/flags/comments/header, `should_write` timestamp preservation. |
| `mo::{write_mo, read_mo, MoError}` | Dependency-free GNU binary MO writer/reader; no hash table; validates magic/revision/counts/offsets and returns structured errors on malformed input. |
| `catalog::{docname_to_domain, catalog_paths, is_outdated, compile_catalog, compile_all, load_catalog, locale_dirs}` | Locale discovery, `gettext_compact` handling, mtime-based `.po` → `.mo` compilation and catalog loading for the configured language/domain. |
| `builder::{GettextBuilder, GettextResult}` | `-b gettext`: shared read pipeline → per-domain message extraction → `.pot` emission (+ in-place update of existing `.po`) → template message extraction. |

Config keys registered in `scribe-config`: `gettext_compact`, `gettext_location`,
`gettext_uuid`, `gettext_auto_build`, `gettext_additional_targets`,
`gettext_last_translator`, `gettext_language_team`,
`gettext_allow_fuzzy_translations`. The CLI dispatches `-b gettext` to
`GettextBuilder`; the HTML, LaTeX and shared output pipelines call `compile_all`
and attach the per-document catalog to `TransformContext` before applying the
read/resolved passes.

### 32.3 Data flow and determinism

1. `gettext` build: parse + resolve every source through `build_common::prepare`
   with `builder_name = "gettext"` (so the translation transforms stay inert),
   extract messages with `scribe_core::extract_messages`, skip descendants of
   substitution definitions, and group by `docname_to_domain`.
2. POT entries are emitted in preorder/first-seen order (Python dict semantics);
   `BTreeMap<String, PoCatalog>` fixes domain order. Locations use
   `os.path.relpath`-style relative source paths and the node's recorded line.
3. `should_write` skips rewriting an unchanged POT so `POT-Creation-Date` is
   stable across builds, matching upstream. `SOURCE_DATE_EPOCH` is honored for
   reproducible timestamps; otherwise local time is used (as upstream does).
4. Non-gettext builds recompile `.mo` files only when the `.po` is newer (or
   the `.mo` is absent), then load the catalog for the current document's
   domain. `Locale` parses each non-empty `msgstr` with the shared RST parser
   (upstream's `_publish_msgstr`) and transplants the first paragraph's inline
   children into the original node, preserving its frozen kind/ids/attributes.
5. MO output is byte-deterministic: entries sorted by `msgid`, no hash table,
   and all table offsets derived from entry lengths. Malformed PO/MO bytes are
   reported as `MoError`/warnings and never panic.

### 32.4 Differential method and normalization

`crates/scribe-builders/tests/i18n_differential.rs`:

* **Byte-exact POT parity** on `test-gettext-template` (`index.pot` and
  `sphinx.pot`): both engines are run with `-b gettext -E`; the only
  normalization is replacing the `POT-Creation-Date` value with `<TIMESTAMP>`.
  This covers the header, escaping, location comments (line numbers included),
  message ordering and template `_('…')` / `{% trans %}…{% endtrans %}`
  extraction.
* **Message-set parity** on `test-intl` with `-D gettext_compact=0`: header
  fields are compared exactly and every oracle `msgid` in the curated plain-text
  domains (`admonitions`, `translation_progress`) must be present in scribe's
  output. Version is supplied explicitly because the Phase 02 static conf.py
  bridge does not parse chained assignments.
* **Babel → scribe round-trip**: a small `.po` is compiled to `.mo` with the
  oracle's Babel, then scribe builds localized HTML. The assertions check that
  heading, paragraph, admonition title and admonition body translations appear.
  A companion test deletes the `.mo` so scribe's own compiler must create it.
* **Malformed catalogs** (invalid UTF-8 PO, garbage MO) degrade to untranslated
  source text without panicking.

`crates/scribe-core/tests/i18n_transforms.rs` adds 15 transform-level tests
(extraction, each activated seam, progress metadata/serialization, malformed
translations). Unit tests in `scribe-core::i18n` and `scribe-builders::i18n`
cover catalog lookup, escaping round-trips, plural entries, MO write/read (including
reading a Babel-generated fixture), header preservation and PO merge.

### 32.5 Intentional divergences / risks

* **Message text**: upstream extracts `node.rawsource` for text elements; the
  frozen §7 AST has no `rawsource`, so scribe extracts cleaned rendered text
  (`astext()`). Messages containing inline markup (e.g. `` `link`_ `` or
  `|translation progress|`) therefore have different msgids. Plain-text
  messages — the overwhelming majority — are identical.
* **Line numbers**: docutils attributes a section title to its adornment line,
  while scribe records the title text line; POT `#:` lines can differ by one or
  two for headings. The byte-exact fixture avoids numbered headings.
* **Numbered headings**: `test-intl/section.txt` (`1. Section`) is parsed by
  Phase 04 as an enumerated-list prefix plus heading, producing extra POT
  messages. Excluded from the message-set differential; parser-level, out of
  Phase 26 scope.
* **Multi-paragraph `msgstr`**: only the first parsed block's inline children
  are transplanted into the target node; subsequent paragraphs are ignored.
* **`translatable` addnodes**: the frozen `NodeKind` has no `translatable` node;
  `PreserveTranslatableMessages` and `RemoveTranslatableInline` operate on the
  additive `translatable` attribute instead.
* **`gettext_uuid`**: the config key is registered, but scribe never emits
  random UUID location comments — those would break deterministic POT output.
  `#: file:line` locations are always used when `gettext_location` is enabled.
* **Catalog fallback chain**: upstream merges a chain of fallback catalogs;
  scribe loads the first available `.mo` per domain. Locale directories are
  searched in configured order.
* **Template extraction** recognizes `_('…')` and `{% trans %}` blocks only;
  arbitrary Jinja i18n constructs (plural `trans`, variable interpolation) are
  out of scope.
* **`.po` update** is an additive scribe behavior: upstream writes only `.pot`
  files. Scribe additionally updates an existing `.po` in the output or locale
  directory, preserving translations and header fields.
* The frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` public shapes are
  untouched. Additions are new modules, additive context/attribute fields, new
  config keys and CLI dispatch.

### 32.6 Tests and gates

`cargo check --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` are clean; three
consecutive `cargo test --workspace` runs are green (**963 passed** each: 920
prior + 43 new). No commits were created.
---

## 33. Phase 27 third-party Python & Wasm extension interop bridge (`scribe-core`)

Phase 27 adds the extension host seam that Phase 26 documented but did not
implement. All work is **additive** in `crates/scribe-core` (new modules
`events.rs`, `plugin.rs`, `python_shim.rs`, `bridge.rs`), a small optional field
on `scribe_builders::html::BuilderOptions`, three new `scribe-config` keys and
CLI wiring. The frozen §7 AST, §12 `BuildEnvironment` and §15 `Domain` shapes
are **unchanged**.

### 33.1 Event bus — `scribe_core::events`

`EventManager` is a cloneable, `RwLock`-backed bus mirroring upstream
`sphinx.events.EventManager`:

| API | Upstream counterpart / behaviour |
|---|---|
| `new()` | pre-registers [`CORE_EVENTS`] (traced from `sphinx/events.py`, plus scribe's `html-page-context` / `write-done`). |
| `add_event(name)` | `EventManager.add` — duplicate names return `EventDiagnostic::EventAlreadyPresent`. |
| `connect(name, handler, priority) -> ConnectionId` | `connect` — lower priorities first, ties broken by monotonic connection id. |
| `disconnect(id)` | `disconnect` — removes exactly one handler; unknown ids are a no-op. |
| `emit(name, &Event) -> Vec<HandlerResult>` | `emit` — every handler runs; results collected in priority order. |
| `emit_firstresult(name, &Event) -> Option<Json>` | `emit_firstresult` — stops at the first `Ok(Some(..))`. |
| `take_diagnostics()` / `diagnostics()` | structured, miette-renderable contained failures. |

**Typed payloads.** `Event` is an enum with one variant per core event
(`ConfigInited`, `BuilderInited`, `EnvGetOutdated`, `EnvGetUpdated`, `EnvUpdated`,
`SourceRead`, `DoctreeRead`, `DoctreeResolved`, `MissingReference`,
`WarnMissingReference`, `HtmlPageContext`, `WriteDone`, `BuildFinished`,
`Custom`). Upstream's mutable `source` / page-context lists are represented by
interior-mutable `SharedString = Arc<Mutex<String>>` / `SharedJson =
Arc<Mutex<Json>>`, so a handler can mutate the source exactly as upstream does.

**Containment.** A handler returning `Err(HandlerError)` or unwinding is caught
(`catch_unwind`), recorded as `EventDiagnostic::HandlerFailed` and surfaced as a
build warning. A plugin can never abort a build. `EventManager` is `Send + Sync`
and shares state across clones.

### 33.2 Build-pipeline integration

`InteropBridge` owns the bus, live Python shims, the Wasm host and third-party
directives. The CLI constructs one bridge per build from config and attaches it
to `scribe_builders::html::BuilderOptions::bridge` (`None` keeps the old,
byte-identical behaviour). Events are emitted at upstream's points:

| Event | Builder location |
|---|---|
| `config-inited`, `builder-inited` | start of `build_with_flavor` / `build_common::prepare`. |
| `env-get-outdated`, `env-get-updated`, `env-updated` | immediately after `env.update()`; returned docnames join the rebuild set. |
| `source-read` | after reading each source, **before** RST parsing; mutations are applied. |
| `doctree-read` | after parsing and the Phase 20 read hooks. |
| `doctree-resolved` | after the read/resolved transform passes, before writing. |
| `missing-reference`, `warn-missing-reference` | for each unresolved reference in the read pass. |
| `html-page-context` | after `page_context()`; context mutations are applied to the render context. |
| `write-done`, `build-finished` | after all files are written / at build end. |

Phase 20 built-in extensions keep their existing direct calls (§26). Moving
`&mut Document` through a JSON event payload cannot mutate the frozen §7 AST, so
a switch to bus-driven execution would change behaviour; the differential test
`built_in_extension_output_is_identical_with_and_without_event_bus` proves the
bus is additive. Their logical hook points are now mirrored by emitted events,
which third-party handlers can observe.

### 33.3 Wasm plugin ABI — `scribe_core::plugin`

The ABI (version 1) is a JSON-over-linear-memory convention with host functions
`HOST_LOG`, `HOST_DIAGNOSTIC`, `HOST_CONFIG_GET`, `HOST_AST_GET`,
`HOST_ADD_DIRECTIVE`, `HOST_ADD_ROLE`, `HOST_CONNECT_EVENT` (module `scribe`) and
exported callbacks `on_config_inited`, `on_builder_inited`, `on_source_read`,
`on_doctree_read`, `on_doctree_resolved`, `on_directive`, `on_role`. A plugin
ships a JSON `PluginManifest` (ABI version, name, capabilities, callbacks) in a
`scribe.plugin` custom section or as a standalone file. A Rust/C-compatible
example plugin source is at `crates/scribe-core/examples/wasm_plugin.rs`.

The `wasm-plugins` Cargo feature keeps default builds lightweight:

* **off (default)** — `WasmPluginHost::is_available() == false`, `load_bytes` /
  `invoke` return `PluginError::FeatureNotCompiled`; malformed bytes never
  panic. No `wasmtime`/`extism` dependency is pulled in.
* **on** — a self-contained manifest + native-trampoline backend is compiled:
  a trivial plugin can be loaded from its manifest, given a native callback and
  invoked, and `connect_to_events` wires its declared events to the bus.
  Bytecode interpretation is a documented future backend.

### 33.4 Python extension shim — `scribe_core::python_shim`

`PythonExtensionShim` launches a configured interpreter (`-c <script>` or a
script path) and speaks **line-delimited JSON-RPC 2.0** (`initialize`,
`on_event`, `directive`, `role`, `shutdown`). `initialize` returns the events,
directives and roles the extension provides; `connect_to_events` wires each
subscribed event (including in-place `source-read` mutation) and `directive()`
returns a `Directive` adapter that delegates `run` to the worker.

Config keys: `scribe_python_extensions` (script paths), `scribe_python`
(interpreter, default `python3`), `scribe_wasm_plugins` (plugin paths). A
missing interpreter, worker exit or malformed JSON disables the shim with a
warning — `scribe build` never depends on Python being present.

### 33.5 Tests and risks

New tests: `crates/scribe-core/tests/events.rs` (7), `python_shim.rs` (5),
in-module unit tests (events 9, plugin 3/5, shim 2, bridge 2),
`crates/scribe-builders/tests/interop.rs` (5, including a real build with a
Python extension, a byte-for-byte built-in-extension differential and an
"every core event fires" assertion), and `crates/scribe-cli/tests/interop.rs`
(2 end-to-end `scribe build` invocations). Workspace total **998 tests**.

Risks / limitations: Wasm bytecode execution is not implemented (ABI +
manifest/native backend only); the Python shim is synchronous and uses one
worker per configured script; `html-page-context` only applies scalar context
keys; third-party roles are declared by the protocol but have no RST inline
adapter yet; built-in extensions remain direct calls for §7 fidelity.


---

## 34. Phase 28 live-reload dev server (`scribe serve` / `scribe watch`)

Phase 28 adds an embedded HTTP server, a filesystem watcher and browser live
reload to the CLI. It consumes the frozen Phase 08 `BuildEnvironment` and the
Phase 17 HTML builder without changing either public shape; the frozen §7 AST
and §15 `Domain` traits are untouched.

### 34.1 Upstream conventions inspected

`python -m sphinx.autobuild` / `sphinx-autobuild` (the de-facto Sphinx dev-server
convention) was the behavioral reference: watch SOURCE_DIR recursively,
debounce rapid changes, rebuild the project, and reload the browser only after
a successful build. Sphinx itself has no built-in `serve` command, so there is
no oracle builder to diff against; differential focus is on **rebuild
correctness** (§34.6) rather than server output bytes.

### 34.2 New dependencies and crate layout

Declared in `[workspace.dependencies]` first, then opted into by
`scribe-cli`:

* `notify = "8"` — cross-platform filesystem watcher (`recommended_watcher`,
  `RecursiveMode::Recursive`).
* `tokio` extended with `fs`, `net`, `io-util`, `sync`, `signal` for the async
  server, debounce timer and Ctrl-C handling.
* `serde_json` for the SSE payload (already a workspace dependency).

All serving logic lives in `crates/scribe-cli/src/serve/`:

| Module | Responsibility |
|---|---|
| `mod.rs` | `ServeArgs` (clap), `ServeError` (miette), watcher + debounce loop, rebuild orchestration, Ctrl-C shutdown |
| `mapping.rs` | Pure path→docname mapping, event-kind filtering, change classification, standalone `Debouncer` |
| `mime.rs` | Deterministic MIME table for Sphinx assets (`text/html; charset=utf-8`, CSS, JS, images, fonts, `objects.inv`, …) |
| `http.rs` | Minimal `tokio::net::TcpListener` HTTP/1.1 server: GET/HEAD, directory index, 404/403, health endpoint, SSE stream |
| `inject.rs` | Live-reload browser client (`LIVE_SCRIPT`) and HTML-only `<script>` injection |

The pure helpers are public (`scribe_cli::serve::{mapping, mime, inject, http}`)
so they are unit-testable without starting a server.

### 34.3 CLI surface

```
scribe serve SOURCE_DIR OUTPUT_DIR [--host HOST] [--port N] [--builder NAME]
            [-c CONFDIR] [-D key=value...] [--no-reload] [--delay MILLIS] [-q]
scribe watch ...            # visible alias for `serve`
```

* Defaults: `--host 127.0.0.1`, `--port 8000`, `--builder html`, `--delay 150`.
* `--port 0` asks the OS for an ephemeral port; the resolved address is printed
  as `scribe: serving at http://HOST:PORT (Ctrl-C to stop)` (flushed) so tests
  and tools can discover it without parsing heuristics.
* `-D/-c/-b` feed the existing `build_config` / `html::BuildOptions` path, so
  `serve` builds exactly what `scribe build -b html` would.
* `--no-reload` disables both injection and the `/events` endpoint (which then
  returns 404).
* Errors are typed miette diagnostics: `scribe::serve::missing_source`,
  `bind_failed`, `watch_failed`, `build_failed`, `unsupported_builder`,
  `config_failed`. Port conflicts and missing source dirs exit non-zero with a
  clear message — never a panic.
* Ctrl-C (`tokio::signal::ctrl_c`) broadcasts shutdown to the server task and
  exits 0.

### 34.4 Watch → rebuild loop

1. `notify::recommended_watcher` watches `SOURCE_DIR` (and the conf dir when
   distinct, recursive). **Critical:** `notify` on Linux reports
   `Access(Open(..))` when a file is merely read. The builder reads every source
   during a rebuild, so reacting to Access events creates an infinite rebuild
   loop. `mapping::is_relevant_event` filters `Access`/`Other`, keeping only
   create/modify/remove/rename. This is covered by a unit test.
2. Relevant events feed an async debounce loop (`tokio::time::sleep_until`,
   default 150 ms) that coalesces a burst into one batch. The standalone
   `Debouncer` implements the same policy with logical millisecond time for
   deterministic tests.
3. Each path is classified with `mapping::classify_path`: a source suffix
   (from `Config::source_suffixes()`, defaulting to `.rst`) maps to a docname;
   `conf.py`/`scribe.toml`, templates and static assets set a `force` flag;
   reserved `_`/dot paths are ignored.
4. The authoritative minimal rebuild set is computed by **Phase 08
   `BuildEnvironment::update`** (`rebuild_set` in `mod.rs`, `persist: false`
   so probing does not disturb the on-disk cache). Include/toctree/config and
   `extra_outdated` invalidation are inherited exactly — no incremental logic
   is reimplemented. Removed docs are deleted from the output directory.
5. A batch whose rebuild set is empty and which has no `force` flag is skipped
   (a touch that does not change content therefore does nothing).
6. The HTML-family builder (`StandaloneHtmlBuilder`, `DirectoryHTMLBuilder` or
   `SingleFileHTMLBuilder`) is then invoked, reusing the in-process environment
   that the builder itself refreshes from `OUTPUT_DIR/.doctrees`. Status lines
   mirror sphinx-autobuild: `[scribe] rebuild <docnames> in <elapsed>`, and a
   reload broadcast is sent **only after a successful rebuild**. Build failures
   are reported on stderr and the server keeps watching.

**Design decision — project-wide artifacts.** Sphinx HTML output contains
project-scoped files (`searchindex.js`, `genindex.html`, `objects.inv`,
navigation/prev-next links, theme assets). Rewriting only a subset of pages
while leaving those artifacts untouched yields an inconsistent site (e.g. a
search index that omits unchanged pages). The dev server therefore re-runs the
**full deterministic builder** on each debounced change, with two incremental
guarantees: (a) the Phase 08 minimal set gates *whether* a rebuild happens and
is what is reported/broadcast, and (b) unchanged source reads are skipped by
the Phase 08 cache inside the builder. Generated bytes are identical to a
fresh `scribe build -b html`, which makes the differential gate (§34.6) exact.
True per-page HTML writes with a project-wide artifact cache are a documented
future optimization; the incremental *decision* logic is fully Phase 08's.

### 34.5 HTTP server and live reload

`http::HttpServer` binds `(host, port)` and serves `OUTPUT_DIR`:

* `GET /` and any directory → `index.html`; `GET /index.html` → the page.
* Correct MIME types from `mime.rs`; CSS/JS/JSON/images are served byte-exact.
* 404 for unknown paths, 403 for traversal attempts (`..` rejected after
  percent-decoding), `GET /_scribe_health` → `200 ok`.
* Live reload uses **SSE** rather than WebSocket (a documented fallback in the
  spec): `inject::LIVE_SCRIPT` opens `EventSource('/events')` and listens for
  `event: scribe-reload`, whose payload is the list of changed HTML page paths.
  The client reloads only when the current page is in that list (or when the
  list is empty, e.g. a config/asset change), so an edit to `other.rst` does
  not reload a tab showing `index.html`. Errors are ignored so a server restart
  is silent.
* The script is served at `/_static/scribe-live.js` and injected as a single
  `<script src="..." data-scribe-live></script>` tag immediately before
  `</body>`, **at response time only**. Generated files on disk stay
  byte-identical to a fresh build; injection is idempotent and never applied
  to non-HTML content types.

### 34.6 Tests and differential method

`crates/scribe-cli/tests/serve.rs` spawns the real `scribe` binary with
`--port 0`, discovers the port from stdout, and synchronizes with bounded
polling (no fixed sleeps):

* served `index.html` contains generated text and the injected tag; `/` and
  directory indexes resolve; CSS is not instrumented;
* missing pages → 404, traversal → 403/404;
* editing a `.rst` file causes the served page to contain the new text;
* an SSE client receives `scribe-reload` after a successful rebuild;
* `--no-reload` removes injection and disables `/events`;
* **differential:** after an incremental rebuild, the served page with the
  injected tag stripped is compared byte-for-byte with an independent
  `scribe build -b html -E` output;
* performance smoke: one-page rebuild + HTTP refresh on a small fixture is
  timed (informational, generous 15 s ceiling);
* SIGINT exits cleanly (status 0); port conflict and missing source produce
  clear non-zero errors.

Unit tests (in `src/serve/`) cover MIME mapping, injection and non-injection,
path→docname mapping, event-kind filtering (Access events), change
classification, debounce coalescing, argument parsing (including the `watch`
alias), and the builder-option mapping.

**Fixture pitfall (documented for maintainers):** integration tests must keep
the child's stdout pipe *open and draining* for the child's lifetime. Dropping
the `BufReader` after reading the address closes the pipe and the server panics
with exit code 101 on its next `println!` (which superficially looks like a
shutdown bug). The test helper drains both stdout and stderr on background
threads.

**Performance note:** a one-page rebuild on a three-document fixture completes
in roughly 20 ms (including full deterministic rendering); timing is reported
by the smoke test and is not a hard gate.

**Risks / deferred:** per-page HTML writes with an incremental global-artifact
cache (§34.4); WebSocket transport (SSE used instead, per spec fallback); no
oracle `serve` command exists, so differential testing targets rebuild
correctness; config changes that do not affect `RelevantConfig` do not
invalidate the environment (matching upstream `CONFIG_OK`).

---

## 35. Phase 29 upstream conformance harness & fuzz coverage matrix

Phase 29 adds **measurement infrastructure**, not new engine features: an
automated conformance harness that builds a documented subset of the pinned
upstream fixtures with both the oracle and `scribe`, and a cross-crate fuzz /
regression-guard campaign. The frozen §7 AST, §12 `BuildEnvironment` and §15
`Domain` public shapes are untouched.

### 35.1 Conformance harness design

Location: `crates/scribe-cli/tests/conformance.rs`; committed baseline
`crates/scribe-cli/tests/fixtures/conformance_baseline.txt`; generated report
`CONFORMANCE.md` at the workspace root.

Flow:

1. **Enumerate + select.** `FIXTURES` is a static, documented list of 39
   fixture/builder pairs drawn from `sphinx/tests/roots/` (178 roots total).
   Selection criteria: (a) deterministic offline builds; (b) coverage of every
   parser/directive family (markup & inline, lists/tables, footnotes/citations,
   admonitions, code/highlighting, include/raw/only, toctree, glossary, themes);
   (c) coverage of every implemented domain (py/c/cpp/js) plus xref warnings;
   (d) `pseudoxml` for doctree structure and `html` for DOM markers; (e) a build
   count small enough for regular CI. Five fixtures are pre-marked `skip` with
   recorded reasons (network, external `dot` binary, import-heavy extension,
   and two roots whose upstream testroot copy step is required to build them).
2. **Bounded parallel builds.** A `rayon::ThreadPoolBuilder` capped at
   `clamp(available_parallelism, 2, 8)` evaluates fixtures in parallel. Each
   fixture runs two subprocesses through a hard 90 s timeout implemented with
   `Child::try_wait` + poll (killing the child on expiry so no process leaks):
   - oracle: `sphinx-build -b <builder> -E -q -n SOURCE OUT`
   - scribe: `scribe build -b <builder> -E -q -n SOURCE OUT`
3. **Capture + normalize.** Exit status, stderr and the recursively collected
   generated files (extension `.pseudoxml` or `.html`) are captured. Warning
   lines are normalized by replacing absolute source/output paths with
   `<ROOT>`/`<OUT>` and stripping the `scribe: warning:` prefix.
4. **Semantic signature.** A small hand-written scanner extracts a multiset of
   element tag names from all generated files (`<document>`, `<section>`,
   `<desc_signature>`, `<div>`, …). No regex dependency is introduced.
5. **Rubric & score.** `structure = multiset-Jaccard(oracle_tags, scribe_tags)`;
   `warnings = set-Jaccard(normalized oracle/scribe warning lines)`;
   `score = 0.85 * structure + 0.15 * warnings`. Status thresholds: `pass ≥
   0.90`, `partial ≥ 0.50`, else `fail`. Aggregate parity is the mean score
   over non-skipped fixtures. Divergences are categorized by inspecting the
   symmetric difference of tag keys (`parser`, `domain`, `directive`,
   `html classes`) plus a `warning codes` category when warning sets differ.
6. **Determinism.** The report formatter sorts fixtures internally; the test
   asserts that formatting the results in reverse order yields byte-identical
   output and that the written `CONFORMANCE.md` round-trips. No timestamps or
   machine-specific paths enter the report.

### 35.2 Tracked metric & regression guard

Per the orchestrator's locked decision, parity is a **tracked metric**, not a
hard gate: the conformance test never fails merely because parity is below 95%.
It fails only if the harness cannot run, the report is non-deterministic, the
baseline is missing/incomplete, or a fixture regressed. The committed baseline
records one `(fixture, builder, score, status)` row per fixture. A regression is
(a) a status drop (`pass → partial/fail`, `partial → fail`) or (b) a score drop
greater than 0.15, with `skip` treated as neutral. This makes parity a visible,
honest number while guarding against silent erosion.

Measured on this checkout (`sphinx-build 9.3.0+/035a07c32`):
**65.6 %** over 34 applicable fixtures (3 pass, 25 partial, 6 fail, 5 skip).
The full per-fixture table and categorized divergences live in `CONFORMANCE.md`.

### 35.3 Fuzz coverage matrix

| Target | Test binary | Properties | Default cases | Stress cases (10k/property) | Invariants |
|---|---|---|---|---|---|
| RST lexer/inline + full parse | `scribe-rst/tests/fuzz.rs` | 7 | 1024 | 70,000 | no panic; in-bounds/ordered/nested byte spans at bases 0/97/1000; prolog/epilog |
| MyST parser (new) | `scribe-myst/tests/fuzz.rs` | 4 | 2048 | 40,000 | no panic; in-bounds/UTF-8-boundary/ordered block spans; arena size linear in input |
| MyST parser (Phase 07) | `scribe-myst/tests/adversarial.rs` | 5 | 512 | 50,000 | no panic; span bounds; JSON round-trip |
| Python signatures/annotations | `scribe-domains/tests/fuzz.rs` | 5 | 2048 | 50,000 | no panic; deterministic; parsed name is a substring; linear token count |
| C/C++ declarations | `scribe-domains/tests/fuzz.rs` | (included above) | 2048 | (included above) | `CppDecl.name` = join of segments; bounded tokens; no panic |
| Config `Tags` evaluator | `scribe-config/tests/fuzz.rs` | 4 | 2048 | 40,000 | no panic; cache-deterministic; depth-/length-bounded grammar |

Total stress campaign: **25 property functions × 10,000 cases = 250,000
generated cases**, all green. Every target's case count is read from
`PROPTEST_CASES` (with a documented default), so the campaign is reproducible
with `PROPTEST_CASES=10000 cargo test --workspace`.

### 35.4 Persisted regressions and the cross-crate guard

Proptest's persisted `.proptest-regressions` files are kept in source control
and replayed automatically at property startup. The campaign produced one new
seed, for the C++ bug below. `crates/scribe-cli/tests/regression_guard.rs`
additionally (a) validates every persisted `cc <64-hex>` seed file under
`crates/*/tests/` and (b) replays the known adversarial corpora from Phases
05/07/02/12/13 through the corresponding parsers, asserting no panic, bounded
and contained spans, and unchanged constant-expression fidelity.

**Bug found and fixed:** `parse_cpp_declaration("=aa", …)` panicked with an
index-out-of-bounds at `cpp/parser.rs` because an alias with no declaration
head left `head` empty while the code indexed `head[0]`. The parser now returns
a well-formed empty `CppDecl` (preserving the alias) when the head is empty. The
failing input, its shrunk seed and three neighbours are pinned as deterministic
regressions.

### 35.5 Honest remaining gaps

The 65.6 % parity is intentionally reported as measured, not inflated. Lowest-
scoring fixtures (`test-prolog`, `test-toctree-glob`, `test-root`,
`test-domain-cpp`, `test-productionlist`, `test-default_role`) reflect known
earlier-phase limitations: `rst_prolog`/`rst_epilog` label/target injection,
globbed toctree discovery, full theme HTML DOM parity, maximum-signature-line
wrapping, productionlist token xrefs, and default-role node mapping. Several
fixtures also show warning-score divergence because nitpicky (`-n`) warnings
are emitted with different messages/codes than the oracle. The rubric compares
tag multisets, not attributes or text bytes; those are covered by the finer
per-phase byte-exact differential suites. These gaps are **tracked**, with the
baseline guarding against regressions, not pass/fail gates for Phase 29.

---

## 36. Phase 30 performance, packaging, docs & release readiness

Phase 30 is documentation/packaging only: no frozen AST, `BuildEnvironment` or
`Domain` shape was touched. The only production-code addition is the
`[profile.release]` entry in the workspace `Cargo.toml`
(`opt-level=3`, thin LTO, `codegen-units=1`, `strip=true`).

### 36.1 Self-hosting milestone (`crates/scribe-cli/tests/selfhost.rs`)

The real `sphinx/doc` tree (155 `.rst` files, 10 `sphinx.ext.*` extensions, a
custom `sphinx13` theme and three intersphinx mappings) is built end-to-end
with the compiled `scribe` binary. The test uses a **bounded 120 s timeout**
and file-backed stdout/stderr (a piped child would block on EOF because the
config-bridge Python child inherits the descriptors). It asserts the process
terminates, emits no Rust panic marker, and writes ≥100 HTML pages; it does
**not** assert oracle parity.

Recorded outcome (`BENCHMARKS.md`):

- Unmodified `conf.py`: 155 HTML pages written, **exit 1** because the custom
  `sphinx13` theme inherits from a theme that is not bundled.
- `-D html_theme=basic`: **exit 0**, 155 pages / 337 files, 1359 warnings
  (unresolved external/domain xrefs). This is a real build, not a mocked one.

### 36.2 Benchmark harness (`scripts/gen_bench_fixture.py`, `scripts/bench.sh`)

A deterministic, offline multi-page RST fixture (no extensions/intersphinx)
is generated and built cold (`-E`) by both `scribe` and `sphinx-build`. GNU
`time -v` supplies wall-clock and peak RSS; 1 warmup + 5 measured runs per
engine, medians reported. Measured on the phase-30 host: 5.25× faster and
1.86× less RSS at 40 pages; 6.38× faster but ~1.26× more RSS at 150 pages.
See `BENCHMARKS.md` for the full tables and environment. The aspirational
10–50× speedup is **not** claimed; the honest measured ratio is recorded.

### 36.3 Documentation & release configuration

- `README.md` — quickstart, feature overview, builder/extension summary.
- `MIGRATION.md` — `conf.py`/`scribe.toml` mapping, extension support matrix,
  builder/CLI mapping, and pointers to tracked divergences.
- `BENCHMARKS.md` — measured results + self-hosting ledger.
- `.github/workflows/release.yml` + `scripts/release.sh` — Linux musl
  x86_64/aarch64, macOS universal (`lipo`), Windows MSVC. Not executed during
  the gates because cross toolchains are not installed on the host; `scripts/
  release.sh --check` reports toolchain availability.

Final cross-phase status, conformance metric, fuzz total and remaining gaps are
recorded in `STATUS.md`.

---

## 37. Phase 31–36 final additions (production CLI, streaming lifecycle, warning taxonomy, external themes)

Phases 31–36 complete the engine without altering any frozen cross-phase
contract (§7 AST, §12 `BuildEnvironment`, §15 `Domain` shapes remain unchanged).
The additions are summarized concisely here; per-phase evidence is in
`STATUS.md`.

### 37.1 Production CLI subcommands (Phase 31)

The Phase 01 placeholders for `quickstart`, `clean` and `check` were replaced
with real implementations in `scribe-cli`: `clean` (path-safety, idempotent,
`--dry-run`), `check` (config/discovery/toctree/xref reuse with `-W` semantics)
and `quickstart` (interactive + `--batch` scaffolding), plus the `serve`/
`watch` subcommands from Phase 28 (`crates/scribe-cli/src/serve/`). Nine
real-binary e2e tests cover the CLI surface (`tests/cli_commands.rs`).

### 37.2 Streaming, arena-scoped build lifecycle (Phase 32)

The HTML writer's per-page `HtmlContext` now holds `Arc<Config>` /
`Arc<BuildEnvironment>` instead of cloning the whole environment per document;
pass B takes each AST out of the `parsed` map (single resident copy) and writes
doctree snapshots to disk immediately per page. An opt-in `SCRIBE_PROFILE=1`
stage sampler reports `VmRSS`/`VmHWM` deltas. The result is linear peak RSS
(@150p 116,292 KB → 76,824 KB in the final Phase 36 measurement) and no
superlinear growth; see §36.2 / `BENCHMARKS.md`.

### 37.3 Warning-code taxonomy & Sphinx-format diagnostics (Phases 33–36)

`scribe-core/src/warnings.rs` centralizes the stable `scribe::* → type.subtype`
map, the documented `suppress_warnings` code list and message classification.
Phase 36 additionally surfaces toctree diagnostics in the upstream logger shape
(`<source>/<doc>.rst: WARNING: <msg> [toc.<subtype>]`,
`crates/scribe-builders/src/build_common.rs`) so their normalized lines match
the oracle. The remaining domain/xref diagnostics still use `docname: message`
text and are tracked as a semantic gap (see §35.5 / `CONFORMANCE.md`).

### 37.4 External HTML theme resolution (Phase 35)

`scribe-theme` resolves external themes through `html_theme_path` plus
low-precedence site-packages discovery (`SCRIBE_THEME_SITE_PATHS` for
deterministic tests), keeps bundled themes winning over same-named local dirs,
and uses a theme-aware fallback manager for generated index/search/genindex/
py-modindex pages. A strict classifier warns-and-falls-back only for a
genuinely absent root theme; a present-but-misconfigured theme stays a hard
error. This lets unmodified `sphinx/doc` self-host with its custom `sphinx13`
theme (exit 0, 158 HTML pages).

### 37.5 Phase 36 final verification & honest gate status

Final measured figures (all commands run on this machine):

- Semantic parity **70.8%** over 34 applicable fixtures (4 pass / 26 partial /
  4 fail / 5 skip) — progression 65.6% → 66.7% → 70.8%. The **≥80% gate is
  NOT met**; the largest residual is the raw-toctree vs resolved-toctree
  doctree difference and the unstructured domain warning format. No metric was
  inflated and no fixture was skipped to dodge a divergence.
- Fuzz sweep: **270,000 cases** across 27 proptest properties (added
  `new_ast_node_constructs_have_valid_spans` and
  `inventory_loads_never_panics`), zero panics, all spans in bounds.
- Benchmarks: 5.42× @40p (35,296 KB), 7.00× @150p (76,824 KB < 92,496 KB
  Python) — RSS gate passes.
- `cargo test --workspace` 1,108 passed × 3 consecutive + release 1,108;
  check/clippy clean; self-host both modes exit 0; conformance regression guard
  green.

## 38. Phase 37–40 final design close-out and audited gates

### 38.1 What changed in the final four phases

- **Phase 37 — span-carrying structured diagnostics.** Warnings carry byte
  spans and a stable `type.subtype` code taxonomy (`scribe-core/src/warnings.rs`),
  rendered through `thiserror`/`miette`; no `.unwrap()`/`.expect()` in library
  code.
- **Phase 38 — exported-doctree TocTree expansion.** `expand_toctrees`
  (`scribe-core/src/toctree_expand.rs`) resolves every reachable `toctree` node
  into Sphinx's `compact_paragraph → bullet_list → list_item → reference`
  shape for the `xml`/`pseudoxml` builders, with a `visiting` cycle guard,
  `maxdepth`/`titlesonly` pruning and domain-object `literal` titles. It is
  applied only to exported doctrees so the HTML5 writer's direct `toctree`
  rendering is not double-wrapped.
- **Phase 39 — HTML5 DOM parity and domain edge-case closure.** Raised
  per-fixture structure/warning overlap (dominant parity jump 79.2% → 82.8%)
  without weakening the rubric, baseline or fixture selection.
- **Phase 40 — final audit.** Regenerated the conformance report, extended the
  fuzz suite to cover the new expander, re-measured all benchmarks, re-ran every
  gate, and published final audited figures in the five top-level docs.

### 38.2 Fuzz hardening — new TocTree expander property

`crates/scribe-core/tests/fuzz.rs` (new permanent harness, `proptest` dev-dep)
adds `toctree_expansion_never_panics_and_stays_in_bounds` plus a deterministic
`regression_cyclic_child_toctrees_terminate`. It generates documents with 1–3
top-level toctrees whose entries mix doc/self/external/unknown kinds, missing
and multi-byte targets, arbitrary `hidden`/`includehidden`/`titlesonly`/`maxdepth`
and captions, against an environment of cyclic child toctrees. Invariants:
`replaced + removed` equals the number of top-level toctrees, no toctree stays
reachable, all emitted spans are in bounds, and pre-order traversal is finite
and duplicate-free. The cycle corpus terminates via the `visiting` guard.

### 38.3 Final audited figures

- Semantic parity **82.8%** over 34 applicable fixtures (15 pass / 16 partial /
  3 fail / 5 skip); progression 65.6% → 66.7% → 70.8% → 71.5% → 79.2% → 82.8%.
  Zero per-fixture score changes vs the Phase 39 baseline; no fixtures, rubric
  or baseline rules weakened; `CONFORMANCE.md` regenerated by the test.
- Fuzz sweep **280,000 cases / 28 proptest properties / 0 panics**, spans in
  bounds: scribe-core 1 (10k), scribe-rst 8 (80k), scribe-domains 6 (60k),
  scribe-config 4 (40k), scribe-myst 4 (40k), scribe-myst adversarial 5 (50k).
  Covers the AST (`TitleReference`/`Classifier`/`OptionGroup`), RST parser, the
  new TocTree expander, C/C++ parsers and the intersphinx inventory loader.
- Benchmarks: **5.77× @40p (35,904 KB)**; **7.33× @150p (76,940 KB < 92,624 KB
  and < 92,452 KB sphinx)** — both @150p gates pass.
- Self-host `sphinx/doc` (155 pages): unmodified and `-D html_theme=basic`
  builds exit 0, 158 `.html` files, 419 warnings.
- `cargo check`/`clippy` clean; `cargo test --workspace` 1,116 passed × 3 runs
  (1,114 pre-existing + 2 new) and `--release` 1,116 passed, zero warnings;
  regression guard 5 passed; selfhost test 2 passed.
