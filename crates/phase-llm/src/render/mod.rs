//! Text rendering of engine state for LLM consumption.
//!
//! Everything here is a pure function of engine-owned data. The display layer
//! never assembles a prompt fragment of its own: if a model needs to know
//! something, the engine renders it here.

pub mod action;
#[cfg(feature = "draft")]
pub mod draft;
pub mod game;
pub mod text;
