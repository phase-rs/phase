//! Windfall's cross-player MAX aggregate — "the greatest number of cards a
//! player discarded this way".
//!
//! Oracle (Scryfall, verified verbatim 2026-08-15):
//!   "Each player discards their hand, then draws cards equal to the greatest
//!    number of cards a player discarded this way."
//!
//! Class: Windfall, Jace's Archivist, Whispering Madness — identical text.
//!
//! CR 608.2e: the discard action is processed simultaneously for every player,
//!   then the draw action reads that completed action's result.
//! CR 608.2h: the draw count is determined ONCE, when the draw action is
//!   applied — not re-derived per player as the fan-out proceeds.
//! CR 608.2i: that determination is a look-back at the already-completed
//!   discard action, the exception to CR 608.2h this clause relies on.
//! CR 701.9a: to discard a card is to move it from hand to graveyard.
//! CR 121.2: drawing N cards is N individual card draws.
//!
//! The regression this pins: the engine reduces the per-player discard counts to
//! ONE untyped scalar whose aggregate lived on the PRODUCER. With the producer
//! set to a cross-player SUM, Windfall drew 8+7+3+3 = 21 for every player
//! instead of the greatest single player's 8.

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const WINDFALL: &str = "Each player discards their hand, then draws cards equal to the greatest number of cards a player discarded this way.";

/// Syphon Mind's shape — the NON-superlative "discarded this way" neighbour.
///
/// This does NOT guard the aggregate axis, and an earlier revision of this file
/// claimed that it did. Syphon Mind parses to `FilteredTrackedSetSize` and
/// carries no `PreviousEffectAmount` node at all, so it is structurally
/// incapable of detecting a change to `QuantityRef::PreviousEffectAmount`'s
/// aggregate — measured: it stays green under BOTH the aggregate revert and the
/// clause-freeze revert. What it does guard is real and worth keeping: that the
/// superlative combinator did not STEAL the non-superlative phrasing, i.e. this
/// card still reaches `FilteredTrackedSetSize` and still sums.
///
/// The aggregate axis is guarded at unit level instead — see
/// `game/quantity.rs`'s `previous_effect_amount_live_when_no_snapshot` and
/// `previous_effect_amount_aggregates_are_mutually_distinct`. Measured: no card
/// in the **Sum class** yields a clean integration-level Sum-vs-Max
/// discriminator — the Max class does, and it is the first test in this file.
const SYPHON_MIND: &str =
    "Each other player discards a card. You draw a card for each card discarded this way.";

/// Blood Tithe — the drain shape, and the class the corpus actually populates.
/// Measured: 44 cards carry both a `player_scope` and a `PreviousEffectAmount`
/// somewhere; 3 hold it only outside the scoped subtree, in a condition. Of the
/// 41 that hold it inside, in a quantity position, 38 carry it on a `GainLife` —
/// this `LoseLife` → `GainLife { PreviousEffectAmount }` form.
///
/// Unlike Syphon Mind this DOES build `PreviousEffectAmount`, with `aggregate`
/// absent and therefore `Sum`. CR 119.3: an effect causing a player to gain or
/// lose life adjusts that life total accordingly — one rule covers both
/// directions here. "The life lost this way" is the cross-player TOTAL, 9.
///
/// It is a REACH guard, not an aggregate discriminator: `Effect::LoseLife`
/// publishes no per-player table, so `Max`/`Min` fall back to the total and all
/// three reductions coincide at 9. Measured, not reasoned — see the degeneracy
/// note on the test itself.
const BLOOD_TITHE: &str =
    "Each opponent loses 3 life. You gain life equal to the life lost this way.";

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const SEATS: [PlayerId; 4] = [P0, P1, P2, P3];

/// Deep enough that no draw in these tests is library-limited.
const LIBRARY_DEPTH: usize = 60;

fn seed_library(scenario: &mut GameScenario, player: PlayerId, n: usize) {
    for i in 0..n {
        scenario.add_card_to_library_top(player, &format!("Filler {i}"));
    }
}

fn seed_hand(scenario: &mut GameScenario, player: PlayerId, n: usize) {
    for i in 0..n {
        scenario.add_card_to_hand(player, &format!("Hand Filler {i}"));
    }
}

