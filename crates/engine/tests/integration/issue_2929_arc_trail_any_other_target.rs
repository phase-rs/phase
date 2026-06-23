//! Issue #2929 — Arc Trail: "2 damage to any target and 1 damage to any other
//! target" must not allow both damage on the same target.
//!
//! CR 115.4 + CR 601.2c: separate instances of "target", but "other" requires
//! a different choice from prior targets announced for this spell.

use crate::support::shared_card_db;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn add_mana_for_arc_trail(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
}

#[test]
fn arc_trail_rejects_same_target_for_both_damage_steps() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Opp Bear", 2, 2).id();

    let arc_trail = scenario.add_real_card(P0, "Arc Trail", Zone::Hand, db);

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana_for_arc_trail(&mut runner);

    let card_id = runner.state().objects[&arc_trail].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arc_trail,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Arc Trail must succeed");

    let mut chose_first = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } if !chose_first => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bear)),
                    })
                    .expect("first target selection must succeed");
                chose_first = true;
            }
            WaitingFor::TargetSelection { selection, .. }
                if chose_first && selection.current_slot == 1 =>
            {
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(bear)),
                    "any other target slot must not offer the first chosen target"
                );
                let err = runner.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bear)),
                });
                assert!(
                    err.is_err(),
                    "choosing the same creature for both Arc Trail targets must be rejected"
                );
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    panic!(
        "expected two-step TargetSelection for Arc Trail; last waiting_for: {:?}",
        runner.state().waiting_for
    );
}
