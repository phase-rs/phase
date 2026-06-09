//! Issue #1963 — Lotleth Troll's activated ability cost must only accept creature
//! cards from hand, not any card type.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, CardSelectionMode, DiscardSelfScope, QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOTLETH_ORACLE: &str = "\
Trample\n\
Discard a creature card: Put a +1/+1 counter on Lotleth Troll.\n\
{B}: Regenerate Lotleth Troll";

#[test]
fn lotleth_discard_cost_parses_creature_card_filter() {
    assert_eq!(
        parse_oracle_cost("Discard a creature card"),
        AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: Some(TargetFilter::Typed(TypedFilter::creature())),
            selection: CardSelectionMode::Chosen,
            self_scope: DiscardSelfScope::FromHand,
        }
    );
}

#[test]
fn lotleth_discard_activation_only_offers_creature_cards_in_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let troll_id = scenario
        .add_creature_from_oracle(P0, "Lotleth Troll", 2, 1, LOTLETH_ORACLE)
        .id();
    let creature_in_hand = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let _land_in_hand = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: troll_id,
            ability_index: 0,
        })
        .expect("activate Lotleth discard ability");

    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            choices,
            ..
        } => {
            assert!(
                choices.contains(&creature_in_hand),
                "creature card must be eligible: {choices:?}"
            );
            assert_eq!(
                choices.len(),
                1,
                "only creature cards may pay the discard cost, got {choices:?}"
            );
        }
        other => panic!("expected PayCost Discard prompt, got {other:?}"),
    }
}

#[test]
fn lotleth_discard_creature_puts_counter_on_troll() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let troll_id = scenario
        .add_creature_from_oracle(P0, "Lotleth Troll", 2, 1, LOTLETH_ORACLE)
        .id();
    let creature_in_hand = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: troll_id,
            ability_index: 0,
        })
        .expect("activate discard ability");

    runner
        .act(GameAction::SelectCards {
            cards: vec![creature_in_hand],
        })
        .expect("discard creature card to pay cost");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&troll_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "discarding a creature card must put a +1/+1 counter on Lotleth Troll"
    );
    assert_eq!(
        runner.state().objects.get(&creature_in_hand).unwrap().zone,
        Zone::Graveyard
    );
}