fn zone_len(outcome: &Outcome, player: PlayerId, zone: Zone) -> usize {
    let p = outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");
    match zone {
        Zone::Hand => p.hand.len(),
        Zone::Library => p.library.len(),
        Zone::Graveyard => p.graveyard.len(),
        other => panic!("zone_len does not cover {other:?}"),
    }
}

/// CR 608.2e + CR 121.2: four seats, hands 8/7/3/3 (the USER-reported board).
/// CR 608.2h: the greatest number of cards any one player discarded is 8 and is
/// determined once when the draw action is applied, so EVERY player draws
/// exactly 8.
///
/// P0's eight are the cards held BESIDE Windfall: CR 601.2a removes the spell
/// from hand when the cast commits to the stack, so it is not itself discarded.
///
/// Non-vacuous and discriminating: the four hand sizes make MAX (8), SUM (21),
/// MIN (3), and per-player (8/7/3/3) four mutually distinguishable outcomes, so
/// the assertion fails under every wrong aggregate, not merely the one that
/// shipped. The graveyard assertion is the reach guard — it proves the discard
/// step actually ran, so a spell that failed to parse or resolve cannot pass a
/// bare hand-size check for the wrong reason.
#[test]
fn windfall_draws_the_greatest_single_players_discard_not_the_cross_player_sum() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, hand) in SEATS.iter().zip([8usize, 7, 3, 3]) {
        seed_hand(&mut scenario, *seat, hand);
        seed_library(&mut scenario, *seat, LIBRARY_DEPTH);
    }
    let windfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Windfall", false, WINDFALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(windfall).resolve();

    let drawn: Vec<usize> = SEATS
        .iter()
        .map(|p| LIBRARY_DEPTH - zone_len(&outcome, *p, Zone::Library))
        .collect();
    let hands: Vec<usize> = SEATS
        .iter()
        .map(|p| zone_len(&outcome, *p, Zone::Hand))
        .collect();
    let graveyards: Vec<usize> = SEATS
        .iter()
        .map(|p| zone_len(&outcome, *p, Zone::Graveyard))
        .collect();

    // CR 701.9a reach guard: every player really did discard their whole hand.
    assert!(
        graveyards[0] >= 8 && graveyards[1] >= 7 && graveyards[2] >= 3 && graveyards[3] >= 3,
        "reach guard: each player's hand must have reached the graveyard, got {graveyards:?}"
    );
    assert_eq!(
        drawn,
        vec![8, 8, 8, 8],
        "each player draws the GREATEST single-player discard (8), not the cross-player sum (21)"
    );
    assert_eq!(
        hands,
        vec![8, 8, 8, 8],
        "each hand holds exactly the freshly drawn cards"
    );
}

/// NON-INTERFERENCE, not an aggregate guard. Syphon Mind in a four-player game:
/// the three other players each discard one card and the controller draws 3.
///
/// What this discriminates: that the superlative combinator did not swallow the
/// non-superlative "discarded this way" phrasing — this card must still reach
/// `FilteredTrackedSetSize` and still sum. What it does NOT discriminate: the
/// aggregate axis. Syphon Mind builds no `PreviousEffectAmount` node, so it
/// cannot see a change to that ref's `aggregate` and stays green under both
/// revert arms. The cross-aggregate guard lives at unit level, in
/// `game/quantity.rs`'s `previous_effect_amount_aggregates_are_mutually_distinct`
/// and `previous_effect_amount_live_when_no_snapshot` — no card in the **Sum
/// class** gives a clean integration-level Sum-vs-Max discriminator. (The Max
/// class does: `windfall_draws_the_greatest_single_players_discard_not_the_cross_player_sum`
/// above separates MAX 8 / SUM 21 / MIN 3. The gap is specific to the Sum class,
/// whose producers publish no per-player table for an aggregate to reduce over.)
#[test]
fn syphon_mind_shape_still_draws_the_cross_player_total() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in SEATS {
        seed_hand(&mut scenario, seat, 1);
        seed_library(&mut scenario, seat, LIBRARY_DEPTH);
    }
    let syphon = scenario
        .add_spell_to_hand_from_oracle(P0, "Syphon Mind", false, SYPHON_MIND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(syphon).resolve();

    let drawn = LIBRARY_DEPTH - zone_len(&outcome, P0, Zone::Library);
    let opponents_discarded: usize = [P1, P2, P3]
        .iter()
        .map(|p| zone_len(&outcome, *p, Zone::Graveyard))
        .sum();

    // Reach guard: the discard step ran for all three opponents (CR 701.9a).
    assert_eq!(
        opponents_discarded, 3,
        "reach guard: each of the three other players must discard one card"
    );
    assert_eq!(
        drawn, 3,
        "controller draws one per card discarded across all opponents (sum), not the max (1)"
    );
}

