pub mod error;
pub mod lexer;
pub mod token;

pub use error::{LexError, LexErrorKind};
pub use lexer::Lexer;
pub use token::{Token, TokenKind};

/// Tokenize a source string, returning (tokens, errors).
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<LexError>) {
    lexer::tokenize(src)
}

#[cfg(test)]
mod tests;
