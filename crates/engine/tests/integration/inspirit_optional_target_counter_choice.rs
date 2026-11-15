//! Sibling of the Elspeth Resplendent case in issue #7817: a counter-kind
//! choice hanging under an OPTIONAL target slot.
//!
//! Oracle text (the station-1 trigger):
//! > At the beginning of combat on your turn, put your choice of a +1/+1
//! > counter or two charge counters on up to one other target artifact.
//!
//! "up to one **other** target artifact" — declining the target, or having no
//! other artifact to choose, must place nothing. A bare parent anaphor with no
//! chosen object falls back to the ability's own source, which would put the
//! counters on the Spacecraft the card excludes by name.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Verbatim from `client/public/card-data.json`, the station-1 line only.
const INSPIRIT_TRIGGER: &str = "At the beginning of combat on your turn, put your choice of a \
     +1/+1 counter or two charge counters on up to one other target artifact.";

/// CR 115.6: "a spell or ability that requires targets may allow zero targets
/// to be chosen", so nothing ever fills the referent here.
#[test]
fn inspirit_with_no_other_artifact_puts_no_counter_on_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let inspirit = scenario
        .add_artifact_from_oracle(P0, "Inspirit, Flagship Vessel", INSPIRIT_TRIGGER)
        .id();
    let mut runner = scenario.build();

    // No other artifact exists, so the only legal announcement is zero targets.
    runner.pass_both_players();

    let mut offered_choice = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets { targets: vec![] })
                    .expect("zero targets is legal for \"up to one\"");
            }
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                offered_choice = true;
                let index = branch_descriptions
                    .iter()
                    .position(|d| d.to_lowercase().contains("+1/+1"))
                    .unwrap_or(0);
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("answer the counter choice");
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.advance_until_stack_empty();
            }
        }
    }

    // Reach-guard, not decoration: a fresh artifact carries no counters, so the
    // assertion below would also hold if the trigger never resolved at all.
    assert!(
        offered_choice,
        "the trigger must have resolved and asked the counter choice"
    );
    let counters = &runner.state().objects[&inspirit].counters;
    assert!(
        counters.is_empty(),
        "no counter of this ability may land on the source it excludes by name, \
         got {counters:?}"
    );
}
