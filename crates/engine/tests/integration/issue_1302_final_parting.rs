//! Regression for issue #1302: Final Parting must split search results between
//! hand and graveyard instead of putting both cards into hand.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FINAL_PARTING: &str = "Search your library for two cards. Put one into your \
hand and the other into your graveyard. Then shuffle.";

fn add_final_parting_mana(runner: &mut engine::game::scenario::GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..3 {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..2 {
        pool.add(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    }
}

#[test]
fn issue_1302_final_parting_partitions_hand_and_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let parting = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Parting", false, FINAL_PARTING)
        .id();
    let card_a = scenario.add_card_to_library_top(P0, "Card A");
    let card_b = scenario.add_card_to_library_top(P0, "Card B");

    let mut runner = scenario.build();
    add_final_parting_mana(&mut runner);

    let card_id = runner.state().objects[&parting].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: parting,
            card_id,
            targets: vec![],
        })
        .expect("Final Parting cast should succeed");
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(cards.contains(&card_a) && cards.contains(&card_b));
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![card_a, card_b],
        })
        .expect("select both cards");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::SearchPartitionChoice { .. }
    ) {
        runner
            .act(GameAction::SelectCards {
                cards: vec![card_a],
            })
            .expect("pick hand card");
    }

    runner.advance_until_stack_empty();

    let zones: Vec<_> = [card_a, card_b]
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    assert!(
        zones.contains(&Zone::Hand) && zones.contains(&Zone::Graveyard),
        "Final Parting must send one card to hand and one to graveyard, got {zones:?}"
    );
}
