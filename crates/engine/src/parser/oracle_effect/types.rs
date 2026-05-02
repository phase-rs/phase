//! Compatibility shim — AST and intermediate parser-state types now live
//! in `parser::ast::clause`.
//!
//! Phase 1 of the Oracle parser refactor (see `PLAN.md` §5) relocated the
//! type definitions out of `oracle_effect/` so all parser sub-pipelines
//! can share the AST. This module re-exports everything from the new
//! location so existing call sites (`use super::types::*;`,
//! `super::types::ParsedEffectClause`, etc.) continue to compile
//! unchanged. The shim is removed in the Phase 8 cleanup pass once all
//! call sites have been updated to import from `crate::parser::ast`
//! directly.

pub(crate) use crate::parser::ast::clause::*;

/// Debug-only assertion that a `parse_target` remainder doesn't contain a compound
/// connector (` and <verb>`). Used as a safety net at call sites that discard
/// remainders — compound detection runs first, so these should never fire for
/// production paths. `and put ...` is exempt because targeted compound actions
/// intentionally preserve that continuation for the higher-level clause parser.
///
/// Lives here (not in `parser::ast::clause`) because it depends on
/// `oracle_effect::sequence::starts_bare_and_clause`, which is parser
/// internal state, not AST. The AST module must not depend on
/// `oracle_effect/`.
#[cfg(debug_assertions)]
pub(crate) fn assert_no_compound_remainder(rem: &str, context: &str) {
    assert!(
        rem.is_empty()
            || !rem.strip_prefix(" and ").is_some_and(|after| {
                let after = after.trim();
                !after.starts_with("put ") && super::sequence::starts_bare_and_clause(after)
            }),
        "silent remainder drop: {rem:?} from: {context:?}"
    );
}
