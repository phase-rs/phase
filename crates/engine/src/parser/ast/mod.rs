//! Top-level AST module for the Oracle parser refactor.
//!
//! See `PLAN.md` §4.1 for the eventual layout. Phase 1 introduces only
//! `clause` (lifted verbatim from `parser::oracle_effect::types`); future
//! phases add `trigger`, `static_`, `replacement`, `modal`, `casting`,
//! `keyword`, `cost`, and a top-level `OracleAst` enum.

pub(crate) mod clause;
