//! Nom 8.0 shared combinator module for Oracle text parsing.
//!
//! This module provides typed, composable parser combinators built on nom 8.0.
//! All parser branch files use these combinators for dispatch, structural
//! parsing, and atomic operations (numbers, mana, colors, P/T, etc.).
//!
//! All combinators use the standardized `OracleResult` type alias and the
//! trait-based `.parse(input)` API from nom 8.0.

pub mod bridge;
pub mod condition;
pub mod context;
pub mod duration;
pub mod enchant;
pub mod error;
pub mod filter;
pub mod primitives;
pub mod quantity;
pub mod target;
