//! Runtime pipeline regression — Hollow One cost reduction.
//!
//! Oracle (Scryfall oracle_id b9e4d34a-ba91-4ad6-b886-daeeda6c9498, printed
//! cost {5}, colorless Artifact Creature — Golem, 4/4):
//!   This spell costs {2} less to cast for each card you've cycled or
//!   discarded this turn.
//!   Cycling {2} ({2}, Discard this card: Draw a card.)
//!
//! The compound "cycled or discarded this turn" reduction phrase must lower
//! to `CardsDiscardedThisTurn { Controller }` (a per-controller count), NOT
//! the generic cross-player `ObjectCount{Card}` misparse that was the root of
//! the reported over-reduction bug. `record_discard` is the shared counter
//! both discards and cycling (CR 702.29a — cycling's cost is "[Cost], Discard
//! this card") feed, so the runtime reads the controller's tally from
//! `cards_discarded_this_turn_by_player`.
//!
//! DISCARD-COUNT SEEDING: qualifying discard/cycle events are seeded directly
//! into `cards_discarded_this_turn_by_player` (the exact field the runtime
//! resolver reads, and the same technique used by the Dream Salvage
//! integration test), rather than driving live cycling activations. This
//! isolates the parser + resolution fix under test from the orthogonal
//! cycling-activation plumbing (which already works and is untouched by this
//! fix).

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const HOLLOW_ONE_TEXT: &str =
    "This spell costs {2} less to cast for each card you've cycled or discarded this turn.\n\
Cycling {2} ({2}, Discard this card: Draw a card.)";

fn build_hollow_one(
    scenario: &mut GameScenario,
    controller: engine::types::player::PlayerId,
) -> ObjectId {
    scenario
        .add_creature_to_hand(controller, "Hollow One", 4, 4)
        .with_mana_cost(ManaCost::generic(5))
        .from_oracle_text(HOLLOW_ONE_TEXT)
        .id()
}

/// N colorless mana in `owner`'s pool.
fn colorless(owner: ObjectId, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, owner, false, Vec::new()))
        .collect()
}

/// DISCRIMINATING: the controller cycled one card and discarded one other
/// card this turn (2 qualifying events → {2}×2 = {4} reduction), so Hollow
/// One costs {5} − {4} = {1}. A pool of exactly {1} suffices *only if* the
/// reduction applies. If the compound phrase failed to lower (or lowered to
/// the wrong scope), the full {5} would be due and this cast would be
/// illegal.
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

/// ROOT-CAUSE REGRESSION: only an OPPONENT cycled/discarded this turn; the
/// controller had zero qualifying events. The controller-scoped count must
/// be 0 → no reduction → the full {5} is due, and a {1} pool is
/// insufficient.
///
/// This is the assertion that actually proves the original cross-player
/// `ObjectCount` bug is fixed: the buggy parse counted every card object
/// (including the opponent's), granting an illegitimate reduction. A revert
/// of the fix would let this cast through and flip the assertion.
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
