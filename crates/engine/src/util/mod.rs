//! Cross-cutting engine utilities.
//!
//! This module hosts helpers that are not tied to a specific game subsystem
//! (types, game rules, parser, etc.) and are used as building blocks by
//! downstream crates (`phase-ai`, `phase-server`, `engine-wasm`).

pub mod deadline;
pub mod im_ext;

pub use deadline::Deadline;
