//! Nom 8.0 shared combinator module for Oracle text parsing.
//!
//! This module provides typed, composable parser combinators built on nom 8.0
//! that serve as the foundation for migrating the strip_prefix-based Oracle
//! parser to structured, error-accumulating combinators.
//!
//! All combinators use the standardized `OracleResult` type alias and the
//! trait-based `.parse(input)` API from nom 8.0.

pub mod bridge;
pub mod condition;
pub mod context;
pub mod duration;
pub mod error;
pub mod filter;
pub mod primitives;
pub mod quantity;
pub mod target;
