//! Native Python autodoc and napoleon engine (Phase 21).
//!
//! The primary path is fully static: Python sources are parsed with
//! `rustpython-parser` into [`model::ModuleInfo`] trees, docstrings are
//! converted by the [`napoleon`] converter, and auto directives emit the same
//! skeleton `desc` nodes as the Phase 12 `py:*` directives so signature
//! rendering is delegated to `scribe_domains::PythonDomain`. An optional
//! documented subprocess bridge (`dynamic`) provides introspection fallback.

pub mod autosummary;
pub mod config;
pub mod discovery;
pub mod directives;
pub mod dynamic;
pub mod error;
pub mod model;
pub mod napoleon;
pub mod parser;

pub use autosummary::{
    escape_rst, generate_stubs, mangle_signature, parse_names, AutosummaryDirective,
    AutosummaryItem, AutosummaryOptions, StubRenderer, StubReport,
};
pub use config::{napoleon_options, register_config_keys, AutoOptions, ClassDocFrom, MemberOrder};
pub use discovery::{discover_package, load_modules, resolve_module, search_roots, LoadReport};
pub use directives::{register_directives, select_members};
pub use dynamic::{introspect, DynamicModule, DynamicOptions};
pub use error::{AutodocError, AutodocResult};
pub use model::*;
pub use napoleon::{convert_docstring, NapoleonOptions};
pub use parser::{dedent_docstring, parse_source, parse_sources};
