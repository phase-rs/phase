//! Issue #1022 — Savai Triome hand cycling must discard the card and draw.
//!
//! CR 702.29a: Cycling functions only while the card is in a player's hand.
//! CR 701.9a: Discard moves a card from hand to graveyard.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_activate_ability_now, handle_activate_ability};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAVAI_TRIOME_ORACLE: &str =
    "({T}: Add {R}, {W}, or {B}.)\nThis land enters tapped.\nCycling {3}";

fn cycling_index(state: &engine::types::game_state::GameState, triome: ObjectId) -> usize {
    state.objects[&triome]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
        .expect("synthesized cycling ability")
}

#[test]
fn savai_triome_hand_cycling_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Cycled Draw"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 3],
    );

    let triome = scenario
        .add_land_to_hand(P0, "Savai Triome")
        .from_oracle_text(SAVAI_TRIOME_ORACLE)
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();
    let cycling_index = cycling_index(runner.state(), triome);

    assert!(
        can_activate_ability_now(runner.state(), P0, triome, cycling_index),
        "cycling must be legal from hand"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: triome,
            ability_index: cycling_index,
        })
        .expect("activate cycling");

    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&triome].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].hand.len(), 1, "cycling must draw");
    assert_eq!(runner.state().players[0].library.len(), library_before - 1);
}

#[test]
fn savai_triome_battlefield_cycling_is_not_legal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 3],
    );

    let triome = scenario
        .add_land_to_hand(P0, "Savai Triome")
        .from_oracle_text(SAVAI_TRIOME_ORACLE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&triome].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: triome,
            card_id,
        })
        .expect("play Savai Triome");

    let cycling_index = cycling_index(runner.state(), triome);

    assert!(
        !can_activate_ability_now(runner.state(), P0, triome, cycling_index),
        "CR 702.29a: cycling functions only from hand"
    );
    assert!(
        !legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == triome && *ability_index == cycling_index
        )),
        "legal actions must not offer battlefield cycling"
    );
}

#[test]
fn savai_triome_battlefield_cycling_rejected_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 3],
    );

    let triome = scenario
        .add_land_to_hand(P0, "Savai Triome")
        .from_oracle_text(SAVAI_TRIOME_ORACLE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&triome].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: triome,
            card_id,
        })
        .expect("play Savai Triome");

    let cycling_index = cycling_index(runner.state(), triome);
    let library_before = runner.state().players[0].library.len();
    let mut events = Vec::new();

    let err = handle_activate_ability(runner.state_mut(), P0, triome, cycling_index, &mut events)
        .expect_err("battlefield cycling must be rejected before costs are paid");

    assert!(
        err.to_string().contains("correct zone"),
        "unexpected error: {err}"
    );
    assert_eq!(runner.state().objects[&triome].zone, Zone::Battlefield);
    assert!(runner.state().players[0].hand.is_empty());
    assert_eq!(runner.state().players[0].library.len(), library_before);
    assert!(
        !events.iter().any(
            |event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == triome)
        ),
        "discard cost must not execute when activation zone is wrong"
    );
}