/// CR 608.2c: a zero-contributor board must not disturb the Max class.
///
/// Board 8/7/3/**0** — P3 has an empty hand, so "each player discards their
/// hand" emits no discard event for them and the event-built table arrives as
/// `{8,7,3}` with P3 absent. The producer fills that gap with a 0 so an
/// aggregate reduces over every subject.
///
/// SCOPE — this asserts the NON-REGRESSION half only: the greatest discard is
/// still 8, so every player including the empty-handed one still draws 8. It
/// does NOT assert the table's contents, and deliberately so: by the time
/// `outcome.state()` is readable the table is already `[]` regardless of the
/// fix. An earlier revision asserted on it and failed with `left: []` — an
/// INSTRUMENT failure, not a fix failure.
///
/// The clearer is this card's OWN draw tail, not the player-action boundary:
/// `Effect::Draw` is not a count producer, so its postlude calls
/// `install_previous_effect_counts_by_player(.., None, ..)` and takes the arm
/// that clears — inside the same resolution, long before `apply()`'s
/// start-of-action clear could matter. Two tests bracket this: a bare `Discard`
/// fan-out with no tail leaves the table populated
/// (`game/effects/mod.rs`'s `player_scope_fan_out_publishes_a_zero_for_the_empty_handed_seat`),
/// and adding the draw tail — this test — empties it.
///
/// The table's contents are therefore asserted where they survive, at unit
/// level: that same production-wire test pins the zero entry, and
/// `game/quantity.rs`'s `previous_effect_amount_min_counts_the_zero_contributor`
/// pins `Min` at 0 filled versus 3 unfilled.
#[test]
fn windfall_zero_contributor_board_still_draws_the_greatest() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    seed_hand(&mut scenario, P0, 8);
    seed_hand(&mut scenario, P1, 7);
    seed_hand(&mut scenario, P2, 3);
    // P3: no hand at all — the zero contributor.
    for seat in SEATS {
        seed_library(&mut scenario, seat, LIBRARY_DEPTH);
    }
    let windfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Windfall", false, WINDFALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(windfall).resolve();

    let drawn: Vec<usize> = SEATS
        .iter()
        .map(|p| LIBRARY_DEPTH - zone_len(&outcome, *p, Zone::Library))
        .collect();
    let graveyards: Vec<usize> = SEATS
        .iter()
        .map(|p| zone_len(&outcome, *p, Zone::Graveyard))
        .collect();

    // CR 701.9a reach guard: the discard step ran, and P3 really contributed
    // nothing — without this, an all-8 draw could pass on a board that never
    // had a zero contributor at all.
    assert_eq!(
        graveyards[3], 0,
        "reach guard: P3 must be the zero contributor, got {graveyards:?}"
    );
    assert!(
        graveyards[0] >= 8,
        "reach guard: the discard step must have run, got {graveyards:?}"
    );
    assert_eq!(
        drawn,
        vec![8, 8, 8, 8],
        "non-regression: the greatest discard is still 8, so every player draws 8"
    );
}

