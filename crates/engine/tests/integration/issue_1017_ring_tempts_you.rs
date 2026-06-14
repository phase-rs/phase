//! Issue #1017 — The Ring tempts you must increment level, set bearer, and fire
//! "Whenever the Ring tempts you" triggers.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const RING_TEMPT_ORACLE: &str = "The Ring tempts you.";
const RING_TRIGGER_ORACLE: &str = "Whenever the Ring tempts you, draw a card.";

#[test]
fn ring_tempts_you_fires_whenever_ring_tempts_you_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let watcher = scenario
        .add_creature_from_oracle(P0, "Ring Watcher", 1, 1, RING_TRIGGER_ORACLE)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Tempt Spell", false)
        .from_oracle_text(RING_TEMPT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().ring_level.get(&P0).copied(), Some(1));
    assert_eq!(
        runner.state().ring_bearer.get(&P0).copied().flatten(),
        Some(watcher)
    );
    assert!(runner.state().objects[&watcher]
        .card_types
        .supertypes
        .contains(&Supertype::Legendary));
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "Whenever the Ring tempts you trigger must draw a card; waiting_for={:?}",
        runner.state().waiting_for
    );
}

#[test]
fn ring_tempts_you_prompts_bearer_choice_and_fires_trigger_after_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    scenario.add_creature_from_oracle(P0, "Ring Watcher", 1, 1, RING_TRIGGER_ORACLE);
    let bear = scenario.add_creature(P0, "Ring Bearer Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Tempt Spell", false)
        .from_oracle_text(RING_TEMPT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    runner.cast(spell).resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseRingBearer { .. }
        ),
        "multiple creatures must prompt for a ring bearer"
    );
    let hand_after_tempt = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();

    runner
        .act(GameAction::ChooseRingBearer { target: bear })
        .expect("choose ring bearer");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().ring_bearer.get(&P0).copied().flatten(),
        Some(bear)
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len(),
        hand_after_tempt + 1,
        "Whenever the Ring tempts you trigger must fire after bearer selection"
    );
}
