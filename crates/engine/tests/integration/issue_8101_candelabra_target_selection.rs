//! Issue #8101 — selecting many Candelabra targets must not clone the full
//! game state once per non-terminal target choice.

use crate::support::shared_card_db;
use engine::ai_support::{candidate_actions, legal_actions};
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
    perf_counters::reset();
    let (mut runner, lands) = candelabra_target_selection();
    let initial_target_walk_counters = perf_counters::homogeneous_target_walk_cache_snapshot();
    assert_eq!(
        initial_target_walk_counters.initializations, 1,
        "the homogeneous target walk must enumerate its initial legal set once"
    );
    perf_counters::reset();
    let mut raw_action_candidates = 0;
    let mut returned_legal_actions = 0;
    let mut target_candidates = 0;
    let mut cancel_candidates = 0;

    for remaining in (1..=TARGET_COUNT).rev() {
        perf_counters::reset();
        let raw = candidate_actions(runner.state());
        let actions = legal_actions(runner.state());
        let raw_target_count = raw
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(_)),
                    }
                )
            })
            .count();
        let raw_cancel_count = raw
            .iter()
            .filter(|candidate| matches!(candidate.action, GameAction::CancelCast))
            .count();
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

        assert_eq!(raw_target_count, remaining, "raw target candidate census");
        assert_eq!(raw_cancel_count, 1, "raw cancel candidate census");
        assert_eq!(raw.len(), remaining + 1, "raw action candidate census");
        assert_eq!(target_actions.len(), remaining, "target action census");
        assert_eq!(cancel_count, 1, "each target prompt remains cancellable");
        assert_eq!(actions.len(), remaining + 1, "full action census");
        raw_action_candidates += raw.len();
        returned_legal_actions += actions.len();
        target_candidates += target_actions.len();
        cancel_candidates += cancel_count;

        let target = target_actions[0];
        let cache_advances_before_choice =
            perf_counters::homogeneous_target_walk_cache_snapshot().advances;
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("the engine-enumerated target must be accepted");
        let action_counters = perf_counters::snapshot();
        assert_eq!(
            action_counters.state_clone_for_legality,
            if remaining > 1 { 1 } else { 2 },
            "only cancellation and the terminal target choice may clone state for legality"
        );
        assert_eq!(
            perf_counters::homogeneous_target_walk_cache_snapshot().advances
                - cache_advances_before_choice,
            if remaining > 1 { 1 } else { 0 },
            "each non-final target selection must consume the cached legal set once"
        );
    }

    assert_eq!(raw_action_candidates, 3320, "raw action candidate census");
    assert_eq!(returned_legal_actions, 3320, "returned legal action census");
    assert_eq!(target_candidates, 3240, "target candidate census");
    assert_eq!(cancel_candidates, 80, "cancel candidate census");
    runner
        .act(GameAction::PassPriority)
        .expect("the funded activation must accept priority passing");
    runner.advance_until_stack_empty();

    assert!(lands
        .iter()
        .all(|land| !runner.state().objects[land].tapped));
}