/// REACH + non-regression guard for the drain class — NOT an aggregate
/// discriminator. Read the measured degeneracy below before trusting it as one.
///
/// Blood Tithe in a four-player game: each of the three opponents loses 3 life,
/// so "the life lost this way" is 3 + 3 + 3 = 9 (CR 119.3) and the controller
/// gains 9. This is the shape 38 of the 41 quantity-position corpus carriers
/// take (see `BLOOD_TITHE`'s note for the full split), so it is the widest
/// non-regression this file has.
///
/// WHAT IT DISCRIMINATES, measured by sentinel probe: the ref is genuinely
/// reached — forcing an early `return 999` at the top of the
/// `QuantityRef::PreviousEffectAmount` arm moves this card to 1019 life. So a
/// change that stopped routing the drain class through that arm fails here.
///
/// WHAT IT DOES **NOT** DISCRIMINATE: the aggregate axis. `Effect::LoseLife`
/// publishes no per-player breakdown — only `Discard` / `DiscardCard` /
/// `ChangeZoneAll` populate `last_effect_counts_by_player` — so the table is
/// EMPTY here and `Max`/`Min` both fall back to `unwrap_or(total)`. All three
/// reductions coincide:
///
///   Sum -> 9      Max -> 9      Min -> 9      (degenerate)
///
/// Measured, not reasoned: forcing `AggregateFunction::Sum => per_player.max()
/// .unwrap_or(total)` leaves this test green at 29. An earlier revision of this
/// comment claimed `Max -> 3` and that a global flip would fail here. That was
/// wrong, and it is the same error as the Syphon Mind control above — a
/// discriminating claim derived from the parse tree and never revert-probed.
///
/// The aggregate axis IS discriminated, at unit level where a populated table
/// can be constructed directly: `game/quantity.rs`'s
/// `previous_effect_amount_live_when_no_snapshot` asserts `Max` = 8 over
/// `{P0:8, P1:3}` with `last_effect_amount` = 11, so `Sum` fails it.
#[test]
fn blood_tithe_drain_still_gains_the_cross_player_total() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in SEATS {
        seed_library(&mut scenario, seat, LIBRARY_DEPTH);
    }
    let tithe = scenario
        .add_spell_to_hand_from_oracle(P0, "Blood Tithe", false, BLOOD_TITHE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(tithe).resolve();

    let life: Vec<i32> = SEATS
        .iter()
        .map(|p| {
            outcome
                .state()
                .players
                .iter()
                .find(|pl| pl.id == *p)
                .expect("player exists")
                .life
        })
        .collect();

    // Reach guard: the loss step actually ran for all three opponents, so the 9
    // really is a three-way total and not a single 3 read off a fan-out that
    // never happened. (It is NOT evidence about the per-player table, which is
    // empty here — see the degeneracy note above.)
    assert_eq!(
        &life[1..],
        &[17, 17, 17],
        "reach guard: each of the three opponents loses exactly 3 (CR 119.3)"
    );
    assert_eq!(
        life[0], 29,
        "controller gains the cross-player TOTAL life lost (9) via \
         PreviousEffectAmount — a reach guard for the 38-card drain class, not an \
         aggregate discriminator (see the degeneracy note above)"
    );
}

/// PROBE for the second, independent defect the code map surfaced: the draw
/// tail keeps `player_scope: All` and re-fans-out, and each player's completed
/// draw re-stamps the shared scalar with that player's DELIVERED count. So a
/// player whose library ran short does not just draw fewer cards — they
/// redefine how many every LATER player draws.
///
/// CR 608.2h: the draw action's count is determined only once, when the
/// effect is applied — one player's short library cannot change another
/// player's count. CR 608.2e: the whole fan-out is one action processed
/// simultaneously. CR 121.2c: the SERIALIZATION (the active player performs
/// all of their draws first, then each other player in turn order) is itself
/// rules-correct — only the leaked count is not.
///
/// Discriminating: P0's library holds 5, everyone else 60. Correct = [5,8,8,8].
/// Leaked-delivered-count = [5,5,5,5]. The two differ on three seats.
#[test]
fn windfall_short_library_does_not_shrink_later_players_draws() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, hand) in SEATS.iter().zip([8usize, 7, 3, 3]) {
        seed_hand(&mut scenario, *seat, hand);
        seed_library(
            &mut scenario,
            *seat,
            if *seat == P0 { 5 } else { LIBRARY_DEPTH },
        );
    }
    let windfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Windfall", false, WINDFALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(windfall).resolve();

    let drawn: Vec<usize> = SEATS
        .iter()
        .map(|p| {
            let depth = if *p == P0 { 5 } else { LIBRARY_DEPTH };
            depth - zone_len(&outcome, *p, Zone::Library)
        })
        .collect();
    assert_eq!(
        drawn,
        vec![5, 8, 8, 8],
        "P0's short library caps only P0; every later player still draws the greatest discard (8)"
    );
}

