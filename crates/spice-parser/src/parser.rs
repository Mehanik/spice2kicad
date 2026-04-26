//! Token stream -> [`crate::ast::Netlist`].

use crate::ParseResult;
use crate::ast::Netlist;
use crate::lexer::Token;

pub fn parse(_tokens: &[Token]) -> ParseResult<Netlist> {
    // TODO: implement. Stub returns empty netlist.
    Ok(Netlist::default())
}
