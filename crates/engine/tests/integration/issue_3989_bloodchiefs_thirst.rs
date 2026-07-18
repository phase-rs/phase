//! Regression for issue #3989: kicked Bloodchief's Thirst must be castable on
//! opponent creatures with mana value greater than 2 (e.g. Pyrogoyf, MV 4).
//!
//! https://github.com/phase-rs/phase/issues/3989

use engine::game::casting::{can_cast_object_now, spell_has_legal_targets};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn kicked_mana_pool(spell: ObjectId) -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Black, spell, false, vec![]),
        ManaUnit::new(ManaType::Black, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
    ]
}

#[test]
fn bloodchiefs_thirst_castable_on_opponent_pyrogoyf_when_kicked() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pyrogoyf = scenario.add_real_card(P1, "Pyrogoyf", Zone::Battlefield, db);
    let thirst = scenario.add_real_card(P0, "Bloodchief's Thirst", Zone::Hand, db);
    scenario.with_mana_pool(P0, kicked_mana_pool(thirst));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let thirst_obj = &runner.state().objects[&thirst];
    assert!(
        spell_has_legal_targets(runner.state(), thirst_obj, P0),
        "Bloodchief's Thirst must be castable when only kicked targets exist"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, thirst),
        "can_cast_object_now must admit kicked-only targets"
    );

    let card_id = runner.state().objects[&thirst].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: thirst,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Bloodchief's Thirst should start");

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("pay kicker");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(pyrogoyf)),
                    "Pyrogoyf must be a legal kicked target: {:?}",
                    target_slots[0].legal_targets
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(pyrogoyf)],
                    })
                    .expect("target Pyrogoyf");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment should auto-finalize");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority during cast");
            }
            other => panic!(
                "unexpected waiting_for during cast: {other:?}, stack={:?}",
                runner.state().stack.len()
            ),
        }
    }

    assert!(
        runner.state().players[P0.0 as usize]
            .hand
            .iter()
            .all(|&id| id != thirst),
        "Bloodchief's Thirst must leave hand after casting"
    );

    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &runner.state().stack[0].kind
    else {
        panic!(
            "expected spell on stack, got {:?}",
            runner.state().stack[0].kind
        );
    };
    assert!(
        ability.context.additional_cost_paid,
        "kicked spell on stack must have additional_cost_paid set"
    );
    assert!(
        ability.targets.contains(&TargetRef::Object(pyrogoyf)),
        "stack spell must target Pyrogoyf, got {:?}",
        ability.targets
    );
}

/// Resolve the stack to empty by passing priority.
fn resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// dq-f (Layer 3, PR #6143 finding #2): kicked Bloodchief's Thirst must
/// actually DESTROY the chosen mana-value-4+ target on resolution, not just
/// accept it as a legal cast-time target (issue #3989 above only proves the
/// latter). Before the fix, `apply_instead_swap` cloned the PARENT node —
/// whose targets CR 608.2b emptied because the narrow base filter (mana
/// value 2 or less) rejects Pyrogoyf — discarding the override sub's own
/// validated `[Pyrogoyf]` target. The swapped-in `Destroy` effect then ran
/// with zero targets: a silent no-op that left Pyrogoyf alive.
///
/// A second mana-value-3+ creature keeps the broad (kicked) branch's target
/// slot genuinely interactive: with only Pyrogoyf on the battlefield,
/// `auto_select_targets_for_ability` (ability_utils.rs) would auto-resolve
/// the sole legal target inline and skip `WaitingFor::TargetSelection`
/// entirely, so the reach-guard below would never execute.
#[test]
fn kicked_bloodchiefs_thirst_destroys_high_mv_target_on_resolution() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pyrogoyf = scenario.add_real_card(P1, "Pyrogoyf", Zone::Battlefield, db);
    let mut decoy = scenario.add_creature(P1, "MV Three Decoy", 3, 3);
    decoy.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let decoy = decoy.id();
    let thirst = scenario.add_real_card(P0, "Bloodchief's Thirst", Zone::Hand, db);
    scenario.with_mana_pool(P0, kicked_mana_pool(thirst));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&thirst].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: thirst,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Bloodchief's Thirst should start");

    let mut reached_target_selection = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("pay kicker");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                // Positive reach-guard: both mana-value-3+ creatures must be
                // legal broad-branch targets before the spell is targeted,
                // proving the pause was reached with the kicked filter live
                // (not an auto-resolved single-target skip).
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(pyrogoyf)),
                    "Pyrogoyf must be a legal kicked target: {:?}",
                    target_slots[0].legal_targets
                );
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(decoy)),
                    "the decoy creature must also be a legal kicked target: {:?}",
                    target_slots[0].legal_targets
                );
                reached_target_selection = true;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(pyrogoyf)],
                    })
                    .expect("target Pyrogoyf");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment should auto-finalize");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority during cast");
            }
            other => panic!(
                "unexpected waiting_for during cast: {other:?}, stack={:?}",
                runner.state().stack.len()
            ),
        }
    }
    assert!(
        reached_target_selection,
        "test setup must force an interactive target selection pause"
    );

    resolve_stack(&mut runner);

    assert!(
        !runner.state().battlefield.contains(&pyrogoyf),
        "kicked Bloodchief's Thirst must destroy the mana-value-4 target on resolution"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .graveyard
            .contains(&pyrogoyf),
        "the destroyed Pyrogoyf must land in its owner's graveyard"
    );
}
