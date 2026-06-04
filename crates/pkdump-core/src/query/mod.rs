//! Scryfall-style search query language — pure parsing stage.
//!
//! Pipeline: tokenize → recursive-descent parse → [`Ast`]. There is no IO
//! here; the [`KeywordRegistry`] (alias → canonical resolution) is injected by
//! the caller so the crate stays IO-free. The SQL compiler that consumes the
//! [`Ast`] lives in `pkdump-db`. See architecture/SEARCH_QUERY_LANGUAGE.md.

pub mod ast;
pub mod error;
mod lexer;
mod parser;
pub mod registry;

pub use ast::{Ast, Op};
pub use error::QueryError;
pub use parser::parse;
pub use registry::{KeywordDef, KeywordRegistry};
