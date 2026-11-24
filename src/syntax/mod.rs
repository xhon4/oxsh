//! Syntax: lexing and parsing. Pure code: nothing here runs commands,
//! reads variables or touches files (invariant I2).
// Lexing and expansion must not panic on any input (I3); tests may.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod lex;
pub mod parse;

pub use lex::{OpKind, SyntaxError, Token, lex};
