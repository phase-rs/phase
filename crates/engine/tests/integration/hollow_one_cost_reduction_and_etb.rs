//! Runtime pipeline regression — Hollow One (cost reduction + self-sacrifice ETB).
//!
//! Oracle:
//!   Trample
//!   This spell costs {2} less to cast for each card you've cycled or discarded
//!   this turn.
//!   When Hollow One enters the battlefield, if it was cast for {5} or less,
//!   sacrifice it.
//!
//! Two coupled fixes are exercised end to end through the real cast pipeline:
//!
//!   SECTION A — the compound "cycled or discarded this turn" reduction phrase
//!   must lower to `CardsDiscardedThisTurn { Controller }` (a per-controller
//!   count), NOT the generic cross-player `ObjectCount{Card}` misparse that was
//!   the root of the reported over-reduction bug. `record_discard` is the shared
//!   counter both discards and cycling (CR 702.29c — a cycled card is discarded
//!   as its cost, still emitting the Discarded event) feed, so the runtime reads
//!   the controller's tally from `cards_discarded_this_turn_by_player`.
//!
//!   SECTION B — the ETB "if it was cast for {5} or less" intervening-if must
//!   gate on the ACTUAL mana spent to cast Hollow One (`ManaSpentToCast
//!   { SelfObject, Total }`, populated during cost payment), so the reduction
//!   from Section A directly changes whether the sacrifice fires.
//!
//! COST ASSUMPTION (offline environment): Hollow One's printed mana cost is
//! `{5}` (a colorless artifact creature, 4/4, Trample). This test builds the
//! card from that cost. `card-data.json` was unavailable in the authoring
//! session, so the {5} base cost was NOT re-verified against the committed card
//! fixture — it reflects the card's well-established printed cost and is called
//! out here explicitly for the maintainer/CI to confirm.
//!
//! DISCARD-COUNT SEEDING: qualifying discard/cycle events are seeded directly
//! into `cards_discarded_this_turn_by_player` (the exact field the runtime
//! resolver reads, and the same technique used by the Dream Salvage integration
//! test), rather than driving live cycling activations. This isolates the parser
//! + resolution fixes under test from the orthogonal cycling-activation plumbing.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HOLLOW_ONE_TEXT: &str = "Trample\n\
This spell costs {2} less to cast for each card you've cycled or discarded this turn.\n\
When Hollow One enters the battlefield, if it was cast for {5} or less, sacrifice it.";

/// Hollow One's printed mana cost — {5} generic (see COST ASSUMPTION above).
fn hollow_one_cost() -> ManaCost {
    ManaCost::generic(5)
}

fn build_hollow_one(scenario: &mut GameScenario, controller: engine::types::player::PlayerId) -> ObjectId {
    scenario
        .add_creature_to_hand(controller, "Hollow One", 4, 4)
        .with_mana_cost(hollow_one_cost())
        .from_oracle_text(HOLLOW_ONE_TEXT)
        .id()
}

/// N colorless mana in `owner`'s pool.
fn colorless(owner: ObjectId, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, owner, false, Vec::new()))
        .collect()
}

// ── SECTION A: cost reduction counts the controller's own events ───────────────

/// SECTION A, DISCRIMINATING: the controller cycled one card and discarded one
/// other card this turn (2 qualifying events → {2}×2 = {4} reduction), so Hollow
/// One costs {5} − {4} = {1}. A pool of exactly {1} suffices *only if* the
/// reduction applies. If the compound phrase failed to lower (or lowered to the
/// wrong scope), the full {5} would be due and this cast would be illegal.
#[test]
fn hollow_one_reduction_applies_for_controller_cycled_or_discarded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hollow = build_hollow_one(&mut scenario, P0);
    scenario.with_mana_pool(P0, colorless(hollow, 1));

    let mut runner = scenario.build();
    // 2 qualifying controller events this turn (one cycle + one discard).
    runner
        .state_mut()
        .cards_discarded_this_turn_by_player
        .insert(P0, 2);

    assert!(
        can_cast_object_now(runner.state(), P0, hollow),
        "with 2 controller cycled/discarded events the {{4}} reduction applies, so Hollow One \
         is castable for {{1}}"
    );
}

