//! Unified Oracle IR module — AST types and document-level IR.
//!
//! Phase 47: Foundation module for the Oracle AST/IR layer (v1.4).
//! - `ast`: All parser AST types (moved from oracle_effect/types.rs, oracle_modal.rs, oracle.rs)
//! - `doc`: Document-level IR types (OracleDocIr, OracleItemIr)

pub(crate) mod ast;
pub(crate) mod context;
pub mod diagnostic;
pub(crate) mod doc;
pub(crate) mod effect_chain;
pub(crate) mod relation;
pub(crate) mod replacement;
pub(crate) mod static_ir;
pub(crate) mod trigger;

#[cfg(test)]
mod snapshot_tests;
