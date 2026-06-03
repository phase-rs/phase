//! CR 309 / CR 311: Planechase planar deck scaffolding.
//!
//! Full planar die, planeswalk actions, and chaos abilities are not yet wired;
//! this module holds the shared game-state shape so format setup and coverage
//! can reference a single `planar_deck` home.

use serde::{Deserialize, Serialize};

use crate::types::player::PlayerId;

/// CR 309.4: Active planar deck and face-up plane for a Planechase game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanechaseState {
    /// Plane card names in the planar deck (ordered).
    pub planar_deck: Vec<String>,
    /// Index of the face-up plane in `planar_deck`, if any.
    pub active_plane_index: Option<usize>,
    /// CR 311.2: The player who controls the planar deck (starting player).
    pub planar_controller: PlayerId,
}

impl PlanechaseState {
    pub fn new(planar_controller: PlayerId, planar_deck: Vec<String>) -> Self {
        Self {
            planar_deck,
            active_plane_index: None,
            planar_controller,
        }
    }

    pub fn active_plane_name(&self) -> Option<&str> {
        self.active_plane_index
            .and_then(|idx| self.planar_deck.get(idx))
            .map(String::as_str)
    }
}
