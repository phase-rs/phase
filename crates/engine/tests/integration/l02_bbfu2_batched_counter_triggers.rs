//! L02 BB-FU2 (R-6) — batched "one or more counters" CounterAdded triggers.
//!
//! Three coupled fixes, each driven through the real cast pipeline:
//!   - R1 (parser): the kind-agnostic "one or more counters" CounterAdded form is
//!     marked `batched` (CR 603.2c) unless its reproduction lowers to
//!     `Effect::Unimplemented` (Tier-3 per-kind cards, deferred to BB-FU11).
//!   - Tier-2 (magnitude arm): batched `CounterAdded` reads the counter MAGNITUDE
//!     placed by the triggering event(s), so All Will Be One's "that much" (an
//!     `EventContextAmount` read) aggregates across a multi-kind batch (CR 608.2).
//!   - R2 (runtime): Stalwart Successor's "first time counters have been put on it
//!     this turn" (CR 122.1 + CR 603.4) counts per-object put-EVENT occurrences
//!     via `object_counter_placement_count_this_turn`, so a simultaneous multi-kind
//!     first placement counts as ONE occurrence and fires exactly once.
//!
//! Oracle text is verbatim from the card database (Unexpected Fangs, All Will Be
//! One, Stalwart Successor). "Unexpected Fangs" is the multi-kind driver: it places
//! a +1/+1 AND a lifelink counter on one target in a single resolution, emitting
//! two distinct `CounterAdded` events in one trigger-collection batch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::keywords::KeywordKind;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

// Verbatim Oracle text (card database, 2026-07-12).
const STALWART_SUCCESSOR: &str = "Menace (This creature can't be blocked except by two or more creatures.)\n\
    Whenever one or more counters are put on a creature you control, if it's the first time counters \
    have been put on that creature this turn, put a +1/+1 counter on that creature.";

const ALL_WILL_BE_ONE: &str = "Whenever you put one or more counters on a permanent or player, \
    this enchantment deals that much damage to target opponent, creature an opponent controls, \
    or planeswalker an opponent controls.";

// Places a +1/+1 AND a lifelink counter on one target — two distinct-kind
// `CounterAdded` events in one batch.
const UNEXPECTED_FANGS: &str = "Put a +1/+1 counter and a lifelink counter on target creature.";
// Single kind, magnitude 2 (one `CounterAdded` event with count == 2).
const PUT_TWO_PLUS: &str = "Put two +1/+1 counters on target creature.";

fn lifelink_counter() -> CounterType {
    CounterType::Keyword(KeywordKind::Lifelink)
}

/// Sum of `DamageDealt` amounts to player `p1`, one entry per event — so
/// `[2]` proves a single firing of 2 while `[1, 1]` proves two firings.
fn damage_to_p1(out: &engine::game::scenario::CastOutcome) -> Vec<u32> {
    out.events()
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                target: TargetRef::Player(p),
                amount,
                ..
            } if *p == P1 => Some(*amount),
            _ => None,
        })
        .collect()
}

// ===========================================================================
// R1 + R2 — Stalwart Successor multi-kind first placement fires exactly once
// ===========================================================================

/// A creature receives a +1/+1 AND a lifelink counter simultaneously as the first
/// counter event of the turn (Unexpected Fangs). Stalwart fires EXACTLY once:
/// the creature ends with two +1/+1 counters (Fangs's one plus Stalwart's grant)
/// and one lifelink counter.
///
/// Discriminator (measured): reverting R2 (condition → the old
/// `counter_added_this_turn` record scan) makes the multi-kind batch push two
/// records → `count()==2` → ZERO fires → one +1/+1 counter (2 → 1, FLIP). This is
/// the occurrence-ledger discriminator.
///
/// Note: R1 (batching) is NOT the discriminator for Stalwart. Because Stalwart has
/// an intervening-if, R2's per-object occurrence ledger plus the CR 603.4
/// resolution-time recheck already fizzle any extra fire — Stalwart's own +1/+1
/// payload bumps the occurrence count to 2 before a second (non-batched) pending
/// trigger would resolve, so a bare R1 revert here still yields exactly one fire
/// (measured: stays 2). R1/batching is discriminated on a card WITHOUT an
/// intervening-if — All Will Be One — in `awbo_batched_multi_type_deals_total_single_firing`
/// ([2] batched vs [1, 1] non-batched).
#[test]
fn stalwart_multi_type_first_fires_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Stalwart Successor", 3, 2, STALWART_SUCCESSOR)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let fangs = scenario
        .add_spell_to_hand_from_oracle(P0, "Unexpected Fangs", true, UNEXPECTED_FANGS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    // Reach-guard: no prior counter placement this turn (genuinely first event).
    assert!(
        !runner
            .state()
            .object_counter_placement_count_this_turn
            .contains_key(&a),
        "reach-guard: the occurrence ledger is empty before the placement"
    );

    let out = runner.cast(fangs).target_object(a).resolve();

    // Reach-guard: the placement was multi-KIND — a lifelink counter landed
    // alongside the +1/+1, so the batch really carried two distinct CounterAdded
    // events (the old record scan really would read 2).
    assert_eq!(
        out.counters(a, lifelink_counter()),
        1,
        "reach-guard: the lifelink counter landed (multi-kind simultaneous batch)"
    );

    // Primary: Stalwart fired exactly once → 1 (Fangs +1/+1) + 1 (Stalwart) = 2.
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        2,
        "multi-kind first placement fires Stalwart exactly once (Fangs +1/+1 plus one Stalwart +1/+1)"
    );

    // Reach-guard on the occurrence ledger: the Fangs multi-kind batch deduped to
    // ONE occurrence and Stalwart's own +1/+1 added a second → exactly 2. Not
    // deduped it would be 3 (2 for Fangs's two kinds + 1 for Stalwart); not fired
    // it would be 1. So `== 2` corroborates both the dedup and the single fire.
    assert_eq!(
        runner
            .state()
            .object_counter_placement_count_this_turn
            .get(&a)
            .copied(),
        Some(2),
        "occurrence ledger: 1 (deduped Fangs batch) + 1 (Stalwart's grant)"
    );
}

