//! The N3 (Notation3) front end: lexer, parser, forward/backward reasoner,
//! and output (printing/proof). It is built on the shared
//! `crate::ast::{Term, Triple, Literal}` types, and `rdf_compat` reads the
//! RDF syntaxes — Turtle, TriG, N-Triples and N-Quads — through the same
//! parser, so RDF data and N3 rules meet in one document.

pub mod lexer;
pub mod parser;
pub mod printing;
pub mod proof;
pub mod rdf_compat;
pub mod reasoner;
