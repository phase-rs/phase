//! Huskburster Swarm — a for-each cost reduction counted over a TWO-ZONE
//! population (CR 601.2f + CR 400.1).
//!
//! `{7}{B}` Creature — Elemental Insect, 6/6 (BLB):
//! "This spell costs {1} less to cast for each creature card you own in exile
//! and in your graveyard." / "Menace, deathtouch"
//!
//! The regression under guard: the zone list's second half was dropped. The
//! shared zone-suffix grammar accepted only `" or "` as a zone-list separator,
//! and additionally required every entry to carry the same possessive, so
//! "in exile and in your graveyard" parsed as the bare head "in exile" and the
//! graveyard half was silently discarded. The spell then counted only exiled
//! creature cards and cost MORE than it should, with nothing flagging it: the
//! emitted `ObjectCount` is well formed, so the card reported
//! `supported: true, gap_count: 0` and `cargo semantic-audit` found nothing.
//!
//! CR 601.2f is the authorizing rule for the reduction itself ("The total cost
//! is the mana cost or alternative cost ..., plus all additional costs and cost
//! increases, and minus all cost reductions"). CR 400.1 authorizes both halves
//! of the parse fix: "Each player has their own library, hand, and graveyard.
//! The other zones are shared by all players" — so exile can never carry a
//! possessor (hence the printed asymmetry "in exile and in YOUR graveyard"), and
//! an object occupies one zone at a time (CR 400.7: it "becomes a new object"
//! when it moves), so a zone list in a population is a union however English
//! joins it.
//!
//! DISCRIMINATING: every case drives the real cast pipeline.
//! `display_spell_cost` is the production cost-determination readout, and each
//! case then pays EXACTLY that cost through `runner.cast(..)`, so a cost that is
//! off by even one generic leaves the spell in hand. Reverting the parser fix
//! drops the graveyard half: the combined case's cost rises from {2}{B} to
//! {5}{B} and the graveyard-only case's from {4}{B} to {7}{B}, failing both the
//! readout assertion and the exact-payment cast.

use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "This spell costs {1} less to cast for each creature card you own in exile and in your graveyard.\n\
Menace, deathtouch";

/// {7}{B} — Huskburster Swarm's printed mana cost.
fn printed_cost() -> ManaCost {
    cost_with_generic(7)
}

fn cost_with_generic(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic,
    }
}

/// Exactly `generic` colorless plus the one {B} — no slack, so the cast commits
/// only when the reduction the static computed matches the asserted cost.
fn exact_payment(generic: u32) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    let mut units: Vec<ManaUnit> = (0..generic)
        .map(|_| ManaUnit::new(ManaType::Colorless, dummy, false, vec![]))
        .collect();
    units.push(ManaUnit::new(ManaType::Black, dummy, false, vec![]));
    units
}

/// Stage Huskburster Swarm in P0's hand behind `exiled` creature cards P0 owns
/// in exile and `buried` creature cards P0 owns in their graveyard.
fn stage(exiled: u32, buried: u32) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    for index in 0..exiled {
        scenario.add_creature_to_exile(P0, &format!("Exiled Beast {index}"), 2, 2);
    }
    for index in 0..buried {
        scenario.add_creature_to_graveyard(P0, &format!("Buried Beast {index}"), 2, 2);
    }
    let swarm = scenario
        .add_creature_to_hand_from_oracle(P0, "Huskburster Swarm", 6, 6, ORACLE)
        .with_mana_cost(printed_cost())
        .id();
    (scenario, swarm)
}

/// Assert the production cost readout, then prove that cost is the one actually
/// charged by casting with exactly it.
fn assert_payable_cost(exiled: u32, buried: u32, expected_generic: u32, why: &str) {
    let (mut scenario, swarm) = stage(exiled, buried);
    scenario.with_mana_pool(P0, exact_payment(expected_generic));
    let mut runner = scenario.build();

    assert_eq!(
        display_spell_cost(runner.state(), P0, swarm),
        Some(cost_with_generic(expected_generic)),
        "{why}"
    );

    let outcome = runner.cast(swarm).resolve();
    assert_eq!(
        outcome.zone_of(swarm),
        Zone::Battlefield,
        "{why}: the reduced cost must be the one the cast pipeline actually charges"
    );
}

