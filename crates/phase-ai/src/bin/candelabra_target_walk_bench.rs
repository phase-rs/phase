//! Time Candelabra of Tawnos target walking at realistic fanout.
//!
//! Build/run in an isolated target directory:
//! `CARGO_TARGET_DIR=/tmp/forge-candelabra cargo run -p phase-ai --features scenario-benches --bin candelabra-target-walk-bench`

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::time::{Duration, Instant};

use engine::ai_support::{candidate_actions, legal_actions};
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TARGET_COUNT: usize = 80;
const CANDELABRA_ORACLE: &str = "{X}, {T}: Untap X target lands.";

fn candelabra_target_selection() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let candelabra = scenario
        .add_artifact_from_oracle(P0, "Candelabra of Tawnos", CANDELABRA_ORACLE)
        .id();
    let lands: Vec<_> = (0..TARGET_COUNT)
        .map(|_| scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    for land in lands {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
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
    runner
        .act(GameAction::ChooseX {
            value: TARGET_COUNT as u32,
        })
        .expect("Candelabra X must be announced");
    runner
}

fn main() {
    let mut runner = candelabra_target_selection();
    let mut target_candidates = 0usize;
    let mut cancel_candidates = 0usize;
    let mut raw_action_candidates = 0usize;
    let mut returned_legal_actions = 0usize;
    perf_counters::reset();
    let mut action_generation = Duration::ZERO;
    let mut choose_target_submit = Duration::ZERO;

    for remaining in (1..=TARGET_COUNT).rev() {
        let generation_start = Instant::now();
        let raw = candidate_actions(runner.state());
        let actions = legal_actions(runner.state());
        action_generation += generation_start.elapsed();
        let raw_targets = raw
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
        let raw_cancels = raw
            .iter()
            .filter(|candidate| matches!(candidate.action, GameAction::CancelCast))
            .count();
        let targets: Vec<_> = actions
            .iter()
            .filter_map(|action| match action {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                } => Some(*id),
                GameAction::ChooseTarget { target: None } => {
                    panic!("Candelabra target selection must not offer a targetless choice")
                }
                GameAction::SelectTargets { .. } => {
                    panic!("Candelabra must use the incremental ChooseTarget action")
                }
                GameAction::CancelCast => None,
                unexpected => {
                    panic!("Candelabra target selection offered unexpected action: {unexpected:?}")
                }
            })
            .collect();
        let cancels = actions
            .iter()
            .filter(|action| matches!(action, GameAction::CancelCast))
            .count();
        assert_eq!(raw_targets, remaining, "raw target candidate census");
        assert_eq!(raw_cancels, 1, "raw cancel candidate census");
        assert_eq!(raw.len(), remaining + 1, "raw action candidate census");
        assert_eq!(targets.len(), remaining, "target candidate census");
        assert_eq!(cancels, 1, "cancel candidate census");
        assert_eq!(actions.len(), remaining + 1, "action candidate census");
        target_candidates += targets.len();
        cancel_candidates += cancels;
        raw_action_candidates += raw.len();
        returned_legal_actions += actions.len();
        let submit_start = Instant::now();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(targets[0])),
            })
            .expect("the engine-enumerated target must be accepted");
        choose_target_submit += submit_start.elapsed();
    }

    let counters = perf_counters::snapshot();
    assert_eq!(target_candidates, TARGET_COUNT * (TARGET_COUNT + 1) / 2);
    assert_eq!(cancel_candidates, TARGET_COUNT);
    println!(
        "raw_actions={raw_action_candidates} returned_legal_actions={returned_legal_actions} \
         targets={target_candidates} cancels={cancel_candidates} clones={} \
         action_generation={action_generation:.3?} choose_target_submit={choose_target_submit:.3?}",
        counters.state_clone_for_legality,
    );
}
