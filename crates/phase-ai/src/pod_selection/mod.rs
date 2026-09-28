//! Pre-game AI-seat pod selection.
//!
//! Commander bracket guidance is format policy, not part of the Comprehensive
//! Rules. The selector is pure and seeded: identical candidates, request, and
//! card database produce an identical assignment.

pub mod select;
mod types;

pub use select::{commander_color_identity, select_pod, RELAXATION_ORDER};
pub use types::{
    AiDeckCandidate, BracketLabel, LabelProvenance, PodAssignment, PodConstraint, PodRelaxation,
    PodSeat, PodSeatOccupant, PodSelectionError, PodSelectionRequest, SeatAttribute,
    TierEnforcement, TierSet,
};