// ===========================================================================
// Tier-2 (magnitude arm) — All Will Be One aggregates "that much"
// ===========================================================================

/// Batched All Will Be One on a multi-kind placement (a +1/+1 AND a lifelink
/// counter, magnitude 1 + 1) deals the TOTAL (2) to a single opponent target in
/// ONE firing. Revert the Tier-2 magnitude arm → the batch amount reads
/// `Some(0)` → AWBO deals 0 (measured FLIP).
///
/// The `[2]` (not `[1, 1]`) assertion also proves this is one batched firing, not
/// two per-event fires; and it flips to `[0]`/`[]` if the magnitude arm is reverted.
#[test]
fn awbo_batched_multi_type_deals_total_single_firing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Modeled as a permanent via `add_creature_from_oracle` (no enchantment-from-
    // oracle helper exists); "this enchantment" self-references the object
    // regardless of its card type, so the CounterAdded trigger parses identically.
    scenario
        .add_creature_from_oracle(P0, "All Will Be One", 0, 3, ALL_WILL_BE_ONE)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let fangs = scenario
        .add_spell_to_hand_from_oracle(P0, "Unexpected Fangs", true, UNEXPECTED_FANGS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner
        .cast(fangs)
        .target_object(a)
        .target_player(P1)
        .resolve();

    // Reach-guards: both kinds landed on a real permanent, so AWBO fired on a
    // genuine multi-kind counter placement (not a no-op).
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        1,
        "reach-guard: the +1/+1 counter landed"
    );
    assert_eq!(
        out.counters(a, lifelink_counter()),
        1,
        "reach-guard: the lifelink counter landed (multi-kind batch)"
    );

    assert_eq!(
        damage_to_p1(&out),
        vec![2],
        "batched AWBO deals total counters placed (1 + 1 = 2) to one opponent in a single firing"
    );
}

/// Batched AWBO single-KIND magnitude control: a single put-event placing TWO
/// +1/+1 counters (one `CounterAdded` event, count == 2) makes AWBO deal 2 — the
/// counter MAGNITUDE, not a subject headcount (which would be 1). This 1-event
/// batch flows through `count_trigger_subjects_in_batch`; the magnitude arm's
/// value matches what the non-batched reader (`extract_amount_from_event`, which
/// also returns the event `count`) would yield for the same event — the
/// defense-in-depth check that the arm did not disturb the non-batched path.
/// Revert the Tier-2 arm → 0 (FLIP).
#[test]
fn awbo_batched_single_type_reads_magnitude_not_headcount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "All Will Be One", 0, 3, ALL_WILL_BE_ONE)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let double = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Bolster", true, PUT_TWO_PLUS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner
        .cast(double)
        .target_object(a)
        .target_player(P1)
        .resolve();

    // Reach-guard: two +1/+1 counters actually landed (single kind, magnitude 2) —
    // proves the batch amount to read is 2, distinct from the 1-subject headcount.
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        2,
        "reach-guard: two +1/+1 counters placed (magnitude 2)"
    );

    assert_eq!(
        damage_to_p1(&out),
        vec![2],
        "batched AWBO reads the counter MAGNITUDE (2), not the subject headcount (1)"
    );
}
