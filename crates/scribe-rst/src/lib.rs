//! reStructuredText lexer, indentation scanner, inline parser and directive
//! registry.
//!
//! Implementation is split across Phases 04 (lexer/block scanner), 05 (inline
//! markup and roles) and 06 (directives and roles). All three phases construct
//! nodes from `scribe_ast`.

/// The phase range that will implement this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phases 04-06";
