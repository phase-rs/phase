//! Issue #2921: Amareth, the Lustrous — shared-card-type gate on optional reveal.
//!
//! https://github.com/phase-rs/phase/issues/2921
//!
//! Oracle: "Whenever another permanent you control enters, look at the top card
//! of your library. If it shares a card type with that permanent, you may
//! reveal that card and put it into your hand."

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMARETH_ORACLE: &str = "Flying\nWhenever another permanent you control enters, look at the \
top card of your library. If it shares a card type with that permanent, you may reveal that card \
and put it into your hand.";

fn put_library_top(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let zone = runner.state().objects.get(&id).expect("object").zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner
            .state_mut()
            .players
            .get_mut(owner.0 as usize)
            .unwrap()
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

fn library_top_id(runner: &engine::game::scenario::GameRunner) -> ObjectId {
    runner
        .state()
        .players
        .get(P0.0 as usize)
        .unwrap()
        .library
        .front()
        .copied()
        .expect("library non-empty")
}

/// Creature ETB + creature library top → shared `Creature` card type → optional
/// reveal is offered and accepting puts the card into hand.
#[test]
fn amareth_offers_reveal_when_library_top_shares_card_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Amareth, the Lustrous", 2, 2)
        .from_oracle_text(AMARETH_ORACLE);

    let library_creature = scenario.add_creature(P0, "Library Bear", 2, 2).id();
    let entering_creature = scenario.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, library_creature);

    assert!(runner
        .state()
        .objects
        .get(&library_creature)
        .unwrap()
        .card_types
        .core_types
        .contains(&CoreType::Creature));

    assert_eq!(
        library_top_id(&runner),
        library_creature,
        "precondition: library_creature is on top"
    );

    let outcome = runner.cast(entering_creature).accept_optional().resolve();

    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(
        runner.state().objects.get(&library_creature).unwrap().zone,
        Zone::Hand
    );
}

/// Creature ETB + instant library top → no shared card type → optional reveal is
/// skipped and the library top stays in the library.
#[test]
fn amareth_skips_reveal_when_library_top_lacks_shared_card_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Amareth, the Lustrous", 2, 2)
        .from_oracle_text(AMARETH_ORACLE);

    let library_instant = scenario
        .add_spell_to_hand_from_oracle(P0, "Library Bolt", true, "Instant\nDeal 3 damage.")
        .id();
    let entering_creature = scenario.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, library_instant);

    assert!(runner
        .state()
        .objects
        .get(&library_instant)
        .unwrap()
        .card_types
        .core_types
        .contains(&CoreType::Instant));

    let outcome = runner.cast(entering_creature).resolve();

    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        runner.state().objects.get(&library_instant).unwrap().zone,
        Zone::Library
    );
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "must not prompt to reveal when card types do not share"
    );
}
