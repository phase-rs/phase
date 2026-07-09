//! Coverage for **unless-pay routing through the single payment authority**
//! (cost-payment unification, Phase 3).
//!
//! Phase 3 routes the deterministic `handle_unless_payment` arms (Mana,
//! PayLife, PayEnergy) through `game::costs::pay_ability_cost_for_resolution`
//! at `PaymentScope::Resolution`, replacing the bespoke inline payment code
//! that used to live in `engine_payment_choices.rs`. The interactive arms
//! (Discard/Sacrifice/ReturnToHand/library-exile) stay adapter-side because
//! their resources are acquired via WaitingFor round-trips before payment.
//!
//! These tests drive the REAL pipeline: a creature built from authoritative
//! Oracle text through `GameScenario::add_creature_from_oracle` (which runs
//! `synthesize_all`), advanced through the upkeep step so the "tap this
//! creature unless you pay [cost]" trigger fires and surfaces the
//! `WaitingFor::UnlessPayment` prompt, then
//! `GameAction::PayUnlessCost { pay: true }`.
//!
//! Test character (honest labeling): the two PayLife tests are real-pipeline
//! BEHAVIOR-PRESERVATION tests — the pre-Phase-3 inline arm used the same
//! `pay_life_as_cost` with identical insufficiency handling, so they pin the
//! routed path's equivalence rather than discriminating routing. The Mana
//! test IS discriminating: Phase 3 changed the Mana arm's failure semantics
//! (old: auto-tap mutated live state then returned `ActionNotAllowed`,
//! rejecting the action and retaining the prompt; new: the authority
//! pre-flights affordability on a clone and maps unaffordable to `Failed` →
//! the "unless" branch — CR 118.12 can't-pay ≡ didn't-pay), so the old code
//! fails that test.
//!
//! CR ANCHORS (verified against docs/MagicCompRules.txt):
//!   * CR 118.3   — "A player can't pay a cost without having the necessary resources to pay it fully."
//!   * CR 118.12  — "[Do something]. If [a player] [does/doesn't/can't] …"
//!   * CR 118.12a — "[Do something] unless [a player does something else]."
//!   * CR 119.4   — "If a player pays life … the player loses that much life."
//!   * CR 601.2h  — "Partial payments are not allowed. Unpayable costs can't be paid."
//!
//! CARD TEXT (verified from this engine's card-data for Sangrophage):
//!   "At the beginning of your upkeep, tap this creature unless you pay 2 life."

use engine::game::scenario::GameScenario;
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const SANGROPHAGE_ORACLE: &str =
    "At the beginning of your upkeep, tap this creature unless you pay 2 life.";

/// Build a Sangrophage on PlayerId(0)'s battlefield with `life` life, then
/// advance to the controller's upkeep and resolve the upkeep trigger so the
/// engine pauses at the PayLife `UnlessPayment` prompt.
///
/// Returns `(runner, sangrophage_id)` paused at `WaitingFor::UnlessPayment`.
fn setup_at_unless_prompt(life: i32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    // Start at Untap so a single `auto_advance` (driven by `advance_to_upkeep`)
    // ticks into Upkeep and fires the synthesized upkeep trigger (CR 503.1a).
    scenario.at_phase(Phase::Untap);
    scenario.with_life(P0, life);

    let sangrophage = scenario
        .add_creature_from_oracle(P0, "Sangrophage", 2, 2, SANGROPHAGE_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.resolve_top();

    // The trigger surfaced a PayLife { 2 } unless-cost prompt.
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "controller is the unless-payer");
            assert!(
                matches!(
                    cost,
                    AbilityCost::PayLife {
                        amount: engine::types::ability::QuantityExpr::Fixed { value: 2 }
                    }
                ),
                "expected a PayLife {{ 2 }} unless-cost, got {cost:?}"
            );
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    (runner, sangrophage)
}

/// CR 118.3 + CR 118.12a + CR 601.2h: with only 1 life, the 2-life unless-cost
/// is unpayable — the authority returns `Failed`, so the effect (tap) happens
/// and NO life is deducted (no partial payment). Behavior-preservation pin:
/// the pre-Phase-3 inline arm handled insufficiency identically via
/// `pay_life_as_cost`; this pins the routed path's equivalence.
#[test]
fn unless_pay_life_insufficient_routes_through_authority_failure_path() {
    let (mut runner, sangrophage) = setup_at_unless_prompt(1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay an unpayable life cost must be accepted");

    // CR 118.12a: the unless-cost was unpayable, so the effect happened.
    assert!(
        runner.state().objects[&sangrophage].tapped,
        "an unpayable life cost must let the tap effect happen"
    );
    // CR 601.2h: partial payments are not allowed — life is untouched.
    assert_eq!(
        runner.life(P0),
        1,
        "no life may be deducted when the cost is unpayable"
    );
}

/// CR 118.3 + CR 118.12: DISCRIMINATING test for the Phase 3 Mana-arm change.
/// With no mana available, `PayUnlessCost { pay: true }` on a "{2}" unless-cost
/// must be ACCEPTED and fall through to the "unless" branch (can't-pay ≡
/// didn't-pay): the effect (tap) happens, the prompt clears, and no live state
/// was mutated by a failed payment attempt. The pre-Phase-3 inline arm instead
/// auto-tapped live state and returned `EngineError::ActionNotAllowed`,
/// rejecting the action and leaving the prompt stuck — the old code fails
/// every assertion below.
#[test]
fn unless_pay_mana_unaffordable_falls_through_to_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let creature = scenario
        .add_creature_from_oracle(
            P0,
            "Test Tithe Beast",
            2,
            2,
            "At the beginning of your upkeep, tap this creature unless you pay {2}.",
        )
        .id();

    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.resolve_top();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "controller is the unless-payer");
            assert!(
                matches!(cost, AbilityCost::Mana { .. }),
                "expected a Mana unless-cost, got {cost:?}"
            );
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    // No lands, no pool: the {2} cost is unaffordable. The pay attempt must
    // be accepted (not rejected with ActionNotAllowed) and resolve the
    // punishment effect.
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay an unaffordable mana unless-cost must be accepted");

    assert!(
        runner.state().objects[&creature].tapped,
        "an unaffordable mana cost must let the tap effect happen (CR 118.12 can't-pay)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the unless-prompt must clear instead of sticking on a rejected action"
    );
}

/// CR 119.4 + CR 118.12: with enough life, the authority pays — life is
/// deducted by exactly the cost and the effect (tap) is suppressed.
#[test]
fn unless_pay_life_sufficient_routes_through_authority_paid_path() {
    let (mut runner, sangrophage) = setup_at_unless_prompt(20);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the life cost must be accepted");

    // CR 118.12a: paying the unless-cost suppresses the effect.
    assert!(
        !runner.state().objects[&sangrophage].tapped,
        "paying the life cost must suppress the tap effect"
    );
    // CR 119.4: paying 2 life loses exactly 2 life.
    assert_eq!(
        runner.life(P0),
        18,
        "paying the unless-cost must deduct exactly 2 life"
    );
}
