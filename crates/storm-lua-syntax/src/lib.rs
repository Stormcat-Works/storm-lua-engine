//! Lua source syntax, AST traversal, printing and source-size measurement.
//! Compiler-only code: no VM, filesystem or scheduler is initialized.

pub mod ast;
pub mod ast_utils;
pub mod dump;
pub mod lexer;
pub mod numeric;
pub mod parser;
pub mod print;
pub mod size;
pub mod source_tools;

pub use ast::{Ast, IfArm, Node, NodeId, SymbolId, TableField};
pub use lexer::{LexError, Lexer, StormComment, Token, TokenKind};
pub use parser::{
    parse_source, parse_source_with_origins, parse_source_with_positions, NameSite, NodePositions,
    ParseError, Parser, ParserError,
};
pub use print::{token_minify, NodeEmission, PrintedSource, Printer};

pub mod source_position;

/// Node storage that invalidates unannotated mutation origins.
pub mod node_arena;
/// Optional source-origin attribution independent of syntax equality.
pub mod provenance;

pub mod explanation;

#[cfg(test)]
mod explanation_tests;
