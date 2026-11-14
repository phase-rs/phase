//! Re-export of the unified ParseContext from oracle_ir::context.
//!
//! Preserves backward compatibility for nom combinator modules that
//! import `super::context::ParseContext`. The canonical location is
//! `crate::parser::oracle_ir::context`.
#[allow(unused_imports)] // Re-export for future nom combinator consumers.
pub(crate) use crate::parser::oracle_ir::context::ParseContext;
