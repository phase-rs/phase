//! Regression for issue #1128: Elrond, Master of Healing — second ability.
//!
//! "Whenever a creature you control with a +1/+1 counter on it becomes the
//! target of a spell or ability an opponent controls, you may draw a card."
//!
//! Investigation found this ability already parses and resolves correctly —
//! both constituent pieces (a controller-scoped `becomes the target of a
//! spell or ability an opponent controls` source filter, and a `<type> you
//! control with a +1/+1 counter on it` subject filter) already exist and are
//! independently tested (see `oracle_trigger_tests.rs`'s
//! `trigger_becomes_target_pawpatch_recruit_pattern` for the source-filter
//! shape and `oracle_replacement.rs`'s Uncivil Unrest coverage for the
//! counter-presence subject shape), but no existing test combined them on one
//! card. These tests close that combination gap end-to-end through the real
//! cast/targeting/trigger pipeline — no code changes were needed for this
//! ability.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const ELROND_ABILITY_2: &str = "Whenever a creature you control with a +1/+1 counter on it becomes the target of a spell or ability an opponent controls, you may draw a card.";

fn red_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// Positive: an opponent's spell targets a creature you control that has a
/// +1/+1 counter on it — the optional draw must fire when accepted.
#[test]
fn elrond_draws_when_opponent_targets_countered_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elrond = scenario
        .add_creature_from_oracle(P0, "Elrond, Master of Healing", 3, 4, ELROND_ABILITY_2)
        .id();
    scenario.with_counter(elrond, CounterType::Plus1Plus1, 1);
    // A card to draw when the optional trigger resolves — without this the
    // accepted draw comes from an empty library and P0 loses the game
    // instead of actually drawing (a state-based action, not the trigger
    // failing to fire).
    scenario.with_library_top(P0, &["Library Card"]);

    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, red_pool(1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner
        .cast(bolt)
        .target_object(elrond)
        .accept_optional()
        .resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "an opponent's spell targeting a countered creature you control must fire the optional draw"
    );
}

/// Negative: an opponent's spell targets a creature you control with NO
/// counter on it — the trigger must not fire at all.
#[test]
fn elrond_does_not_draw_when_targeted_creature_has_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Elrond, Master of Healing", 3, 4, ELROND_ABILITY_2);
    let ward = scenario.add_creature(P0, "Uncountered Ward", 2, 2).id();

    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, red_pool(1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner
        .cast(bolt)
        .target_object(ward)
        .accept_optional()
        .resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "targeting a creature with no +1/+1 counter must not fire the trigger"
    );
}

/// Negative: you (not an opponent) target your own countered creature — "an
/// opponent controls" must exclude a spell/ability you control yourself.
#[test]
fn elrond_does_not_draw_when_you_target_your_own_countered_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elrond = scenario
        .add_creature_from_oracle(P0, "Elrond, Master of Healing", 3, 4, ELROND_ABILITY_2)
        .id();
    scenario.with_counter(elrond, CounterType::Plus1Plus1, 1);

    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_pool(1));

    let mut runner = scenario.build();

    let outcome = runner
        .cast(bolt)
        .target_object(elrond)
        .accept_optional()
        .resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "a spell you control targeting your own countered creature must not satisfy 'an opponent controls'"
    );
}
