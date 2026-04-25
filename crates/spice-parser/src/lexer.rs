//! SPICE tokenizer. Handles line continuation (`+`), comments (`*`, `;`),
//! and case-insensitive directives (`.tran`, `.subckt`, ...).

use crate::error::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub line: usize,
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

pub fn tokenize(_source: &str) -> Result<Vec<Token>, ParseError> {
    // TODO: implement. Stub returns empty stream.
    Ok(Vec::new())
}
