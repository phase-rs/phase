//! Runtime pipeline regression — Faerie Slumber Party (issue #6943).
//!
//! Oracle: "Return all creatures to their owners' hands. For each opponent who
//! controlled a creature returned this way, you create two 1/1 blue Faerie
//! creature tokens with flying and "This token can block only creatures with
//! flying.""
//!
//! Two independent quantities live in the second sentence: the SET of creatures
//! returned, and the COUNT OF DISTINCT OPPONENTS who controlled at least one
//! member of it. The token count is driven by the second; the first is only the
//! membership test. The parser collapsed the clause onto the bare object-count
//! `QuantityRef::TrackedSetSize` fallback, so the card created two tokens per
//! RETURNED CREATURE (including the caster's own) instead of two per OPPONENT.
//!
//! CR 608.2c (this-way back-reference) + CR 608.2h (last known information) +
//! CR 109.4 (only objects on the battlefield have a controller).
//!
//! ## Reading the numbers — 0 is NOT a neutral result here
//!
//! Three outcomes are distinguishable and every assertion below names all of
//! them, because two DIFFERENT defects both produce a plausible-looking count:
//!
//! - **18** — the reported bug: object-count `TrackedSetSize` (9 creatures × 2).
//! - **0**  — the tracked set was never PUBLISHED. `PlayerFilter::TrackedSetPossessor`
//!   is a consumer of `tracked_object_sets`, which the producing `BounceAll`
//!   fills only when `next_sub_needs_tracked_set` reports a consumer. That
//!   predicate bottoms out in an allowlist that a new variant joins silently and
//!   wrongly; if the variant is missing from it, set selection returns `None`,
//!   every player is rejected, and the count is 0 — with nothing failing to
//!   compile. `effects::tests::repeat_for_player_count_over_tracked_set_possessors_
//!   references_tracked_set` is the dedicated guard for that seam.
//! - **6**  — correct: 3 opponents × 2.
//!
//! A test that merely asserted "not 18" could be satisfied by the 0 regression.
//! Every zero-expecting fixture below is therefore TWO-POINT: the same board is
//! re-run with one opponent creature added and must yield a NON-zero count, so 0
//! is always contrastive and can never be reached by a dead producer.

use engine::game::scenario::GameScenario;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Verbatim Oracle text. A paraphrase can take a different parser branch and go
/// green while the real card stays broken, so this is copied byte-for-byte from
/// `data/card-data.json`.
const FAERIE_SLUMBER_PARTY: &str = "Return all creatures to their owners' hands. \
For each opponent who controlled a creature returned this way, you create two 1/1 blue \
Faerie creature tokens with flying and \"This token can block only creatures with flying.\"";

/// Build a 4-player board with `creatures[i]` vanilla creatures under player i,
/// cast Faerie Slumber Party from P0, resolve it, and return
/// `(faerie_tokens_created, creatures_still_on_battlefield)`.
fn run_slumber_party(creatures: [usize; 4]) -> (usize, usize) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for (idx, count) in creatures.iter().enumerate() {
        let player = PlayerId(idx as u8);
        for _ in 0..*count {
            scenario.add_vanilla(player, 2, 2);
        }
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Faerie Slumber Party", false, FAERIE_SLUMBER_PARTY)
        .id();

    let mut runner = scenario.build();
    // Fund {4}{U}{U}.
    for _ in 0..6 {
        let unit = ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    runner.cast(spell).resolve();

    let tokens = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            runner
                .state()
                .objects
                .get(&id)
                .is_some_and(|o| o.is_token && o.name == "Faerie")
        })
        .count();
    let creatures_left = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            runner.state().objects.get(&id).is_some_and(|o| {
                !o.is_token
                    && o.card_types
                        .core_types
                        .contains(&engine::types::card_type::CoreType::Creature)
            })
        })
        .count();
    (tokens, creatures_left)
}

/// T1 — the reported scenario. P0 controls 3 creatures, P1 one, P2 two, P3
/// three (9 total). All three opponents controlled a returned creature, so the
/// count is 3 opponents × 2 tokens = 6.
///
/// REVERT DISCRIMINATOR: this is the assertion that flips. Reverting the parser
/// arm restores `repeat_for: Ref(TrackedSetSize)` and yields 18; dropping the
/// `PlayerCount` arm from `quantity_expr_references_tracked_set` yields 0.
#[test]
fn faerie_slumber_party_creates_two_tokens_per_opponent_not_per_creature() {
    let (tokens, creatures_left) = run_slumber_party([3, 1, 2, 3]);

    assert_eq!(
        tokens, 6,
        "expected 6 Faerie tokens (3 opponents × 2). \
         18 ⇒ object-count TrackedSetSize regression (9 returned creatures × 2); \
         0 ⇒ the tracked set was never published (consumer-allowlist de-registration); \
         6 ⇒ correct"
    );
    // The bounce must actually have happened, so a 0 result can be attributed to
    // the player count rather than to a producer that silently did nothing.
    assert_eq!(
        creatures_left, 0,
        "all 9 creatures must have been returned to their owners' hands — \
         the fix must not pass by breaking the bounce"
    );
}

