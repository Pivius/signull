//! `sinu` systems language front end.
//!
//! Lexes, parses, type-checks and lowers `sinu` source to a
//! [`bytecode::ProgramImage`] for the kernel `exec` path.

pub mod ast;
pub mod codegen;
pub mod diag;
pub mod image;
pub mod lexer;
pub mod parser;
pub mod types;

pub use ast::{BinOp, Block, Expr, Func, Global, Item, Param, Program, Stmt, Type, UnOp};
pub use codegen::compile;
pub use diag::{CompileError, Span};
pub use image::{decode_image, encode_image};
pub use lexer::{Kw, Tok, Token, lex};
pub use parser::parse;
pub use types::Ty;

/// Lex and parse a source file in one call.
pub fn parse_source(source: &str) -> Result<Program, CompileError> {
    let tokens = lex(source)?;
    parse(&tokens)
}
