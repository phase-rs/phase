//! Issue #629 — Fractured Sanity cycling must mill each opponent four cards.
//!
//! CR 702.29a: Cycling is an activated ability from hand (discard, draw).
//! CR 702.29c: "When you cycle this card" triggers fire from the graveyard.
//! CR 701.13: Mill moves cards from library to graveyard.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const FRACTURED_SANITY_ORACLE: &str = "\
Each opponent mills fourteen cards.\n\
Cycling {1}{U} ({1}{U}, Discard this card: Draw a card.)\n\
When you cycle this card, each opponent mills four cards.";

fn cycling_index(state: &engine::types::game_state::GameState, card: ObjectId) -> usize {
    state.objects[&card]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
        .expect("synthesized cycling ability")
}

#[test]
fn fractured_sanity_parses_cycle_trigger_with_mill() {
    let mut scenario = GameScenario::new();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Fractured Sanity", false, FRACTURED_SANITY_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&card];

    let cycle_trigger = obj
        .trigger_definitions
        .iter_unchecked()
        .find(|t| t.definition.mode == TriggerMode::Cycled)
        .expect("must parse a Cycled self-trigger");
    assert_eq!(
        cycle_trigger.definition.valid_card,
        Some(engine::types::ability::TargetFilter::SelfRef)
    );
    assert!(cycle_trigger
        .definition
        .trigger_zones
        .contains(&Zone::Graveyard));
    assert!(
        cycle_trigger.definition.execute.is_some(),
        "cycle trigger must carry the mill effect"
    );
    assert!(
        cycling_index(runner.state(), card) < obj.abilities.len(),
        "cycling activated ability must be synthesized"
    );
}

#[test]
fn fractured_sanity_cycling_mills_each_opponent_four() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Cycled Draw"]);
    for i in 0..10 {
        scenario.with_library_top(P1, &[&format!("Opp Card {i}")]);
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
        ],
    );

    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Fractured Sanity", false, FRACTURED_SANITY_ORACLE)
        .id();

    let mut runner = scenario.build();
    let opp_library_before = runner.state().players[1].library.len();
    let cycling_index = cycling_index(runner.state(), card);

    runner
        .act(GameAction::ActivateAbility {
            source_id: card,
            ability_index: cycling_index,
        })
        .expect("activate cycling");

    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        4,
        "cycling trigger must mill opponent four cards"
    );
    assert_eq!(
        runner.state().players[1].library.len(),
        opp_library_before - 4,
        "opponent library must shrink by four"
    );
}
