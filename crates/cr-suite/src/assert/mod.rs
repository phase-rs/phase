//! Typed CR assertion evaluators.
//!
//! All predicates read engine state only — no duplicated game logic.

mod combat;
mod life;
mod phase;
mod player;
mod sba;
mod stack;
mod zone;

use engine::game::scenario::GameRunner;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::schema::AssertionSpec;

pub use combat::assert_creature_damage;
pub use life::assert_player_life;
pub use phase::assert_phase_is;
pub use player::{assert_game_not_over, assert_game_over, assert_hand_count};
pub use sba::check_sbas_via_priority;
pub use stack::stack_is_empty;
pub use zone::{assert_creature_in_graveyard, assert_creature_on_battlefield, assert_creature_zone};

/// Object handle map from fixture creature ids → ObjectIds.
pub type HandleMap = std::collections::HashMap<String, ObjectId>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionFailure {
    pub kind: String,
    pub detail: String,
}

/// Evaluate one assertion against the current runner state.
pub fn evaluate_assertion(
    runner: &GameRunner,
    handles: &HandleMap,
    assertion: &AssertionSpec,
) -> Result<(), AssertionFailure> {
    match assertion {
        AssertionSpec::PlayerLife { player, life } => {
            assert_player_life(runner, PlayerId(*player), *life)
        }
        AssertionSpec::CreatureZone { creature, zone } => {
            assert_creature_zone(runner, handles, creature, zone)
        }
        AssertionSpec::CreatureOnBattlefield { creature } => {
            assert_creature_on_battlefield(runner, handles, creature)
        }
        AssertionSpec::CreatureInGraveyard { creature } => {
            assert_creature_in_graveyard(runner, handles, creature)
        }
        AssertionSpec::GameOver { winner } => {
            assert_game_over(runner, winner.map(PlayerId))
        }
        AssertionSpec::GameNotOver => assert_game_not_over(runner),
        AssertionSpec::PhaseIs { phase } => assert_phase_is(runner, phase),
        AssertionSpec::HandCountAtLeast { player, count } => {
            assert_hand_count(runner, PlayerId(*player), *count, HandCompare::AtLeast)
        }
        AssertionSpec::HandCountEquals { player, count } => {
            assert_hand_count(runner, PlayerId(*player), *count, HandCompare::Equals)
        }
        AssertionSpec::CreatureDamage { creature, damage } => {
            assert_creature_damage(runner, handles, creature, *damage)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HandCompare {
    AtLeast,
    Equals,
}
