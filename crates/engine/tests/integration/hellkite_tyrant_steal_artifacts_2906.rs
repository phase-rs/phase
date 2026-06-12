//! Integration regression for Hellkite Tyrant (#2906).
//!
//! Oracle (relevant line): "Whenever this creature deals combat damage to a
//! player, gain control of all artifacts that player controls."
//!
//! This drives the REAL production pipeline end-to-end — Oracle parse → combat
//! damage → `DamageDone` trigger → "that player" (the damaged player) resolving
//! from event context → `GainControlAll` battlefield enumeration — rather than
//! a hand-built `ResolvedAbility`. Those joints (the combat-damage trigger
//! actually firing, the triggering-damaged-player anaphor, and mass resolution)
//! are exactly where the reported "does nothing" bug lived, and each one is a
//! place a unit test that constructs the resolved ability directly cannot
//! exercise.
//!
//! Discriminating: revert any one of the three fixes and this fails —
//!   - without `GainControlAll`, the mass filter is never enumerated (0 taken);
//!   - if `GainControlAll`'s `target_filter()` is treated as a chosen-target
//!     slot, the trigger fizzles before resolving (0 taken);
//!   - if `controller: TargetPlayer` does not fall back to the triggering
//!     event's player, the filter matches no artifacts (0 taken).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::run_combat;

const HELLKITE_ORACLE: &str = "Flying, trample\nWhenever Hellkite Tyrant deals \
    combat damage to a player, gain control of all artifacts that player controls.";

fn add_permanent(
    runner: &mut GameRunner,
    cid: u32,
    controller: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let id = create_object(
        runner.state_mut(),
        CardId(cid.into()),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    id
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects.get(&id).unwrap().controller
}

#[test]
fn hellkite_combat_damage_steals_all_of_the_damaged_players_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hellkite = scenario
        .add_creature_from_oracle(P0, "Hellkite Tyrant", 6, 5, HELLKITE_ORACLE)
        .id();
    let mut runner = scenario.build();

    // The defending player (P1) controls two artifacts and one non-artifact
    // (an Enchantment, so no state-based death complicates the assertion).
    let boots = add_permanent(&mut runner, 100, P1, "Swiftfoot Boots", CoreType::Artifact);
    let banana = add_permanent(&mut runner, 101, P1, "Banana", CoreType::Artifact);
    let pact = add_permanent(&mut runner, 102, P1, "Demonic Pact", CoreType::Enchantment);

    // Sanity: everything starts under P1's control.
    assert_eq!(controller_of(&runner, boots), P1);
    assert_eq!(controller_of(&runner, banana), P1);
    assert_eq!(controller_of(&runner, pact), P1);

    // Hellkite attacks P1 unblocked and deals combat damage; the trigger fires
    // and resolves.
    run_combat(&mut runner, vec![hellkite], vec![]);
    runner.advance_until_stack_empty();

    // CR 613.1b: every artifact the damaged player (P1) controlled is now
    // controlled by Hellkite's controller — ALL of them, not one.
    assert_eq!(
        controller_of(&runner, boots),
        P0,
        "Hellkite's controller gains the first artifact",
    );
    assert_eq!(
        controller_of(&runner, banana),
        P0,
        "and the second — 'all artifacts', not a single targeted permanent",
    );
    // The non-artifact permanent is untouched (the filter is type-scoped).
    assert_eq!(
        controller_of(&runner, pact),
        P1,
        "the Enchantment is not an artifact and must stay with its controller",
    );
}
