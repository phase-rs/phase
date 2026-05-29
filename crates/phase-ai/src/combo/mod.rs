//! Combo-recognition layer for cEDH difficulty.
//!
//! - `line.rs` — pure types (`ComboLine`, `ComboPiece`, `ComboReachability`, ...)
//! - `detection.rs` — `ComboDetector` trait + structural impl over `GameState`
//! - `registry.rs` — hand-authored `ComboRegistry`
//!
//! `ComboLinePolicy` (in `policies/combo_line.rs`) wires this layer into the
//! existing planner via `TacticalPolicy::activation()` keyed on
//! `DeckFeatures::is_cedh`.

pub mod detection;
pub mod line;
pub mod registry;

pub use detection::{ComboDetector, StructuralComboDetector};
pub use line::{
    CardPredicate, ComboLine, ComboLineId, ComboPiece, ComboReachability, ComboStep, WinKind,
};
pub use registry::ComboRegistry;
