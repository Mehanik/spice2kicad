//! SPICE netlist parser.
//!
//! Pipeline: source text -> [`lexer`] tokens -> [`parser`] -> [`ast::Netlist`].

pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;

pub use ast::Netlist;
pub use error::ParseError;

pub fn parse(source: &str) -> Result<Netlist, ParseError> {
    let tokens = lexer::tokenize(source)?;
    parser::parse(&tokens)
}
