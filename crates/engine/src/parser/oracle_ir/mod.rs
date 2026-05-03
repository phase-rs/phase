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
pub(crate) mod replacement;
pub(crate) mod static_ir;
pub(crate) mod trigger;

#[allow(unused_imports)]
// Re-exports for future consumers; direct ast:: paths used during migration.
pub(crate) use self::ast::*;
#[allow(unused_imports)] // Re-export for future consumers using oracle_ir::ParseContext path.
pub(crate) use self::context::*;
#[allow(unused_imports)]
// Re-export for future consumers using oracle_ir::OracleDiagnostic path.
pub(crate) use self::diagnostic::*;
pub(crate) use self::doc::*;
#[allow(unused_imports)] // Re-export for future consumers; wired into parser in Plan 02.
pub(crate) use self::effect_chain::*;
#[allow(unused_imports)] // Re-export for future consumers; wired into parser in Plan 02.
pub(crate) use self::replacement::*;
#[allow(unused_imports)] // Re-export for future consumers; wired into parser in Plan 02.
pub(crate) use self::static_ir::*;
#[allow(unused_imports)] // Re-export for future consumers; wired into parser in Plan 01.
pub(crate) use self::trigger::*;