/// CR 601.2f + CR 400.1: the load-bearing case — creature cards in BOTH exile
/// and the graveyard reduce by their COMBINED count. 2 + 3 = 5, so {7}{B}
/// becomes {2}{B}.
///
/// DISCRIMINATING: with the graveyard half of the zone list dropped the count is
/// 2, the cost is {5}{B}, and both the readout assertion and the exact-{2}{B}
/// cast fail.
#[test]
fn huskburster_reduction_counts_exile_and_graveyard_together() {
    assert_payable_cost(
        2,
        3,
        2,
        "two exiled plus three buried creature cards must reduce {7}{B} by 5",
    );
}

/// CR 400.1: each half of the union still reduces on its own — the union added
/// the graveyard rather than replacing exile with it.
///
/// DISCRIMINATING on the graveyard leg: before the fix a graveyard-only board
/// reduced nothing and the cost stayed {7}{B}.
#[test]
fn huskburster_reduction_reads_each_zone_of_the_union_alone() {
    assert_payable_cost(
        2,
        0,
        5,
        "two exiled creature cards alone must reduce {7}{B} by 2",
    );
    assert_payable_cost(
        0,
        3,
        4,
        "three buried creature cards alone must reduce {7}{B} by 3",
    );
}

/// CR 108.3 + CR 400.1: "you own" scopes BOTH halves of the union. Exile is a
/// zone shared by all players (CR 400.1), so an opponent's exiled creature card
/// sits in the very same zone as the controller's and must still not reduce the
/// cost. Nor may an opponent's graveyard or a noncreature card in either zone
/// reduce it.
///
/// What this discriminates is the zone-list ENCODING, not the presence of an
/// `Owned` property: "you own" contributes `Owned { You }` on its own (which is
/// why the zone suffix's identical property is deduped). Had the mixed list kept
/// the `TypedFilter.controller` owner-zone encoding instead, the population
/// would be filtered by a control test that `filter::typed_filter_is_owner_scoped`
/// declines to re-read as ownership for a list naming a shared zone — so the
/// count would be wrong for a reason this test's costs would expose.
///
/// Paired positive reach-guard in the SAME test: the identical board plus one
/// creature card P0 owns in each of the two zones reduces by exactly 2, so the
/// no-reduction assertion above is an ownership/type exclusion rather than a
/// reduction that never ran at all.
#[test]
fn huskburster_reduction_excludes_other_owners_and_noncreature_cards() {
    /// The excluded staging, shared by the negative case and its reach-guard.
    fn stage_excluded() -> GameScenario {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
        // Opponent-owned creature cards: one in the SHARED exile zone, one in P1's graveyard.
        scenario.add_creature_to_exile(P1, "Rival Exiled Beast", 2, 2);
        scenario.add_creature_to_graveyard(P1, "Rival Buried Beast", 2, 2);
        // Noncreature cards P0 owns in both zones.
        scenario.add_land_to_exile(P0, "Exiled Waste");
        scenario.add_land_to_graveyard(P0, "Buried Waste");
        scenario
    }

    let mut excluded = stage_excluded();
    let swarm = excluded
        .add_creature_to_hand_from_oracle(P0, "Huskburster Swarm", 6, 6, ORACLE)
        .with_mana_cost(printed_cost())
        .id();
    let runner = excluded.build();
    assert_eq!(
        display_spell_cost(runner.state(), P0, swarm),
        Some(printed_cost()),
        "neither an opponent's cards in either zone nor P0's own land cards may reduce the cost"
    );

    let mut reached = stage_excluded();
    reached.add_creature_to_exile(P0, "Own Exiled Beast", 2, 2);
    reached.add_creature_to_graveyard(P0, "Own Buried Beast", 2, 2);
    let reached_swarm = reached
        .add_creature_to_hand_from_oracle(P0, "Huskburster Swarm", 6, 6, ORACLE)
        .with_mana_cost(printed_cost())
        .id();
    let reached_runner = reached.build();
    assert_eq!(
        display_spell_cost(reached_runner.state(), P0, reached_swarm),
        Some(cost_with_generic(5)),
        "reach-guard: one owned creature card in each zone must reduce {{7}}{{B}} by exactly 2"
    );
}
