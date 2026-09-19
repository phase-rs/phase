//! Wickerfolk Indomitable — graveyard cast permission with a COMPOSITE
//! additional cost: 2 life AND sacrificing an artifact or creature
//! (CR 601.2f + CR 118.8).
//!
//! "You may cast this card from your graveyard by paying 2 life and sacrificing
//! an artifact or creature in addition to paying its other costs."
//!
//! The permission keeps the spell's printed mana cost (CR 601.2f: an ADDITIONAL
//! cost, not an alternative one) and requires BOTH legs of the ` and `-joined
//! gerund rider (CR 118.8a: any number of additional costs may apply). These
//! tests drive the real cast pipeline through the scenario `GameRunner` /
//! `SpellCast` driver and assert both legs are actually paid.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::EngineError;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WICKERFOLK_ORACLE: &str = "You may cast this card from your graveyard by paying 2 life and sacrificing an artifact or creature in addition to paying its other costs.";

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

/// Wickerfolk's printed cost is {3}{B}; the composite rider behavior under
/// test does not depend on the mana amount, so the stand-in uses {1} and a
/// single colorless unit to keep the pool minimal.
fn stage_wickerfolk(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Wickerfolk Indomitable", 4, 3)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text(WICKERFOLK_ORACLE)
        .id()
}

/// CR 601.2f + CR 118.8 + CR 119.4 + CR 701.21a: end-to-end — casting
/// Wickerfolk from the graveyard pays its {1} AND 2 life AND sacrifices the
/// artifact. DISCRIMINATING: before per-component de-conjugation the sacrifice
/// leg stayed `Unimplemented`, so the permission paid only life — the fodder
/// would stay on the battlefield and the cast would be rejected at payment.
#[test]
fn wickerfolk_graveyard_cast_pays_life_and_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut scenario);
    let fodder = scenario
        .add_creature(P0, "Fodder Artifact", 0, 0)
        .as_artifact()
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&wickerfolk),
        "the graveyard permission must surface Wickerfolk as castable"
    );
    let outcome = runner.cast(wickerfolk).sacrifice_with(&[fodder]).resolve();

    outcome.assert_life_delta(P0, -2);
    outcome.assert_zone(&[fodder], Zone::Graveyard);
    outcome.assert_zone(&[wickerfolk], Zone::Battlefield);
}

/// CR 601.2h + CR 118.3: the sacrifice leg is a real cost gate — with no
/// artifact or creature P0 controls, the composite additional cost is
/// unpayable and the graveyard cast must not be offered. Reach-guard: the same
/// setup with a controlled fodder IS castable, so the block is affordability-
/// specific, not a blanket refusal.
#[test]
fn wickerfolk_graveyard_cast_blocked_without_artifact_or_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut scenario);
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, wickerfolk),
        "with no artifact or creature to sacrifice the composite additional cost \
         (CR 601.2h) is unpayable, so the graveyard cast must not be offered"
    );

    let mut reachable = GameScenario::new();
    reachable.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut reachable);
    reachable.add_creature(P0, "Fodder", 1, 1);
    reachable.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let runner = reachable.build();
    assert!(
        can_cast_object_now(runner.state(), P0, wickerfolk),
        "reach-guard: a fodder P0 controls makes the same cast legal"
    );
}

/// CR 701.21a: only permanents the caster controls can be sacrificed — an
/// opponent's creature is NOT a legal payment. Reach-guard: the same creature
/// under P0's control makes the cast legal, so the block is control-specific.
#[test]
fn wickerfolk_graveyard_cast_requires_controlling_the_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut scenario);
    scenario.add_creature(P1, "Opponent Fodder", 1, 1);
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, wickerfolk),
        "an opponent's creature is not a legal sacrifice for P0 (CR 701.21a), so \
         the graveyard cast must not be offered"
    );

    let mut reachable = GameScenario::new();
    reachable.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut reachable);
    reachable.add_creature(P0, "Own Fodder", 1, 1);
    reachable.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let runner = reachable.build();
    assert!(
        can_cast_object_now(runner.state(), P0, wickerfolk),
        "reach-guard: the same creature under P0's control is a legal sacrifice"
    );
}

/// CR 601.2f + CR 601.2h + CR 701.21a: the sacrifice leg is re-checked when it
/// is actually paid, not only when the cast is declared. If the declared
/// fodder leaves the battlefield between declaration and payment (the hostile
/// ordering the composite cost must not paper over), selecting the now-gone
/// permanent is refused and the cast does not commit for free.
///
/// The fluent `SpellCast` driver announces and pays inside one loop, so this
/// test steps the pipeline manually through `GameRunner::act` and removes the
/// fodder at the exposed `WaitingFor::PayCost` seam via the production
/// `zone_pipeline::move_object_for_test` entry point — the only point in the
/// scenario harness where a post-declaration removal is representable.
#[test]
fn wickerfolk_graveyard_cast_rejected_when_declared_fodder_leaves_before_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = stage_wickerfolk(&mut scenario);
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&wickerfolk].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wickerfolk,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("declaring the graveyard cast must be accepted");

    match &runner.state().waiting_for {
        WaitingFor::PayCost { choices, .. } => assert!(
            choices.contains(&fodder),
            "reach-guard: the declared fodder must be the sacrifice prompt's eligible choice"
        ),
        other => panic!(
            "expected the composite cost's sacrifice prompt, got {other:?} — the hostile \
             removal needs the PayCost seam"
        ),
    }

    // Hostile timing: the fodder leaves the battlefield after the cast was
    // declared but before the sacrifice is paid. Route the move through the
    // production zone-change pipeline (CR 614.1a replacement handling + the
    // delivery tail) rather than a raw zone write, so the fixture proves the
    // hostile event the engine actually produces. A vanilla fodder has no
    // applicable replacement, so the move must not pause for a choice.
    let mut events = Vec::new();
    let paused_for_choice = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(fodder, Zone::Graveyard, fodder),
        &mut events,
    );
    assert!(
        !paused_for_choice,
        "reach-guard: a vanilla fodder move must not pause for a replacement choice"
    );

    let rejected = runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect_err("sacrificing a permanent that already left the battlefield must be refused");
    assert!(
        matches!(rejected, EngineError::ActionNotAllowed(_)),
        "expected ActionNotAllowed for the gone permanent, got {rejected:?}"
    );
    assert_eq!(
        runner.state().objects[&wickerfolk].zone,
        Zone::Graveyard,
        "the cast must not commit for free when the composite payment is refused"
    );
    assert!(
        !runner.state().battlefield.contains(&wickerfolk),
        "the refused cast must not put Wickerfolk onto the battlefield"
    );
    // NOTE: the engine announces the spell onto the stack (CR 601.2a) before
    // the cost is paid and does not rewind that announcement when a synthetic
    // `SelectCards` rejection fails mid-payment, so `stack.is_empty()` is not
    // assertable on this failure path. The load-bearing claim is the refusal:
    // the now-gone permanent is rejected and the card never leaves the
    // graveyard, so no free cast happens.
}

/// CR 601.2b: the composite additional cost belongs to the graveyard
/// permission only — a normal hand cast of the same card loses no life and
/// sacrifices nothing. This is the control for the end-to-end test above.
#[test]
fn wickerfolk_hand_cast_pays_no_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let wickerfolk = scenario
        .add_creature_to_hand(P0, "Wickerfolk Indomitable", 4, 3)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text(WICKERFOLK_ORACLE)
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    let outcome = runner.cast(wickerfolk).resolve();

    outcome.assert_life_delta(P0, 0);
    outcome.assert_zone(&[wickerfolk], Zone::Battlefield);
}
