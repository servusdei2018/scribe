//! Internationalization pipeline (Phase 26).
//!
//! * [`po`] — PO/POT parsing and deterministic emission with upstream's
//!   header-preservation semantics.
//! * [`mo`] — dependency-free GNU binary MO reader/writer.
//! * [`catalog`] — locale discovery, `.po` → `.mo` compilation and catalog
//!   loading for the configured language/domain.
//! * [`builder`] — the `gettext` message-catalog builder.

pub mod builder;
pub mod catalog;
pub mod mo;
pub mod po;

pub use builder::{GettextBuilder, GettextResult};
pub use catalog::{
    catalog_paths, compact_setting, compile_all, compile_catalog, docname_to_domain, is_outdated,
    language, load_catalog, locale_dirs, CatalogPaths,
};
pub use mo::{read_mo, write_mo, MoError};
pub use po::{
    escape_inline, escape_message, should_write, unquote_po, Message, PoCatalog, PotContext,
};