/// SECTION A, ROOT-CAUSE REGRESSION: only an OPPONENT cycled/discarded this turn;
/// the controller had zero qualifying events. The controller-scoped count must
/// be 0 → no reduction → the full {5} is due, and a {1} pool is insufficient.
///
/// This is the assertion that actually proves the original cross-player
/// `ObjectCount` bug is fixed: the buggy parse counted every card object
/// (including the opponent's), granting an illegitimate reduction. A revert of
/// the Section A fix would let this cast through and flip the assertion.
#[test]
fn hollow_one_reduction_not_applied_for_opponent_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hollow = build_hollow_one(&mut scenario, P0);
    scenario.with_mana_pool(P0, colorless(hollow, 1));

    let mut runner = scenario.build();
    // The opponent discarded/cycled 3 cards; the controller (P0) discarded none.
    runner
        .state_mut()
        .cards_discarded_this_turn_by_player
        .insert(P1, 3);

    assert!(
        !can_cast_object_now(runner.state(), P0, hollow),
        "the opponent's cycled/discarded cards must NOT reduce the controller's Hollow One; \
         with zero controller events the full {{5}} is due and a {{1}} pool is insufficient"
    );
}

// ── SECTION B: the ETB sacrifice gates on the ACTUAL mana spent ────────────────

/// SECTION B, CRUX: cast Hollow One after 2 qualifying events (reduction applies,
/// paying {1}), resolve, and let the ETB trigger process. Because {1} ≤ {5}, the
/// self-sacrifice fires and Hollow One ends up in the graveyard rather than on
/// the battlefield. The condition reads `ManaSpentToCast{SelfObject, Total}`,
/// which the cost-payment step populated with the reduced amount.
#[test]
fn hollow_one_etb_sacrifices_when_cast_reduced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hollow = build_hollow_one(&mut scenario, P0);
    scenario.with_mana_pool(P0, colorless(hollow, 1));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .cards_discarded_this_turn_by_player
        .insert(P0, 2);

    let outcome = runner.cast(hollow).resolve();

    assert_eq!(
        outcome.zone_of(hollow),
        Zone::Graveyard,
        "cast for {{1}} (≤ {{5}}), the ETB self-sacrifice must fire → Hollow One in the graveyard"
    );
}

/// SECTION B, BOUNDARY (LE inclusive): cast Hollow One with zero reduction so the
/// full {5} is paid. Because the threshold is "or less" (inclusive), paying
/// exactly {5} still fires the sacrifice.
#[test]
fn hollow_one_etb_sacrifices_at_exactly_five() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hollow = build_hollow_one(&mut scenario, P0);
    scenario.with_mana_pool(P0, colorless(hollow, 5));

    let mut runner = scenario.build();

    let outcome = runner.cast(hollow).resolve();

    assert_eq!(
        outcome.zone_of(hollow),
        Zone::Graveyard,
        "cast for exactly {{5}}, the LE-inclusive threshold must still fire the ETB sacrifice"
    );
}

/// SECTION B, SURVIVAL: a cost-increasing static ("Spells your opponents cast
/// cost {2} more to cast.") on the opponent's permanent pushes Hollow One's paid
/// total to {7} > {5}, so the ETB condition is false and Hollow One STAYS on the
/// battlefield. Discriminates that the condition reads the ACTUAL mana spent
/// (here raised, not reduced) rather than a fixed base cost or a mis-chosen
/// quantity that would always/never fire.
#[test]
fn hollow_one_survives_when_cast_for_more_than_five() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hollow = build_hollow_one(&mut scenario, P0);
    // {7} = base {5} + the {2} opponent tax below.
    scenario.with_mana_pool(P0, colorless(hollow, 7));

    let mut runner = scenario.build();

    // CR 601.2f: opponent-controlled cost increaser. Parsed live and attached to
    // a P1 battlefield permanent so it raises P0's spells by {2}.
    let taxer = create_object(
        runner.state_mut(),
        CardId(9100),
        P1,
        "Test Cost Taxer".to_string(),
        Zone::Battlefield,
    );
    let tax_static = parse_static_line("Spells your opponents cast cost {2} more to cast.")
        .expect("opponent cost-increase static should parse");
    runner
        .state_mut()
        .objects
        .get_mut(&taxer)
        .unwrap()
        .static_definitions
        .push(tax_static);

    let outcome = runner.cast(hollow).resolve();

    assert_eq!(
        outcome.zone_of(hollow),
        Zone::Battlefield,
        "cast for {{7}} (> {{5}}), the ETB condition is false → Hollow One must survive on the \
         battlefield, proving the condition reads the actual mana spent"
    );
}
