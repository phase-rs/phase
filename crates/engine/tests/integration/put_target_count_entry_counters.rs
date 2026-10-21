//! Issue #9995: "put N target ..." keeps its announced target count, and an
//! unconditional "Each of them enters with ..." rider after a battlefield mover
//! is carried by that mover's own entry counters.
//!
//! CR 601.2c + CR 115.1a: a spell whose text says "one, two, or three target ..."
//! or "two target ..." announces that many targets as it is cast; the number of
//! targets then does not change. CR 614.12 + CR 122.6: a rider that makes the
//! moved permanents enter with counters modifies how each of them enters.
//!
//! Every card is staged from its verbatim Oracle text (checked against
//! `data/mtgjson/AtomicCards.json`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ABERRANT_RETURN: &str = "Put one, two, or three target creature cards from graveyards onto the battlefield under your control. Each of them enters with an additional -1/-1 counter on it.";

const REBUKING_CEREMONY: &str = "Put two target artifacts on top of their owners' libraries.";

/// P0's board for an Aberrant Return cast: `p0_graveyard` creature cards in
/// P0's graveyard and `p1_graveyard` creature cards in P1's graveyard (all
/// vanilla 2/2s), plus the zero-cost sorcery in P0's hand.
///
/// Returns `(runner, spell, p0_graveyard_ids, p1_graveyard_ids)`.
fn aberrant_return_board(
    p0_graveyard: &[&str],
    p1_graveyard: &[&str],
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Aberrant Return", false, ABERRANT_RETURN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let p0_ids: Vec<ObjectId> = p0_graveyard
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P0, name, 2, 2).id())
        .collect();
    let p1_ids: Vec<ObjectId> = p1_graveyard
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P1, name, 2, 2).id())
        .collect();
    (scenario.build(), spell, p0_ids, p1_ids)
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

/// C1 + C3. "One, two, or three target creature cards from graveyards" accepts
/// three announced targets, each of them (owned by either player) enters under
/// P0's control (CR 110.2a) with exactly one -1/-1 counter (CR 614.12 + CR 122.6),
/// and the fourth candidate is untouched because the spell has three slots
/// (CR 601.2c).
///
/// Reverting the count recovery leaves one target slot, so B and C stay in the
/// graveyard. Reverting the rider carrier leaves the rider an unimplemented gap,
/// so no creature gets a counter.
#[test]
fn aberrant_return_moves_up_to_three_targets_each_with_a_counter() {
    let (mut runner, spell, p0_gy, p1_gy) = aberrant_return_board(
        &["Graveyard Bear A"],
        &["Graveyard Bear B", "Graveyard Bear C", "Graveyard Bear D"],
    );
    let a = p0_gy[0];
    let (b, c, d) = (p1_gy[0], p1_gy[1], p1_gy[2]);

    let outcome = runner.cast(spell).target_objects(&[a, b, c, d]).resolve();

    // CR 601.2c: three announced targets reach the battlefield.
    outcome.assert_zone(&[a, b, c], Zone::Battlefield);
    // CR 110.2a: put onto the battlefield "under your control" - including the
    // creatures P1 owns.
    for id in [a, b, c] {
        assert_eq!(
            controller_of(&runner, id),
            P0,
            "CR 110.2a: object {} enters under P0's control",
            id.0
        );
    }
    // CR 614.12 + CR 122.6: "Each of them enters with an additional -1/-1
    // counter" - exactly one counter on every moved creature, not just the first.
    for id in [a, b, c] {
        assert_eq!(
            outcome.counters(id, CounterType::Minus1Minus1),
            1,
            "object {} must have entered with exactly one -1/-1 counter",
            id.0
        );
        let object = &outcome.state().objects[&id];
        assert_eq!(
            (object.power, object.toughness),
            (Some(1), Some(1)),
            "a 2/2 with one -1/-1 counter is a 1/1 (CR 122.1)"
        );
    }
    // The spell has at most three target slots, so the fourth declared
    // candidate stays where it was.
    outcome.assert_zone(&[d], Zone::Graveyard);
    assert_eq!(outcome.counters(d, CounterType::Minus1Minus1), 0);
}

/// C1 sibling. The minimum is one target: a single announced target is legal,
/// and the one creature moved still gets its counter. The caster announces a
/// target count of one (CR 601.2c: a variable number of targets is announced
/// before the targets).
///
/// Reach guard for the "other candidates stay put" assertion: the declared
/// creature DID move, so the undeclared one staying in the graveyard is not an
/// artifact of the spell never resolving.
#[test]
fn aberrant_return_single_target_is_legal() {
    let (mut runner, spell, p0_gy, p1_gy) =
        aberrant_return_board(&["Graveyard Bear A"], &["Graveyard Bear B"]);
    let a = p0_gy[0];
    let b = p1_gy[0];

    let outcome = runner.cast(spell).target_objects(&[a]).resolve();

    outcome.assert_zone(&[a], Zone::Battlefield);
    assert_eq!(controller_of(&runner, a), P0);
    assert_eq!(
        outcome.counters(a, CounterType::Minus1Minus1),
        1,
        "CR 614.12 + CR 122.6: the one moved creature enters with its counter"
    );
    outcome.assert_zone(&[b], Zone::Graveyard);
    assert_eq!(outcome.counters(b, CounterType::Minus1Minus1), 0);
}

/// C2. "Put two target artifacts on top of their owners' libraries" announces
/// exactly two targets (CR 601.2c). One artifact is controlled by P1 and one by
/// P0; both leave the battlefield for their owners' libraries (CR 400.3), and a
/// third artifact that was not targeted stays on the battlefield.
///
/// Reverting the count recovery leaves one target slot, so the second artifact
/// stays on the battlefield.
///
/// The position within the library is deliberately not asserted: the plural
/// "libraries" does not reach the positional-placement route (a separate
/// pre-existing defect tracked in the parser misparse backlog).
#[test]
fn rebuking_ceremony_targets_two_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rebuking Ceremony", false, REBUKING_CEREMONY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let theirs = scenario
        .add_artifact_from_oracle(P1, "Opposing Relic", "")
        .id();
    let mine = scenario
        .add_artifact_from_oracle(P0, "Friendly Relic", "")
        .id();
    let bystander = scenario
        .add_artifact_from_oracle(P1, "Bystander Relic", "")
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[theirs, mine]).resolve();

    // CR 601.2c: both announced targets are put into a library.
    outcome.assert_zone(&[theirs, mine], Zone::Library);
    // CR 400.3: each goes to its owner's library.
    assert!(
        outcome.state().players[P1.0 as usize]
            .library
            .contains(&theirs),
        "the opposing artifact goes to its owner's library"
    );
    assert!(
        outcome.state().players[P0.0 as usize]
            .library
            .contains(&mine),
        "my artifact goes to my library"
    );
    // Exactly two targets: the third artifact is untouched.
    outcome.assert_zone(&[bystander], Zone::Battlefield);
}