// ---------------------------------------------------------------------------
// The replacement-pause arms (CR 608.2c: replacement effects may modify an
// instruction's actions). A replacement choice interrupts the discard fan-out
// mid-batch; the clause must still publish ONE complete per-player table.
// ---------------------------------------------------------------------------

/// Library of Leng, verbatim Scryfall (re-fetched 2026-08-16 via
/// `curl -s 'https://api.scryfall.com/cards/named?exact=Library%20of%20Leng' | jq -r .oracle_text`).
///
/// Line 2 parses to a `ReplacementEvent::Discard` definition in
/// `ReplacementMode::Optional` with `valid_card: Typed { controller: You }`
/// (`parser/oracle_replacement.rs`'s `parse_discard_to_library_top_replacement`),
/// so the engine raises an Accept/Decline prompt for its controller's discards
/// and for nobody else's. That is what makes this fixture's pause count
/// predictable: exactly one prompt per card P0 discards.
const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect causes you to discard a card, discard it, but you may put it on top of your library instead of into your graveyard.";

/// Hands beside Windfall. The MAXIMUM sits on P0 — the seat whose batch pauses —
/// so the aggregate is only correct if that seat is present AND complete in the
/// published table. Reference values over `{P0:7, P1:3, P2:5, P3:2}`:
/// MAX 7, MAX-without-P0 5, MAX-with-P0's-paused-card-uncounted 6, last-seat 2.
/// Four mutually distinct numbers, one per failure mode.
const PAUSED_HANDS: [usize; 4] = [7, 3, 5, 2];

fn seed_hand_ids(scenario: &mut GameScenario, player: PlayerId, n: usize) -> Vec<ObjectId> {
    (0..n)
        .map(|i| scenario.add_card_to_hand(player, &format!("Hand Filler {player:?} {i}")))
        .collect()
}

fn state_zone_len(runner: &GameRunner, player: PlayerId, zone: Zone) -> usize {
    let p = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");
    match zone {
        Zone::Hand => p.hand.len(),
        Zone::Library => p.library.len(),
        Zone::Graveyard => p.graveyard.len(),
        other => panic!("state_zone_len does not cover {other:?}"),
    }
}

/// Answer every `ReplacementChoice` the board raises with the named option,
/// returning how many were answered. The count is the reach guard for every
/// assertion below: a run that raised no prompt never exercised the pause path
/// at all, and would pass a bare zone-count check for the wrong reason.
fn answer_every_replacement_choice(runner: &mut GameRunner, description: &str) -> usize {
    for (prompts, _) in (0..64).enumerate() {
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            return prompts;
        };
        let index = candidates
            .iter()
            .position(|c| c.description == description)
            .unwrap_or_else(|| {
                panic!(
                    "no {description:?} option among {:?}",
                    candidates
                        .iter()
                        .map(|c| c.description.clone())
                        .collect::<Vec<_>>()
                )
            });
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("ChooseReplacement must be accepted");
    }
    panic!("the replacement-choice loop never terminated");
}

/// Every observable of the paused clause, asserted as ONE value so a failure
/// prints the whole signature rather than the first divergent field.
#[derive(Debug, PartialEq, Eq)]
struct PausedFanOutSignature {
    prompts: usize,
    graveyards: Vec<usize>,
    drawn: Vec<usize>,
    hands: Vec<usize>,
}

/// Rest in Peace class, made OPTIONAL so it surfaces an Accept/Decline choice.
/// Copied in shape from `random_discard_cost_replacement_resume.rs`'s
/// `optional_graveyard_exile_replacement`; narrowed per call site with
/// `valid_card`.
fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

