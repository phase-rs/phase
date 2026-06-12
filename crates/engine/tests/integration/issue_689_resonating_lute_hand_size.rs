//! Regression for GitHub issue #689 — Resonating Lute draw restriction.
//!
//! "{T}: Draw a card. Activate only if you have seven or more cards in hand."
//! must not be offered when the controller has fewer than seven cards.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const RESONATING_LUTE: &str = "Each land you control has \"{T}: Add two mana of any one color. Spend this mana only to cast instant and/or sorcery spells.\"\n\
{T}: Draw a card. Activate only if you have seven or more cards in hand.";

#[test]
fn resonating_lute_draw_not_activatable_below_seven_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lute_id = scenario
        .add_creature(P0, "Resonating Lute", 0, 0)
        .from_oracle_text(RESONATING_LUTE)
        .as_artifact()
        .id();

    // Four cards in hand including Resonating Lute on the battlefield — hand size 4.
    for i in 0..4 {
        scenario.add_creature_to_hand(P0, &format!("Hand Filler {i}"), 1, 1);
    }

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let lute_actions = grouped.get(&lute_id).map(Vec::as_slice).unwrap_or(&[]);
    let draw_offered = lute_actions.iter().any(|a| {
        matches!(
            a,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == lute_id
        )
    });

    assert!(
        !draw_offered,
        "draw ability must not be legal with only 4 cards in hand; got {lute_actions:?}"
    );
}

#[test]
fn resonating_lute_draw_activatable_at_seven_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lute_id = scenario
        .add_creature(P0, "Resonating Lute", 0, 0)
        .from_oracle_text(RESONATING_LUTE)
        .as_artifact()
        .id();

    for i in 0..7 {
        scenario.add_creature_to_hand(P0, &format!("Hand Filler {i}"), 1, 1);
    }

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let lute_actions = grouped.get(&lute_id).map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        lute_actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == lute_id
        )),
        "draw ability must be legal with 7 cards in hand; got {lute_actions:?}"
    );
}

#[test]
fn resonating_lute_from_card_db_respects_hand_size_restriction() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lute_id = scenario.add_real_card(P0, "Resonating Lute", Zone::Battlefield, db);
    for i in 0..4 {
        scenario.add_creature_to_hand(P0, &format!("Hand Filler {i}"), 1, 1);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let draw_index = runner
        .state()
        .objects
        .get(&lute_id)
        .and_then(|o| {
            o.abilities
                .iter()
                .position(|a| matches!(*a.effect, engine::types::ability::Effect::Draw { .. }))
        })
        .expect("Resonating Lute export must include a Draw activated ability");

    let (_, _, grouped) = legal_actions_full(runner.state());
    let lute_actions = grouped.get(&lute_id).map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        !lute_actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == lute_id && *ability_index == draw_index
        )),
        "card-data Resonating Lute draw must not be legal with 4 cards in hand; got {lute_actions:?}"
    );

    let restrictions = runner.state().objects[&lute_id].abilities[draw_index]
        .activation_restrictions
        .clone();
    assert!(
        !restrictions.is_empty(),
        "draw ability from card-data must carry activation_restrictions"
    );

    let err = runner
        .act(GameAction::ActivateAbility {
            source_id: lute_id,
            ability_index: draw_index,
        })
        .expect_err("handle_activate must reject draw below hand-size threshold");
    assert!(
        err.to_string().contains("restriction") || err.to_string().contains("not satisfied"),
        "expected activation restriction error, got {err}"
    );
}
