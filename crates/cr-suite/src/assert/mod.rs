//! Typed CR assertion evaluators.
//!
//! All predicates read engine state only — no duplicated game logic.

mod combat;
mod combat_state;
mod command;
mod damage;
mod keywords;
mod layers;
mod library;
mod life;
mod phase;
mod player;
mod priority;
mod replacement;
mod sba;
mod stack;
mod targeting;
mod zone;

use engine::game::scenario::GameRunner;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::schema::AssertionSpec;

pub use combat::assert_creature_damage;
pub use combat_state::{assert_attacker_declared, assert_blocker_declared};
pub use command::{assert_in_command_zone, COMMAND_NOTES};
pub use damage::assert_player_poison;
pub use keywords::assert_creature_has_keyword;
pub use layers::LAYER_NOTES;
pub use library::{assert_library_count, assert_library_top};
pub use life::assert_player_life;
pub use phase::{assert_phase_is, parse_phase};
pub use player::{assert_game_not_over, assert_game_over, assert_hand_count};
pub use priority::assert_priority_player;
pub use replacement::REPLACEMENT_NOTES;
pub use sba::check_sbas_via_priority;
pub use stack::stack_is_empty;
pub use targeting::TARGETING_NOTES;
pub use zone::{
    assert_creature_in_graveyard, assert_creature_on_battlefield, assert_creature_zone,
};

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
        AssertionSpec::GameOver { winner } => assert_game_over(runner, winner.map(PlayerId)),
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
        AssertionSpec::StackIsEmpty => stack_is_empty(runner),
        AssertionSpec::PriorityPlayer { player } => {
            assert_priority_player(runner, PlayerId(*player))
        }
        AssertionSpec::LibraryCountEquals { player, count } => {
            assert_library_count(runner, PlayerId(*player), *count)
        }
        AssertionSpec::PlayerPoison { player, count } => {
            assert_player_poison(runner, PlayerId(*player), *count)
        }
        AssertionSpec::AttackerDeclared { creature } => {
            assert_attacker_declared(runner, handles, creature)
        }
        AssertionSpec::BlockerDeclared { creature } => {
            assert_blocker_declared(runner, handles, creature)
        }
        AssertionSpec::CreatureHasKeyword { creature, keyword } => {
            assert_creature_has_keyword(runner, handles, creature, keyword)
        }
        AssertionSpec::InCommandZone { handle } => assert_in_command_zone(runner, handles, handle),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HandCompare {
    AtLeast,
    Equals,
}