/// ARM A — gate 1 (`ReplacementEvent::Discard`, Library of Leng), the fan-out
/// discriminator.
///
/// P0 controls an OPTIONAL discard replacement, so every one of P0's seven
/// discards raises an apply-or-decline choice. Deliberately NOT cited to
/// CR 616.1: that rule governs choosing among two or more competing
/// replacements, and this arm has exactly one. CR 614.6 is what makes the
/// applied-or-declined event resolve as it does. CR 608.2f: the discard action is taken on
/// four players and cannot be processed simultaneously once it pauses, so it is
/// processed per player — but it is still ONE action, and the look-back
/// (CR 608.2i) that feeds the draw clause must see every seat's contribution.
///
/// Discriminating: with `{P0:7, P1:3, P2:5, P3:2}` the correct MAX is 7, and 7
/// is unreachable under every partial-table failure mode — a table missing P0
/// yields 5, a table holding only the last resumed leg yields 2, and a table
/// where P0's paused card went uncounted yields 6.
///
/// Reach guards, both inside the asserted signature: `prompts == 7` proves the
/// pause path really ran seven times (a zero-prompt run would trivially satisfy
/// a graveyard check), and `graveyards == [8, 3, 5, 2]` proves all four seats
/// discarded their whole hands. P0's 8 is seven discards PLUS Windfall itself:
/// CR 608.2n — "As the final part of an instant or sorcery spell's resolution,
/// the spell is put into its owner's graveyard." That is the same reason the
/// first test in this file asserts `>= 8` rather than `== 8`.
#[test]
fn windfall_paused_mid_fan_out_still_draws_the_greatest() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, hand) in SEATS.iter().zip(PAUSED_HANDS) {
        seed_hand_ids(&mut scenario, *seat, hand);
        seed_library(&mut scenario, *seat, LIBRARY_DEPTH);
    }
    let leng = scenario
        .add_creature_from_oracle(P0, "Library of Leng", 1, 1, LIBRARY_OF_LENG)
        .as_artifact()
        .id();
    let windfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Windfall", false, WINDFALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // Fixture self-checks — the prop must be what the derivation assumes.
    assert_eq!(
        runner.state().objects[&leng].card_types.core_types,
        vec![CoreType::Artifact],
        "the Leng prop must be an artifact, not a creature"
    );
    assert_eq!(
        runner.state().objects[&leng].replacement_definitions.len(),
        1,
        "Library of Leng's second line must parse to exactly one replacement"
    );

    // The cast driver stops at the first ReplacementChoice it is not told how
    // to answer; from there this test drives the prompts itself so it can count
    // them.
    runner.cast(windfall).resolve();
    assert!(
        runner.state().pending_discard_batch.is_some(),
        "reach guard: the first Library of Leng prompt must park Windfall's live discard batch"
    );
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("parked Windfall state serializes through the authoritative persistence envelope");
    let restored: PersistedGameState = serde_json::from_str(&saved)
        .expect("parked Windfall state restores through the authoritative persistence envelope");
    let restored = restored
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert!(
        restored.pending_discard_batch.is_some(),
        "the live discard cursor must survive save and restore before its replacement choice"
    );
    let mut runner = GameRunner::from_state(restored);
    let prompts = answer_every_replacement_choice(&mut runner, "Decline");
    runner.advance_until_stack_empty();

    let observed = PausedFanOutSignature {
        prompts,
        graveyards: SEATS
            .iter()
            .map(|p| state_zone_len(&runner, *p, Zone::Graveyard))
            .collect(),
        drawn: SEATS
            .iter()
            .map(|p| LIBRARY_DEPTH - state_zone_len(&runner, *p, Zone::Library))
            .collect(),
        hands: SEATS
            .iter()
            .map(|p| state_zone_len(&runner, *p, Zone::Hand))
            .collect(),
    };

    assert_eq!(
        observed,
        PausedFanOutSignature {
            prompts: 7,
            graveyards: vec![8, 3, 5, 2],
            drawn: vec![7, 7, 7, 7],
            hands: vec![7, 7, 7, 7],
        },
        "a replacement pause must not truncate the batch (prompts/graveyards) nor split \
         the clause's per-player table (drawn/hands)"
    );
}

