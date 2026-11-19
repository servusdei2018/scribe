//! Native `sphinx.ext.autosummary` support (Phase 22).
//!
//! Upstream's extension has two halves: the `autosummary` *directive* builds
//! a signature/summary table (and an optional hidden toctree), while a pre-read
//! pass generates stub `.rst` pages from Jinja2 templates when
//! `autosummary_generate` is on. Both halves consume the Phase 21 static module
//! model instead of importing Python; the frozen §7 AST, §12 `BuildEnvironment`
//! and §15 `Domain` shapes are untouched. See `ARCHITECTURE.md` §28.

pub mod directive;
pub mod render;
pub mod stub;
pub mod table;
mod templates;

pub use directive::{register_config_keys, register_directives, stub_roots, AutosummaryDirective};
pub use render::{escape_rst, StubRenderer};
pub use stub::{generate_stubs, StubReport};
pub use table::{
    emit_table, filename_map, first_summary, items_from_names, limited_join, mangle_signature,
    parse_names, AutosummaryItem, AutosummaryOptions,
};
