//! Issue #6477: Wandering Archaic's "they may pay {2}. If they don't, you may
//! copy that spell" never copied the opponent's spell, whether or not they
//! paid.
//!
//! The parser fix (`oracle_effect/subject.rs`) is covered by
//! `wandering_archaic_they_pay_as_triggering_player` in `oracle_trigger_tests.rs`,
//! which only proves the lowered AST shape (payer + optionality). These tests
//! drive the real trigger-resolution pipeline: an opponent casts an instant,
//! is offered the {2} payment, and either path — decline-then-copy or
//! pay-and-suppress — is exercised through `apply`, never hand-constructed.
//!
//! Oracle text:
//!   Whenever an opponent casts an instant or sorcery spell, they may pay
//!   {2}. If they don't, you may copy that spell. You may choose new targets
//!   for the copy.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WANDERING_ARCHAIC_ORACLE: &str = "Whenever an opponent casts an instant or sorcery spell, \
    they may pay {2}. If they don't, you may copy that spell. You may choose new targets for the copy.";

const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn floating_mana(units: &[ManaType]) -> Vec<ManaUnit> {
    units
        .iter()
        .map(|ty| ManaUnit::new(*ty, ObjectId(0), false, vec![]))
        .collect()
}

/// Build the shared scenario: P0 controls Wandering Archaic, P1 holds Shock
/// (with a red pip to cast plus two floating generic for the optional tax)
/// and has priority to cast it, targeting a bystander creature under P0's
/// control. The target's toughness (10) is well above any damage total these
/// tests deal (up to 4, from the original Shock plus an accepted copy) so it
/// never dies mid-resolution — a dead target would make the second spell's
/// resolution fizzle on an illegal target (CR 608.2b) and mask the copy
/// under test as a false negative.
fn build_scenario() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Wandering Archaic", 4, 4, WANDERING_ARCHAIC_ORACLE);
    let target = scenario.add_creature(P0, "Target Dummy", 2, 10).id();

    let shock = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, SHOCK_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P1,
        floating_mana(&[ManaType::Red, ManaType::Colorless, ManaType::Colorless]),
    );

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    (runner, shock, target)
}

/// Cast `shock` targeting `target` through the real `apply` pipeline,
/// submitting the target-selection prompt manually. Deliberately does NOT use
/// the `SpellCast`/`CastCommit` fluent builder's `.resolve()` — that driver
/// auto-answers every `OptionalEffectChoice` it encounters with a `Decline`
/// default (see `drive_resolution`'s `ResolutionPolicy`), which would silently
/// drive past both of Wandering Archaic's optional prompts before the test
/// ever got a chance to intercept them.
fn cast_shock(runner: &mut GameRunner, shock: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&shock].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: shock,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Shock");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(target)],
                })
                .expect("select the target creature for Shock");
        }
        other => panic!("expected TargetSelection after casting Shock, got {other:?}"),
    }
}

/// Advance the engine until the {2} optional-payment prompt (or the stack
/// empties without one, which would itself be the bug this regresses).
fn drive_to_payment_prompt(runner: &mut GameRunner) {
    for _ in 0..100 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// Drain to an idle, empty stack after every decision this test cares about
/// has already been made explicitly by the caller (the {2} payment, and —
/// on decline — the follow-up copy choice). A further `OptionalEffectChoice`
/// here is UNEXPECTED and fails loudly rather than being silently declined:
/// a regression that offers the copy even after the opponent paid (or offers
/// it twice) must not be swallowed into the same "2 damage" outcome a
/// correctly-suppressed copy produces — that would make the paid-path test
/// pass whether or not the copy was actually suppressed, defeating its whole
/// point. `CopyRetarget` is the one legitimate additional prompt (issued only
/// once a copy has already been created), so it alone is handled here.
fn drive_to_idle(runner: &mut GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the copy's original target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!(
                    "unexpected optional-effect prompt during drive_to_idle: {:?} — \
                     every decision this test exercises must already be settled by now",
                    runner.state().waiting_for
                );
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
}

/// The opponent declines the {2} payment, so the "if they don't" branch
/// offers Wandering Archaic's controller the copy. Accepting must put a
/// second Shock on the stack under the controller's control, dealing a
/// second 2 damage to the target (4 total) once both the original and the
/// copy have resolved.
#[test]
fn wandering_archaic_declined_payment_lets_controller_copy_spell() {
    let (mut runner, shock, target) = build_scenario();

    cast_shock(&mut runner, shock, target);
    drive_to_payment_prompt(&mut runner);

    // The payment choice must be offered to the casting opponent (P1), not
    // Wandering Archaic's controller (P0) — the defect this regresses. "They"
    // in "they may pay" anaphors to the opponent named by the trigger
    // condition (the parser fact asserted directly by
    // `wandering_archaic_they_pay_as_triggering_player`); CR 608.2d only
    // governs that an effect's offered choice is announced by the player
    // applying the effect, not who that player is.
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                player, P1,
                "the {{2}} payment choice must be offered to the casting opponent"
            );
        }
        other => panic!("expected the {{2}} optional payment prompt, got {other:?}"),
    }
    let p1_mana_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .mana_pool
        .total();

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P1 declines the {2} payment");

    // Declining must not spend the opponent's mana.
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .mana_pool
            .total(),
        p1_mana_before,
        "declining the payment must not deduct the opponent's mana"
    );

    // The "if they don't" branch now offers the copy to Wandering Archaic's
    // controller (P0), not the opponent.
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                player, P0,
                "the \"you may copy\" choice belongs to Wandering Archaic's controller"
            );
        }
        other => panic!("expected the \"you may copy\" prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 accepts the copy");

    drive_to_idle(&mut runner);

    assert!(
        runner.state().stack.is_empty(),
        "resolution must settle with an empty stack"
    );
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        4,
        "the original Shock plus the accepted copy must deal 2 + 2 = 4 damage"
    );
}

/// The opponent paying the {2} tax must suppress the copy entirely — only
/// the original Shock resolves. `drive_to_idle` panics on any further
/// `OptionalEffectChoice`, so a regression that still offers the copy after
/// payment fails here instead of coincidentally landing on the same "2
/// damage" outcome a correctly-suppressed copy produces.
#[test]
fn wandering_archaic_paid_payment_suppresses_copy() {
    let (mut runner, shock, target) = build_scenario();

    cast_shock(&mut runner, shock, target);
    drive_to_payment_prompt(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(player, P1),
        other => panic!("expected the {{2}} optional payment prompt, got {other:?}"),
    }
    let p1_mana_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .mana_pool
        .total();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 pays the {2}");

    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .mana_pool
            .total(),
        p1_mana_before - 2,
        "paying must deduct exactly {{2}} generic from the opponent's pool"
    );

    drive_to_idle(&mut runner);

    assert!(
        runner.state().stack.is_empty(),
        "resolution must settle with an empty stack"
    );
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        2,
        "paying the tax must suppress the copy — only the original Shock's 2 damage lands"
    );
}