#[test]
fn replacement_resumed_targeted_discard_preserves_the_announced_multi_owner_tail() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_card_to_hand(P1, "First Target");
    let second = scenario.add_card_to_hand(P2, "Second Target");
    let source = scenario
        .add_spell_to_hand(P0, "Targeted Discard", false)
        .id();
    scenario
        .add_creature(P0, "Graveyard Warden", 1, 1)
        .with_replacement_definition(
            optional_graveyard_exile_replacement()
                .valid_card(TargetFilter::SpecificObject { id: first }),
        );
    let mut runner = scenario.build();
    let ability = ResolvedAbility::new(
        Effect::DiscardCard {
            count: 2,
            target: TargetFilter::SpecificObject { id: first },
        },
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    engine::game::effects::resolve_ability_chain(
        runner.state_mut(),
        &ability,
        &mut initial_events,
        0,
    )
    .expect("the announced targeted discard resolves to its first replacement choice");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_discard_batch.as_deref().map(|batch| &batch.cursor),
        Some(engine::types::game_state::DiscardBatchCursor::Ordered { remaining })
            if remaining.len() == 1 && remaining[0].object_id == second
    ));

    let result = apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseReplacement { index: 0 },
    )
    .expect("accept the first target's graveyard redirect");

    assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert!(runner.state().pending_discard_batch.is_none());
    assert_eq!(
        result
            .events
            .iter()
            .filter(
                |event| matches!(event, engine::types::events::GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::DiscardCard,
                source_id: event_source,
                ..
            } if *event_source == source)
            )
            .count(),
        1,
        "the resumed ordered target list emits its terminal marker exactly once"
    );
}

/// Every observable of the gate-2 arm, asserted as ONE value.
#[derive(Debug, PartialEq, Eq)]
struct GateTwoSignature {
    prompts: usize,
    p0_graveyard: usize,
    redirected_card_zone: Zone,
    exiled_total: usize,
    drawn: Vec<usize>,
}

/// ARM B — gate 2 (`ReplacementEvent::Moved` on the inner hand → graveyard
/// move), the discriminator for the paused card's OWN count.
///
/// CR 614.6: a replaced event never happens; the modified event happens
/// instead — the card is still discarded (CR 701.9a) and must still be counted.
/// The gate-2 resume returns through terminal zone delivery, which emits no
/// `GameEvent::Discarded` for an unframed discard, so the paused card is the one
/// card that can silently vanish from the table even after the batch resumes.
///
/// Discriminating: exactly one card in the game can prompt (`valid_card` is a
/// `SpecificObject`), the redirect is ACCEPTED, and P0's counted discards are 7
/// while P0's graveyard tops out at 7 (six discards + Windfall) because the
/// seventh went to exile. If the paused card is uncounted the aggregate is 6,
/// not 7 — the only arm in this file that separates that facet from the batch
/// truncation arm above.
///
/// Reach guards, inside the asserted signature: `prompts == 1` proves the pause
/// happened, and `redirected_card_zone == Exile` / `exiled_total == 1` prove the
/// redirect was actually applied (on a board where it silently did not apply,
/// the card would be in the graveyard and the count would be right for the
/// wrong reason).
#[test]
fn windfall_counts_a_card_redirected_out_of_the_graveyard_mid_batch() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut p0_hand = Vec::new();
    for (seat, hand) in SEATS.iter().zip(PAUSED_HANDS) {
        let ids = seed_hand_ids(&mut scenario, *seat, hand);
        if *seat == P0 {
            p0_hand = ids;
        }
        seed_library(&mut scenario, *seat, LIBRARY_DEPTH);
    }
    let redirected = p0_hand[3];
    // Hosted on P1 so it cannot be confused with the discarding seat's own
    // permanents; narrowed to a single card so the prompt count is exactly 1.
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(
            optional_graveyard_exile_replacement()
                .valid_card(TargetFilter::SpecificObject { id: redirected }),
        );
    let windfall = scenario
        .add_spell_to_hand_from_oracle(P0, "Windfall", false, WINDFALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(windfall).resolve();
    let prompts = answer_every_replacement_choice(&mut runner, "Accept");
    runner.advance_until_stack_empty();

    let observed = GateTwoSignature {
        prompts,
        p0_graveyard: state_zone_len(&runner, P0, Zone::Graveyard),
        redirected_card_zone: runner.state().objects[&redirected].zone,
        exiled_total: runner
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Exile)
            .count(),
        drawn: SEATS
            .iter()
            .map(|p| LIBRARY_DEPTH - state_zone_len(&runner, *p, Zone::Library))
            .collect(),
    };

    assert_eq!(
        observed,
        GateTwoSignature {
            prompts: 1,
            p0_graveyard: 7,
            redirected_card_zone: Zone::Exile,
            exiled_total: 1,
            drawn: vec![7, 7, 7, 7],
        },
        "a card redirected out of the graveyard mid-batch was still discarded \
         (CR 614.6 + CR 701.9a) and must still be counted"
    );
}