/// H1 — TWO-POINT. The `relation: Opponent` gate: creatures the CASTER
/// controlled must not be counted.
///
/// (a) P0 controls 3 creatures and no opponent controls any → 0 tokens. Before
/// the fix this counted P0's own 3 creatures and produced 6.
/// (b) The same board plus ONE P1 creature → 2 tokens. This is the paired
/// non-zero reading that makes (a)'s 0 contrastive rather than absolute.
#[test]
fn faerie_slumber_party_ignores_creatures_the_caster_controlled() {
    let (tokens_none, _) = run_slumber_party([3, 0, 0, 0]);
    assert_eq!(
        tokens_none, 0,
        "no OPPONENT controlled a returned creature ⇒ 0 tokens. \
         6 ⇒ the caster's own 3 creatures were counted (missing Opponent gate)"
    );

    let (tokens_one, _) = run_slumber_party([3, 1, 0, 0]);
    assert_eq!(
        tokens_one, 2,
        "exactly one opponent controlled a returned creature ⇒ 2 tokens. \
         0 here would mean the tracked set is never published, which would also \
         explain the 0 above — this pairing is what tells the two apart"
    );
}

/// H2 — distinct-player semantics. ONE opponent controlling FOUR creatures is
/// still ONE opponent: 2 tokens, not 8. Guards the `.any()` over members
/// (rather than a per-member tally).
#[test]
fn faerie_slumber_party_counts_each_opponent_once_regardless_of_creature_count() {
    let (tokens, _) = run_slumber_party([0, 4, 0, 0]);
    assert_eq!(
        tokens, 2,
        "one opponent with four returned creatures is ONE player ⇒ 2 tokens. \
         8 ⇒ counted per creature instead of per distinct player"
    );
}

/// H5 — TWO-POINT empty-path guard.
///
/// (a) An empty battlefield must yield 0 tokens and must not panic (the set
/// selection returns `None`).
/// (b) The same board plus one opponent creature must yield 2.
///
/// ⚠️ Per the matrix's own rule, part (a) CARRIES NO EVIDENCE ABOUT THE COUNT on
/// its own: 0 is exactly the observable the consumer-allowlist de-registration
/// produces. It may be read only alongside part (b) and
/// `faerie_slumber_party_creates_two_tokens_per_opponent_not_per_creature` being
/// green at 6. A future reader must not treat a passing (a) as coverage of the
/// count.
#[test]
fn faerie_slumber_party_empty_battlefield_is_zero_without_panicking() {
    let (tokens_empty, _) = run_slumber_party([0, 0, 0, 0]);
    assert_eq!(
        tokens_empty, 0,
        "nothing was returned ⇒ 0 tokens, and no panic on the empty tracked set"
    );

    // NOTE the caster creature: with a board of exactly ONE opponent creature
    // and nothing else, the pre-fix object count (1 × 2) and the correct player
    // count (1 × 2) COINCIDE at 2, so such a fixture passes at base and proves
    // nothing. Adding two caster creatures separates them — base yields 6.
    let (tokens_one, _) = run_slumber_party([2, 0, 1, 0]);
    assert_eq!(
        tokens_one, 2,
        "the SAME code path with one opponent creature must produce 2 — \
         this is what distinguishes the legitimate 0 above from a dead producer. \
         6 ⇒ the pre-fix object count"
    );
}

/// Every opponent seat must be reachable and counted, so the count tracks
/// distinct opponents rather than a single hard-coded seat.
///
/// Each fixture deliberately also gives the CASTER two creatures. Without them
/// the board would hold exactly one creature, and the pre-fix object count
/// (1 × 2) would coincide with the correct player count (1 × 2) at 2 — the test
/// would pass at base and discriminate nothing. With them, base yields 6.
#[test]
fn faerie_slumber_party_counts_each_distinct_opponent_seat() {
    for (idx, seat) in [P1, P2, P3].iter().enumerate() {
        let mut creatures = [0usize; 4];
        creatures[0] = 2;
        creatures[idx + 1] = 1;
        let (tokens, _) = run_slumber_party(creatures);
        assert_eq!(
            tokens, 2,
            "opponent seat {seat:?} must be counted like any other, and the \
             caster's own two creatures must not be counted. 6 ⇒ pre-fix object count"
        );
    }
}
