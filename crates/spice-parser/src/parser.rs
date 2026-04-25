//! Token stream -> [`crate::ast::Netlist`].

use crate::ast::Netlist;
use crate::error::ParseError;
use crate::lexer::Token;

pub fn parse(_tokens: &[Token]) -> Result<Netlist, ParseError> {
    // TODO: implement. Stub returns empty netlist.
    Ok(Netlist::default())
}
