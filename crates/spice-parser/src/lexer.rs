//! SPICE tokenizer. Handles line continuation (`+`), comments (`*`, `;`),
//! and case-insensitive directives (`.tran`, `.subckt`, ...).

use spice_diagnostics::{FileId, Span};

use crate::ParseResult;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub span: Span,
    pub kind: TokenKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Word(String),
    Number(f64),
    Equals,
    LParen,
    RParen,
    Eol,
}

pub fn tokenize(_source: &str, _file: FileId) -> ParseResult<Vec<Token>> {
    // TODO: implement. Stub returns empty stream.
    Ok(Vec::new())
}
