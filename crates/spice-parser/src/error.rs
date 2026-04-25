use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("lex error at line {line}: {message}")]
    Lex { line: usize, message: String },

    #[error("parse error at line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("unsupported construct at line {line}: {message}")]
    Unsupported { line: usize, message: String },
}
