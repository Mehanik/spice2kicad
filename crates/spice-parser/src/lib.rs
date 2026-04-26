//! SPICE netlist parser.
//!
//! Pipeline: source text -> [`lexer`] tokens -> [`parser`] -> [`ast::Netlist`].
//!
//! Errors are reported as [`spice_diagnostics::Diagnostic`] values; a fatal
//! parse returns `Err(Vec<Diagnostic>)`. Once the parser grows real
//! soft-warning paths a separate channel will be added (see
//! `docs/layout-adr.md` ADR-6).

pub mod ast;
pub mod lexer;
pub mod parser;

use spice_diagnostics::{Diagnostic, FileId};

pub use ast::Netlist;

/// Result type used throughout the parser.
///
/// On failure, every collected diagnostic is returned. The list is
/// non-empty when `Err` is produced.
pub type ParseResult<T> = Result<T, Vec<Diagnostic>>;

pub fn parse(source: &str, file: FileId) -> ParseResult<Netlist> {
    let tokens = lexer::tokenize(source, file)?;
    parser::parse(&tokens)
}
