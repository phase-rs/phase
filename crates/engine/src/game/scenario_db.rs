//! Extension trait that adds `CardDatabase`-backed helpers to `GameScenario`.
//!
//! Kept separate from `scenario.rs` to preserve that module's "zero filesystem
//! dependencies" contract. Import `GameScenarioDbExt` explicitly to signal that
//! a test uses real parsed card data (and thus detects parser regressions).
//!
//! # Example
//! ```ignore
//! use engine::game::scenario_db::GameScenarioDbExt;
//!
//! let db = CardDatabase::from_export(&data_dir).unwrap();
//! let mut scenario = GameScenario::new();
//! scenario.at_phase(Phase::PreCombatMain);
//! let bolt_id = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, &db);
//! ```

use crate::database::card_db::CardDatabase;
use crate::game::deck_loading::create_object_from_card_face;
use crate::game::scenario::GameScenario;
use crate::game::zones::{add_to_zone, remove_from_zone};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Extends `GameScenario` with `CardDatabase`-backed card placement.
///
/// Methods here use the real parser output stored in the database, so any
/// parser regression that alters a card's abilities will break tests that
/// add that card via these helpers. This is intentional — it makes parser
/// coverage part of integration test coverage.
pub trait GameScenarioDbExt {
    /// Add a card from the database to a player's chosen zone.
    ///
    /// Looks up the card by name (case-insensitive, matches the first face).
    /// Panics if the card is not found in the database.
    ///
    /// Creatures placed on the `Battlefield` are not summoning-sick by default
    /// (entered the previous turn), matching the behavior of `add_creature`.
    fn add_real_card(
        &mut self,
        player: PlayerId,
        name: &str,
        zone: Zone,
        db: &CardDatabase,
    ) -> ObjectId;
}

impl GameScenarioDbExt for GameScenario {
    fn add_real_card(
        &mut self,
        player: PlayerId,
        name: &str,
        zone: Zone,
        db: &CardDatabase,
    ) -> ObjectId {
        let face = db
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("card '{}' not found in CardDatabase", name));

        // create_object_from_card_face places the object in Zone::Library
        let id = create_object_from_card_face(&mut self.state, face, player);

        // Move from Library to the requested zone
        remove_from_zone(&mut self.state, id, Zone::Library, player);
        add_to_zone(&mut self.state, id, zone, player);
        self.state.objects.get_mut(&id).unwrap().zone = zone;

        // Creatures entering the battlefield are not summoning-sick by default
        if zone == Zone::Battlefield {
            let entered_turn = self.state.turn_number.saturating_sub(1);
            self.state
                .objects
                .get_mut(&id)
                .unwrap()
                .entered_battlefield_turn = Some(entered_turn);
        }

        id
    }
}
