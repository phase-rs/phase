#![recursion_limit = "512"]
// Debug builds of this crate's large dispatch functions allocate stack frames above
// clippy's 512 KB `large_unopt_stack_frames` default. Test threads run with
// RUST_MIN_STACK = 16 MiB (.cargo/config.toml) and server runtime threads with 32 MiB.
#![allow(clippy::large_unopt_stack_frames)]

//! The phase.rs Oracle parser: Oracle text to the `phase-ast` ability vocabulary.

pub use phase_ast::types;
pub mod parser;
