//! Regression (issue #564): Wishclaw Talisman's activated ability searches,
//! then prompts to choose an opponent, then that opponent must gain control
//! of the Talisman — not the activator.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const WISHCLAW_ACTIVATED: &str =
    "{1}, {T}, Remove a wish counter from ~: Search your library for a \
     card, put it into your hand, then shuffle. An opponent gains control of ~. \
     Activate only during your turn.";

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn choose_opponent(runner: &mut GameRunner, opponent: engine::types::PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*choice_type, ChoiceType::Opponent);
            assert!(
                options.contains(&opponent.0.to_string()),
                "opponent must be legal; options={options:?}"
            );
        }
        other => panic!("expected NamedChoice for opponent, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: opponent.0.to_string(),
        })
        .expect("ChooseOption(opponent) must succeed");
}

fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).ok();
            }
            _ => break,
        }
    }
}

#[test]
fn issue_564_wishclaw_activation_passes_control_to_chosen_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let talisman_id = scenario
        .add_creature(P0, "Wishclaw Talisman", 0, 0)
        .as_artifact()
        .from_oracle_text(WISHCLAW_ACTIVATED)
        .id();

    scenario.with_counter(talisman_id, CounterType::Generic("wish".to_string()), 1);
    scenario.with_mana_pool(P0, floating_colorless(1));

    let mut runner = scenario.build();
    runner
        .activate(talisman_id, 0)
        .search_first_legal()
        .resolve();

    choose_opponent(&mut runner, P1);
    drain_stack(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects.get(&talisman_id).unwrap().controller,
        P1,
        "chosen opponent must control Wishclaw after the ability resolves; waiting_for={:?}",
        state.waiting_for
    );
}
