//! SPICE netlist parser.
//!
//! Pipeline: source text -> [`lexer::scan`] (logical-line tokenisation)
//! -> [`parser::parse`] -> [`ast::Netlist`].
//!
//! On success, [`parse`] returns a [`ParseOutcome`] that carries both
//! the netlist and any non-fatal diagnostics (warnings, notes) emitted
//! during the parse. On failure, the full diagnostic list is returned
//! as `Err` and short-circuits any subsequent stage.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod spec_version;

use spice_diagnostics::{Diagnostic, FileId};

pub use ast::Netlist;
pub use spec_version::{CURRENT_SPEC, check as check_spec_version};

/// Successful parse result: netlist plus any non-fatal diagnostics.
#[derive(Debug, Clone)]
pub struct ParseOutcome {
    pub netlist: Netlist,
    pub diagnostics: Vec<Diagnostic>,
}

/// Result type used throughout the parser.
///
/// On failure, every collected diagnostic is returned. The list is
/// non-empty when `Err` is produced.
pub type ParseResult<T> = Result<T, Vec<Diagnostic>>;

pub fn parse(source: &str, file: FileId) -> ParseResult<ParseOutcome> {
    let scanned = lexer::scan(source, file);
    parser::parse(scanned, file)
}
