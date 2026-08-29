//! Issue #8101 — selecting many Candelabra targets must not clone the full
//! game state once per non-terminal target choice.

use crate::support::shared_card_db;
use engine::ai_support::legal_actions;
use engine::game::perf_counters;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TARGET_COUNT: usize = 80;

fn candelabra_target_selection() -> (GameRunner, Vec<ObjectId>) {
    let db = shared_card_db().expect("the checked-in fixture contains Candelabra of Tawnos");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let candelabra = scenario.add_real_card(P0, "Candelabra of Tawnos", Zone::Battlefield, db);
    let lands: Vec<_> = (0..TARGET_COUNT)
        .map(|_| scenario.add_real_card(P0, "Forest", Zone::Battlefield, db))
        .collect();

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    for land in &lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..TARGET_COUNT {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: candelabra,
            ability_index: 0,
        })
        .expect("Candelabra activation must begin");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    runner
        .act(GameAction::ChooseX {
            value: TARGET_COUNT as u32,
        })
        .expect("Candelabra X must be announced before choosing targets");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    (runner, lands)
}

#[test]
fn candelabra_many_targets_avoid_nonterminal_legality_clones_and_resolve() {
    let (mut runner, lands) = candelabra_target_selection();
    perf_counters::reset();

    for remaining in (1..=TARGET_COUNT).rev() {
        let actions = legal_actions(runner.state());
        let target_actions: Vec<_> = actions
            .iter()
            .filter_map(|action| match action {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                } => Some(*id),
                _ => None,
            })
            .collect();
        let cancel_count = actions
            .iter()
            .filter(|action| matches!(action, GameAction::CancelCast))
            .count();

        assert_eq!(target_actions.len(), remaining, "target action census");
        assert_eq!(cancel_count, 1, "each target prompt remains cancellable");
        assert_eq!(actions.len(), remaining + 1, "full action census");

        let target = target_actions[0];
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("the engine-enumerated target must be accepted");
    }

    let counters = perf_counters::snapshot();
    assert_eq!(
        counters.state_clone_for_legality,
        TARGET_COUNT as u64 + 1,
        "only each CancelCast and the terminal target choice require raw validation"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "completing all targets must continue to the activation payment step"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the funded activation payment must finalize");
    runner.advance_until_stack_empty();

    assert!(lands
        .iter()
        .all(|land| !runner.state().objects[land].tapped));
}
