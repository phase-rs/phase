//! Measurement suite for the SINGLE cost-paid provenance authority,
//! `ResolvedAbility::cost_paid_objects: Vec<CostPaidObjectRecord>`.
//!
//! This replaced a raw `Vec<ObjectId>`. An `ObjectId` is reusable storage
//! identity, so an id alone cannot distinguish "the object this ability's cost
//! moved" from "a different object that later took the same id" (CR 400.7: an
//! object that changes zones becomes a NEW object). The snapshot authority
//! carries the referent's post-cost incarnation so a later consumer can ask
//! that question; these tests measure that the authority is actually produced,
//! and produced LIVE, at every payment seam.
//!
//! Why these tests read internal provenance instead of a board outcome: this
//! change is a deliberate behavioral no-op for today's consumers — the one
//! consumer of the plural authority,
//! `exclude_cost_paid_object_that_left_battlefield`, is membership-only and
//! reads each record's `object_id` exactly as it read the raw ids. The
//! observable seam being installed here is the incarnation pin. Every
//! measurement below still drives the REAL pipeline — `GameScenario` +
//! `GameRunner`, real `GameAction` cost payment through the engine's own cost
//! windows — and none hand-constructs a `ResolvedAbility`.
//!
//! Rules riding on these assertions:
//!   * CR 400.7 — a zone change makes a new object; a pin captured BEFORE the
//!     cost's own move names the pre-move object and is stale immediately.
//!   * CR 400.7j — "If the cost of a spell or ability causes an object to move
//!     to a public zone, that spell or ability's effects can find that object."
//!     So the cost's OWN move must not invalidate the reference: each payment
//!     seam re-pins once its moves complete.
//!   * CR 608.2h — the snapshot's `lki` must still record PRE-move
//!     characteristics, which is why capture happens before the move and the
//!     pin is refreshed afterwards rather than the whole snapshot being retaken.
//!   * CR 601.2h / CR 602.2b — the payment is the authority; provenance is
//!     recorded during payment, never reconstructed at resolution.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CostPaidObjectRecord, CostPaidObjectSnapshot, ResolvedAbility, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PayCostKind, WaitingFor};
use engine::types::identifiers::{ObjectId, LEGACY_INCARNATION};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Verbatim Oracle text. Every string below is copied from this repository's own
// existing integration coverage for the same printed card — never paraphrased,
// because a paraphrase can take a different parser branch and go green while
// the real card stays broken.
// ---------------------------------------------------------------------------

/// Harnfel, Horn of Bounty (the Birgi, God of Storytelling back face).
/// Source: `crates/engine/tests/integration/birgi.rs`.
const HARNFEL: &str =
    "Discard a card: Exile the top two cards of your library. You may play those cards this turn.";

/// Pyromancy. Source: `crates/engine/tests/integration/pyromancy_random_discard_cost.rs`.
const PYROMANCY: &str = "{3}, Discard a card at random: Pyromancy deals damage to any target equal to the discarded card's mana value.";

/// Greater Good. Source: `crates/engine/tests/integration/greater_good_activation.rs`.
const GREATER_GOOD: &str = "Sacrifice a creature: Draw cards equal to the sacrificed \
     creature's power, then discard three cards.";

/// Coin of Fate. Source: `crates/engine/tests/integration/coin_of_fate.rs`.
const COIN_OF_FATE: &str = "When this artifact enters, surveil 1.\n{3}{W}, {T}, Exile two creature cards from your graveyard, Sacrifice this artifact: An opponent chooses one of the exiled cards. You put that card on the bottom of your library and return the other to the battlefield tapped. You become the monarch.";

// ---------------------------------------------------------------------------
// Shared drivers
// ---------------------------------------------------------------------------

fn white_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
        .collect()
}

fn colorless_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// The index of the single costed activated ability on `source`.
fn costed_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("the fixture card must carry an activated ability with a cost")
}

/// `object`'s live incarnation epoch right now. Recorded BEFORE payment so the
/// tests can prove the cost's own move actually advanced it — without that, a
/// snapshot that was never re-pinned would still compare equal to the live
/// object and the measurement would be vacuous.
fn incarnation(runner: &GameRunner, object: ObjectId) -> u64 {
    runner.state().objects[&object].incarnation
}

/// Answer the engine's own cost windows until the activation reaches the stack.
///
/// `cost_cards` are the objects the caller intends to pay the non-mana cost
/// with; a `PayCost` window whose eligible set does not contain them (a
/// self-sacrifice leg, for instance) is answered from its own `choices`, so
/// this driver never guesses which cost leg it is looking at.
fn pay_until_on_stack(runner: &mut GameRunner, cost_cards: &[ObjectId]) {
    for _ in 0..24 {
        if !runner.state().stack.is_empty() {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, count, .. } => {
                let mut selection: Vec<ObjectId> = cost_cards
                    .iter()
                    .copied()
                    .filter(|id| choices.contains(id))
                    .collect();
                if selection.len() != count {
                    selection = choices.iter().copied().take(count).collect();
                }
                runner
                    .act(GameAction::SelectCards { cards: selection })
                    .expect("the engine's own cost window must accept its own eligible set");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("the mana cost must finalize from the floating pool");
            }
            // CR 616.1: a cost's own move can surface a replacement prompt
            // before the payment completes — the hidden-zone redirect fixture
            // below installs a graveyard redirect that watches the paid card's
            // move. A single mandatory candidate applies without asking, so this
            // arm fires only when the engine genuinely prompts, and index 0 is
            // then the first offered candidate. It is answered here rather than
            // skipped so the payment continues down the engine's OWN resumed
            // cost path (`resume_random_discard_cost_payment`).
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the engine's own replacement prompt must accept its first candidate");
            }
            other => panic!("the activation stalled before reaching the stack at {other:?}"),
        }
    }
    panic!("the activation never reached the stack within the bounded driver");
}

/// The resolving ability sitting on the stack, i.e. the carrier that owns the
/// cost-paid provenance the payment just published.
fn ability_on_stack(runner: &GameRunner) -> &ResolvedAbility {
    let state = runner.state();
    let entry = state.stack.last().unwrap_or_else(|| {
        panic!(
            "reach guard: the activation must be on the stack; waiting_for={:?}",
            state.waiting_for
        )
    });
    entry
        .ability()
        .expect("an activated-ability stack entry carries its ResolvedAbility")
}

/// The object-id projection of the plural authority, in payment order. This is
/// exactly what the membership consumer
/// (`exclude_cost_paid_object_that_left_battlefield`) reads, and it is exact
/// for BOTH record variants.
fn paid_ids(ability: &ResolvedAbility) -> Vec<ObjectId> {
    ability
        .cost_paid_objects
        .iter()
        .map(CostPaidObjectRecord::object_id)
        .collect()
}

/// The CAPTURED snapshot a live payment published for `object_id`. Panics when
/// the record carries no provenance, which is the correct reading for every
/// caller here: each of these measurements is of a REAL payment, and a real
/// payment always records `CostPaidObjectRecord::Captured`.
fn captured_snapshot(ability: &ResolvedAbility, object_id: ObjectId) -> &CostPaidObjectSnapshot {
    ability
        .cost_paid_objects
        .iter()
        .filter(|record| record.object_id() == object_id)
        .find_map(CostPaidObjectRecord::snapshot)
        .unwrap_or_else(|| {
            panic!("a live payment must publish a captured snapshot for {object_id:?}")
        })
}

/// The core measurement: one plural snapshot names `expected_id`, that object
/// really moved (reach guard), the move really advanced its incarnation, and
/// the snapshot was RE-PINNED so it still resolves live (CR 400.7j).
///
/// Revert sensitivity lives in the last two assertions: delete the payment
/// seam's `settle_cost_paid_provenance_recursive` call and the snapshot keeps
/// its pre-move epoch, so `snapshot.incarnation == live.incarnation` fails and
/// `live_object_id` yields `None` instead of `Some(id)`.
fn assert_repinned_live(
    state: &GameState,
    snapshot: &CostPaidObjectSnapshot,
    expected_id: ObjectId,
    expected_zone: Zone,
    incarnation_before_payment: u64,
    label: &str,
) {
    assert_eq!(
        snapshot.object_id, expected_id,
        "{label}: the snapshot must name the object this cost consumed"
    );
    let live = state.objects.get(&expected_id).unwrap_or_else(|| {
        panic!("{label}: the cost-paid object's row must survive its own cost move")
    });
    assert_eq!(
        live.zone, expected_zone,
        "{label}: reach guard — the cost's own move actually happened"
    );
    assert_ne!(
        live.incarnation, incarnation_before_payment,
        "{label}: CR 400.7 — the cost's own move must make a new object, otherwise \
         this measurement cannot tell a re-pinned snapshot from a stale one"
    );
    assert_eq!(
        snapshot.incarnation, live.incarnation,
        "{label}: CR 400.7j — the snapshot must be re-pinned to the incarnation the \
         cost's OWN move produced, not left on its pre-move capture"
    );
    assert_eq!(
        snapshot.live_object_id(state),
        Some(expected_id),
        "{label}: CR 400.7j — the cost-paid referent must resolve LIVE immediately \
         after its own cost payment"
    );
}

// ---------------------------------------------------------------------------
// Deterministic discard — Harnfel, Horn of Bounty
// ---------------------------------------------------------------------------

/// CR 701.9a + CR 400.7j: a discard cost moves the card to the graveyard — a
/// public zone — so this same ability's effects can still find it. The pin must
/// therefore survive the cost's own move.
///
/// Fixture non-degeneracy: TWO cards sit in hand, so the discard window offers a
/// real choice and the payment is not the "only possible card" branch.
#[test]
fn deterministic_discard_cost_publishes_a_live_snapshot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harnfel = scenario
        .add_artifact_from_oracle(P0, "Harnfel, Horn of Bounty", HARNFEL)
        .id();
    let paid = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 2, 2)
        .id();
    let kept = scenario.add_creature_to_hand(P0, "Kept Card", 3, 3).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3"]);
    let mut runner = scenario.build();

    let before = incarnation(&runner, paid);
    let index = costed_ability_index(&runner, harnfel);
    runner
        .act(GameAction::ActivateAbility {
            source_id: harnfel,
            ability_index: index,
        })
        .expect("Harnfel's discard ability must be activatable");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            }
        ),
        "reach guard: the real discard cost window must open, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![paid] })
        .expect("discarding an eligible hand card must pay the cost");
    pay_until_on_stack(&mut runner, &[paid]);

    let ability = ability_on_stack(&runner);
    assert_eq!(
        paid_ids(ability),
        vec![paid],
        "the plural authority records exactly the card the cost discarded"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, paid),
        paid,
        Zone::Graveyard,
        before,
        "deterministic discard",
    );

    // Sibling case: the card that was NOT paid is untouched and is not recorded.
    assert_eq!(
        runner.state().objects[&kept].zone,
        Zone::Hand,
        "the unpaid hand card stays in hand"
    );
    assert!(
        !paid_ids(ability).contains(&kept),
        "only objects the cost actually consumed enter the authority"
    );
}

// ---------------------------------------------------------------------------
// Random discard — Pyromancy
// ---------------------------------------------------------------------------

/// CR 701.9b + CR 400.7j: a RANDOM discard cost publishes provenance from the
/// payment's own `RandomDiscardCostPick::snapshot`, captured by the random
/// discard routine before the card left the hand. `commit_random_discard_cost_picks`
/// consumes those snapshots directly and re-pins them; it never reconstructs
/// provenance from an id.
///
/// Fixture non-degeneracy: TWO hand cards, so the seeded RNG genuinely selects
/// one of them and the test does not assume which.
#[test]
fn random_discard_cost_publishes_a_live_snapshot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless_pool(3));
    let pyromancy = scenario
        .add_enchantment_from_oracle(P0, "Pyromancy", PYROMANCY)
        .id();
    let hand: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_creature_to_hand(P0, &format!("Random Filler {i}"), 1, 1)
                .with_mana_cost(ManaCost::generic(3))
                .id()
        })
        .collect();
    let mut runner = scenario.build();

    let before: Vec<(ObjectId, u64)> = hand
        .iter()
        .map(|&id| (id, incarnation(&runner, id)))
        .collect();

    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancy,
            ability_index: 0,
        })
        .expect("Pyromancy's random-discard ability must be activatable");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("Pyromancy targets any target");
    pay_until_on_stack(&mut runner, &hand);

    let discarded: Vec<ObjectId> = hand
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
        .collect();
    assert_eq!(
        discarded.len(),
        1,
        "reach guard: the production RNG path must have discarded exactly one card"
    );
    let discarded = discarded[0];
    let before = before
        .iter()
        .find(|(id, _)| *id == discarded)
        .map(|(_, epoch)| *epoch)
        .expect("the discarded card is one of the seeded hand cards");

    let ability = ability_on_stack(&runner);
    assert_eq!(
        paid_ids(ability),
        vec![discarded],
        "the plural authority records exactly the randomly discarded card"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, discarded),
        discarded,
        Zone::Graveyard,
        before,
        "random discard",
    );
}

// ---------------------------------------------------------------------------
// Random discard redirected into a HIDDEN zone — Pyromancy + a graveyard
// redirect whose outcome is the library
// ---------------------------------------------------------------------------

/// Verbatim from this repository's own existing coverage for this exact
/// grammar: the "Shuffle Probe" fixture in
/// `crates/engine/tests/integration/will_cycle_duration_seam_b1.rs`
/// (`v3c_shuffle_back_outcome_is_unchanged`), which pins that this printed
/// static parses to one `Moved` / `destination_zone: Graveyard` replacement
/// with the shuffle-back outcome. Copied, never paraphrased — a paraphrase can
/// take a different parser branch and go green while the real grammar changes.
///
/// The card is SYNTHETIC on purpose, and so is the case: every printed
/// shuffle-back redirect (Nexus of Fate, Darksteel/Blightsteel Colossus,
/// Progenitus) is self-referential, and a hand card's own replacement is not
/// consulted for the lowered hand → graveyard `ZoneChange`
/// (`object_replacement_candidate_applies` admits an off-battlefield source only
/// as it ENTERS, as it is DISCARDED — i.e. for a `ProposedEvent::Discard`, not
/// the lowered move — or as it leaves the stack). A battlefield-hosted,
/// non-self redirect of the same family is therefore the minimal shape that
/// reaches the seam, which is exactly the "synthetic and latent" case under
/// test. The redirect itself is fully production code: the same printed-static
/// front door, the same replacement pipeline, the same delivery.
const HIDDEN_GRAVEYARD_REDIRECT: &str =
    "If a card would be put into your graveyard from anywhere, shuffle it into its owner's library instead.";

/// One driven Pyromancy random-discard payment, parked on the stack.
///
/// A named struct rather than a tuple: the four values are all id-shaped and a
/// bare tuple both reads ambiguously at the call site and trips
/// `clippy::type_complexity` on the return type.
struct RandomDiscardFixture {
    runner: GameRunner,
    /// The two seeded hand cards, exactly one of which the RNG pays.
    hand: Vec<ObjectId>,
    /// Each hand card's incarnation epoch BEFORE the payment, so the public
    /// control can prove the cost's own move advanced it (CR 400.7).
    before: Vec<(ObjectId, u64)>,
    /// The installed hidden-zone redirect, when this arm installs one.
    probe: Option<ObjectId>,
}

/// Pay Pyromancy's random discard cost through the engine's own windows,
/// optionally with `HIDDEN_GRAVEYARD_REDIRECT` on the battlefield so the paid
/// card's own cost move is redirected into a hidden zone.
///
/// Fixture non-degeneracy: TWO hand cards, so the seeded RNG genuinely selects
/// one of them and neither arm assumes which; the caller discovers the paid card
/// from the board.
fn pay_pyromancy_random_discard(hidden_redirect: bool) -> RandomDiscardFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless_pool(3));
    let pyromancy = scenario
        .add_enchantment_from_oracle(P0, "Pyromancy", PYROMANCY)
        .id();
    let probe = hidden_redirect.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Hidden Redirect Probe", HIDDEN_GRAVEYARD_REDIRECT)
            .id()
    });
    let hand: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_creature_to_hand(P0, &format!("Random Filler {i}"), 1, 1)
                .with_mana_cost(ManaCost::generic(3))
                .id()
        })
        .collect();
    let mut runner = scenario.build();

    let before: Vec<(ObjectId, u64)> = hand
        .iter()
        .map(|&id| (id, incarnation(&runner, id)))
        .collect();

    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancy,
            ability_index: 0,
        })
        .expect("Pyromancy's random-discard ability must be activatable");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("Pyromancy targets any target");
    pay_until_on_stack(&mut runner, &hand);

    RandomDiscardFixture {
        runner,
        hand,
        before,
        probe,
    }
}

/// The single hand card that this payment moved into `zone`.
fn hand_card_now_in(runner: &GameRunner, hand: &[ObjectId], zone: Zone, label: &str) -> ObjectId {
    let moved: Vec<ObjectId> = hand
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].zone == zone)
        .collect();
    assert_eq!(
        moved.len(),
        1,
        "{label}: reach guard — the production RNG path must have paid exactly one card \
         and delivered it to {zone:?}"
    );
    moved[0]
}

/// The plural record naming `object_id`, whichever variant the payment chose.
fn paid_record(ability: &ResolvedAbility, object_id: ObjectId) -> &CostPaidObjectRecord {
    ability
        .cost_paid_objects
        .iter()
        .find(|record| record.object_id() == object_id)
        .unwrap_or_else(|| panic!("the payment must record membership for {object_id:?}"))
}

/// CR 701.9c + CR 400.7j: a random discard cost whose card a replacement put
/// into an UNREVEALED HIDDEN zone still moved that card — membership is exact
/// (CR 601.2c / CR 602.2b) — but the card's characteristics are undefined and
/// CR 400.7j licenses this ability's effects finding only an object the cost
/// moved to a PUBLIC zone. So the plural authority must publish
/// `MembershipOnly`: the id, and nothing else.
///
/// UNDER REVERT: drop the per-pick classification in
/// `commit_random_discard_cost_picks` and every pick is published as
/// `Captured` again. The hidden arm's `snapshot().is_none()` assertion fails
/// immediately, and `live_object_id(state).is_none()` fails too — the repin
/// step would have bound that captured record to the card's post-move
/// incarnation in the hidden zone, which is precisely the live reference
/// CR 701.9c forbids. The membership assertion holds either way, which is why
/// it cannot be the discriminator.
///
/// Both arms run in ONE test so the pair cannot drift, and the PUBLIC arm is a
/// load-bearing positive control: without it, the hidden arm's two `None`s
/// could be satisfied by an authority that publishes nothing at all.
///
/// The paused/resumed payment path inherits this identically: every one of
/// `commit_random_discard_cost_picks`'s five call sites — the completed and the
/// paused arms of `pay_deferred_random_discard_cost`, the resumed paused pick,
/// and the completed and re-paused arms of `resume_random_discard_cost_payment`
/// — classifies through this one function, and the driver above answers any
/// `ReplacementChoice` prompt rather than bypassing it, so whichever of the two
/// routes the pipeline takes ends in the same classification.
#[test]
fn random_discard_cost_redirected_to_a_hidden_zone_publishes_membership_only() {
    // ── HIDDEN ARM ────────────────────────────────────────────────────────
    let fixture = pay_pyromancy_random_discard(true);
    let runner = &fixture.runner;
    let probe = fixture.probe.expect("the hidden arm installs the redirect");

    // Reach guard: the fixture's printed static really did parse into a
    // graveyard-destination replacement. Without this, a parser change that
    // silently dropped the clause would leave the card in the graveyard and the
    // arm below would be measuring the PUBLIC path while claiming the hidden one.
    let hosted = &runner.state().objects[&probe].replacement_definitions;
    assert_eq!(
        hosted
            .iter_unchecked()
            .filter(|def| def.destination_zone == Some(Zone::Graveyard))
            .count(),
        1,
        "reach guard: the redirect must be hosted as exactly one graveyard-destination \
         replacement, got {hosted:?}"
    );

    let paid = hand_card_now_in(runner, &fixture.hand, Zone::Library, "hidden redirect");
    assert!(
        !runner.state().objects[&paid].zone.is_public(),
        "reach guard: CR 701.9c — the cost's own move must have delivered into a \
         HIDDEN zone, otherwise this arm measures nothing"
    );

    let ability = ability_on_stack(runner);
    assert_eq!(
        paid_ids(ability),
        vec![paid],
        "CR 601.2c: membership stays EXACT — the cost really did move this card, \
         so the target-candidate exclusion must still see it"
    );
    let record = paid_record(ability, paid);
    assert!(
        matches!(record, CostPaidObjectRecord::MembershipOnly(id) if *id == paid),
        "CR 701.9c: a card put into an unrevealed hidden zone must be recorded as \
         membership only, got {record:?}"
    );
    assert!(
        record.snapshot().is_none(),
        "CR 701.9c: all values of the card's characteristics are undefined, so no \
         captured snapshot may be exposed"
    );
    assert!(
        record.live_object_id(runner.state()).is_none(),
        "CR 400.7j: only a cost move to a PUBLIC zone lets this ability's effects \
         find the object — the hidden result must refuse to resolve live"
    );
    assert!(
        ability.cost_paid_object.is_none(),
        "the SINGULAR referent already withholds a hidden result; the plural \
         authority must not expose what the singular one refuses"
    );

    // ── PUBLIC POSITIVE CONTROL ───────────────────────────────────────────
    // The same cost, the same driver, the same RNG — only the redirect is gone.
    let control = pay_pyromancy_random_discard(false);
    let runner = &control.runner;
    assert!(
        control.probe.is_none(),
        "the control arm installs no redirect"
    );
    let paid = hand_card_now_in(runner, &control.hand, Zone::Graveyard, "public control");
    let before = control
        .before
        .iter()
        .find(|(id, _)| *id == paid)
        .map(|(_, epoch)| *epoch)
        .expect("the paid card is one of the seeded hand cards");

    let ability = ability_on_stack(runner);
    let record = paid_record(ability, paid);
    assert!(
        matches!(record, CostPaidObjectRecord::Captured(_)),
        "CR 400.7j: a payment delivered to the graveyard — a public zone — keeps \
         full captured provenance, got {record:?}"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, paid),
        paid,
        Zone::Graveyard,
        before,
        "random discard, public destination",
    );
}

// ---------------------------------------------------------------------------
// DETERMINISTIC discard redirected into a HIDDEN zone — Harnfel + the same
// graveyard redirect vehicle
// ---------------------------------------------------------------------------

/// One driven Harnfel deterministic-discard payment, parked on the stack.
///
/// Named struct for the same reason as `RandomDiscardFixture`: the values are
/// all id-shaped and a bare tuple both reads ambiguously and trips
/// `clippy::type_complexity`.
struct DeterministicDiscardFixture {
    runner: GameRunner,
    /// The hand card the PLAYER deliberately selected to pay the cost.
    paid: ObjectId,
    /// An equally eligible hand card that was not selected, so the cost window
    /// is a genuine choice rather than the "only possible card" branch.
    kept: ObjectId,
    /// `paid`'s incarnation epoch BEFORE the payment, so the public control can
    /// prove the cost's own move advanced it (CR 400.7).
    before: u64,
    /// The installed hidden-zone redirect, when this arm installs one.
    probe: Option<ObjectId>,
}

/// Pay Harnfel's deterministic discard cost through the engine's own windows,
/// optionally with `HIDDEN_GRAVEYARD_REDIRECT` on the battlefield so the paid
/// card's own cost move is redirected into a hidden zone.
///
/// Deliberately the SAME redirect vehicle the random-discard fixture installs:
/// `discard_as_cost` and `discard_at_random` both lower to
/// `complete_discard_to_graveyard`'s hand → graveyard `ZoneChange` and both run
/// it through the replacement pipeline (CR 616.1), so one synthetic
/// shuffle-back replacement reaches both. How the card is CHOSEN is not part of
/// the authority rule.
fn pay_harnfel_discard(hidden_redirect: bool) -> DeterministicDiscardFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harnfel = scenario
        .add_artifact_from_oracle(P0, "Harnfel, Horn of Bounty", HARNFEL)
        .id();
    let probe = hidden_redirect.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Hidden Redirect Probe", HIDDEN_GRAVEYARD_REDIRECT)
            .id()
    });
    let paid = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 2, 2)
        .id();
    let kept = scenario.add_creature_to_hand(P0, "Kept Card", 3, 3).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3"]);
    let mut runner = scenario.build();

    let before = incarnation(&runner, paid);
    let index = costed_ability_index(&runner, harnfel);
    runner
        .act(GameAction::ActivateAbility {
            source_id: harnfel,
            ability_index: index,
        })
        .expect("Harnfel's discard ability must be activatable");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            }
        ),
        "reach guard: the real DETERMINISTIC discard cost window must open — the \
         player picks the card — got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![paid] })
        .expect("discarding an eligible hand card must pay the cost");
    pay_until_on_stack(&mut runner, &[paid]);

    DeterministicDiscardFixture {
        runner,
        paid,
        kept,
        before,
        probe,
    }
}

/// CR 701.9c + CR 400.7j: the DETERMINISTIC counterpart of
/// `random_discard_cost_redirected_to_a_hidden_zone_publishes_membership_only`.
///
/// `handle_discard_for_cost` captures every chosen card BEFORE the move (it must
/// — the `lki` records pre-move characteristics, CR 608.2h) and publishes them
/// as `Captured`, then runs each discard through the replacement pipeline so
/// Madness (CR 702.35) and friends can intercept. That pipeline is exactly what
/// can put the card into an unrevealed hidden zone instead, which makes the
/// pre-move `Captured` classification PROVISIONAL: CR 701.9c then leaves the
/// card's characteristics undefined and CR 400.7j licenses this ability's
/// effects finding only an object the cost moved to a PUBLIC zone. Changing how
/// the card is chosen does not change the authority rule, so the deterministic
/// path must settle to `MembershipOnly` exactly as the random one does.
///
/// UNDER REVERT: restore the plain re-pin at
/// `handle_discard_for_cost`'s post-move seam (i.e. drop the demotion arm of
/// `ResolvedAbility::settle_cost_paid_provenance_recursive`) and the hidden
/// arm's record stays `Captured`. The `matches!(.., MembershipOnly)` assertion
/// fails, `snapshot().is_none()` fails, and `live_object_id(state).is_none()`
/// fails hardest of all — the re-pin would have bound that captured record to
/// the card's post-move incarnation IN THE LIBRARY, which is precisely the live
/// reference CR 400.7j withholds. The membership assertion holds either way,
/// which is why it cannot be the discriminator.
///
/// Both arms run in ONE test so the pair cannot drift, and the PUBLIC arm is a
/// load-bearing positive control: without it, the hidden arm's two `None`s could
/// be satisfied by an authority that publishes nothing at all, or by a redirect
/// that silently stopped the payment.
///
/// CONTRAST — this demotion is DISCARD-scoped, not a general hidden-destination
/// policy. `sacrifice_cost_redirected_to_a_hidden_zone_keeps_its_captured_lki`
/// below drives the SAME redirect vehicle on a sacrifice cost and asserts the
/// opposite record shape, because CR 701.9c's undefined-characteristics rule
/// reaches only discards and CR 608.2h keeps a sacrificed permanent's last
/// known information readable. Read the two together before changing either.
///
/// The RESUMED deterministic suffix (`resume_interrupted_cost_payment`, reached
/// when a multi-card discard cost pauses mid-loop on a replacement choice)
/// settles through the same single traversal, so it classifies identically by
/// construction rather than by a second rule.
#[test]
fn deterministic_discard_cost_redirected_to_a_hidden_zone_publishes_membership_only() {
    // ── HIDDEN ARM ────────────────────────────────────────────────────────
    let fixture = pay_harnfel_discard(true);
    let runner = &fixture.runner;
    let probe = fixture.probe.expect("the hidden arm installs the redirect");

    // Reach guard: the fixture's printed static really did parse into a
    // graveyard-destination replacement. Without this, a parser change that
    // silently dropped the clause would leave the card in the graveyard and the
    // arm below would be measuring the PUBLIC path while claiming the hidden one.
    let hosted = &runner.state().objects[&probe].replacement_definitions;
    assert_eq!(
        hosted
            .iter_unchecked()
            .filter(|def| def.destination_zone == Some(Zone::Graveyard))
            .count(),
        1,
        "reach guard: the redirect must be hosted as exactly one graveyard-destination \
         replacement, got {hosted:?}"
    );

    let paid = fixture.paid;
    assert_eq!(
        runner.state().objects[&paid].zone,
        Zone::Library,
        "reach guard: the deterministic discard cost's own move must have been \
         redirected into the library"
    );
    assert!(
        !runner.state().objects[&paid].zone.is_public(),
        "reach guard: CR 701.9c — the cost's own move must have delivered into a \
         HIDDEN zone, otherwise this arm measures nothing"
    );

    let ability = ability_on_stack(runner);
    assert_eq!(
        paid_ids(ability),
        vec![paid],
        "CR 601.2c: membership stays EXACT — the cost really did move this card, \
         so the target-candidate exclusion must still see it"
    );
    let record = paid_record(ability, paid);
    assert!(
        matches!(record, CostPaidObjectRecord::MembershipOnly(id) if *id == paid),
        "CR 701.9c: a deterministically chosen card put into an unrevealed hidden \
         zone must be recorded as membership only, got {record:?}"
    );
    assert!(
        record.snapshot().is_none(),
        "CR 701.9c: all values of the card's characteristics are undefined, so no \
         captured snapshot may be exposed"
    );
    assert!(
        record.live_object_id(runner.state()).is_none(),
        "CR 400.7j: only a cost move to a PUBLIC zone lets this ability's effects \
         find the object — the hidden result must refuse to resolve live"
    );

    // SCOPE — the SINGULAR `cost_paid_object` referent is deliberately NOT
    // asserted here, and the reason is specific to THIS fixture rather than a
    // blanket exclusion. `handle_discard_for_cost` stamps the singular at
    // selection time, and this payment is IMMEDIATE: there is no CR 601.2g
    // mana-ability window, and nothing else, between that stamp and the cost's
    // own move, so the singular capture cannot drift from the object's last
    // known information and there is no capture-refresh behavior to
    // discriminate. (The DEFERRED spell-sacrifice route is the one where that
    // gap exists; `refresh_cost_paid_capture_recursive` refreshes the singular
    // there, and `issue_5252_additional_sacrifice_after_mana_abilities.rs`
    // asserts it.) What this arm also does not assert is the singular's
    // DESTINATION policy under CR 701.9c — the plural record is demoted to
    // membership above, while the singular keeps its own pre-existing
    // hidden-destination behavior, which this change does not alter.

    // Sibling case: the eligible card the player did NOT select is untouched
    // and is not recorded, redirect or no redirect.
    assert_eq!(
        runner.state().objects[&fixture.kept].zone,
        Zone::Hand,
        "the unpaid hand card stays in hand"
    );
    assert!(
        !paid_ids(ability).contains(&fixture.kept),
        "only objects the cost actually consumed enter the authority"
    );

    // ── PUBLIC POSITIVE CONTROL ───────────────────────────────────────────
    // The same card, the same cost, the same driver — only the redirect is gone.
    let control = pay_harnfel_discard(false);
    let runner = &control.runner;
    assert!(
        control.probe.is_none(),
        "the control arm installs no redirect"
    );
    let paid = control.paid;
    assert_eq!(
        runner.state().objects[&paid].zone,
        Zone::Graveyard,
        "reach guard: without the redirect the discard cost's own move delivers to \
         the graveyard (CR 701.9a)"
    );

    let ability = ability_on_stack(runner);
    let record = paid_record(ability, paid);
    assert!(
        matches!(record, CostPaidObjectRecord::Captured(_)),
        "CR 400.7j: a payment delivered to the graveyard — a public zone — keeps \
         full captured provenance, got {record:?}"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, paid),
        paid,
        Zone::Graveyard,
        control.before,
        "deterministic discard, public destination",
    );
}

// ---------------------------------------------------------------------------
// Sacrifice — Greater Good
// ---------------------------------------------------------------------------

/// CR 701.21a + CR 400.7j: a sacrifice cost moves the permanent to its owner's
/// graveyard, so the same publication/re-pin contract applies.
///
/// A NONTOKEN victim is deliberate: a sacrificed token ceases to exist
/// (CR 704.5d) and is purged from `state.objects`, which would make "the pin
/// still resolves live" unmeasurable rather than false.
///
/// Fixture non-degeneracy: TWO eligible creatures, so the sacrifice window is a
/// real choice.
#[test]
fn sacrifice_cost_publishes_a_live_snapshot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let good = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD)
        .as_enchantment()
        .id();
    let victim = scenario.add_creature(P0, "Beast", 5, 5).id();
    let survivor = scenario.add_creature(P0, "Bystander", 2, 2).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    scenario.with_cards_in_hand(P0, &["H1", "H2", "H3"]);
    let mut runner = scenario.build();

    let before = incarnation(&runner, victim);
    let index = costed_ability_index(&runner, good);
    runner
        .act(GameAction::ActivateAbility {
            source_id: good,
            ability_index: index,
        })
        .expect("Greater Good's sacrifice ability must be activatable");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "reach guard: the real sacrifice cost window must open, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("sacrificing an eligible creature must pay the cost");
    pay_until_on_stack(&mut runner, &[victim]);

    let ability = ability_on_stack(&runner);
    assert_eq!(
        paid_ids(ability),
        vec![victim],
        "the plural authority records exactly the sacrificed permanent"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, victim),
        victim,
        Zone::Graveyard,
        before,
        "sacrifice",
    );

    // Sibling case: an equally eligible permanent that was not chosen is
    // neither moved nor recorded.
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "the unchosen creature stays on the battlefield"
    );
    assert!(
        !paid_ids(ability).contains(&survivor),
        "only objects the cost actually consumed enter the authority"
    );
}

// ---------------------------------------------------------------------------
// SACRIFICE redirected into a HIDDEN zone — Greater Good + the same graveyard
// redirect vehicle. The RELOCATION counterpart of the discard demotion above.
// ---------------------------------------------------------------------------

/// One driven Greater Good sacrifice payment, parked on the stack.
///
/// Named struct for the same reason as the discard fixtures: the values are all
/// id-shaped and a bare tuple both reads ambiguously and trips
/// `clippy::type_complexity`.
struct SacrificeFixture {
    runner: GameRunner,
    /// The permanent the PLAYER deliberately selected to pay the cost.
    victim: ObjectId,
    /// An equally eligible creature that was not selected, so the cost window
    /// is a genuine choice rather than the "only possible permanent" branch.
    survivor: ObjectId,
    /// `victim`'s incarnation epoch BEFORE the payment, so both arms can prove
    /// the cost's own move advanced it (CR 400.7).
    before: u64,
    /// The installed hidden-zone redirect, when this arm installs one.
    probe: Option<ObjectId>,
}

/// Pay Greater Good's sacrifice cost through the engine's own windows,
/// optionally with `HIDDEN_GRAVEYARD_REDIRECT` on the battlefield so the
/// sacrificed permanent's own cost move is redirected into a hidden zone.
///
/// Deliberately the SAME redirect vehicle the two discard fixtures install, and
/// it reaches this seam for a reason stated in production code:
/// `sacrifice::apply_sacrifice_after_replacement` proposes the sacrifice's
/// battlefield → graveyard move as its own inner `ZoneChange` through the
/// replacement pipeline precisely so that "a card would be put into a graveyard
/// from anywhere → redirect instead" replacements apply on sacrifice too
/// (CR 701.21a + CR 614.1). The probe's parsed filter is owner-scoped
/// ("your graveyard") and token-excluding ("a card"), and the victim below is a
/// NONTOKEN permanent owned by the probe's controller, so it matches.
fn pay_greater_good_sacrifice(hidden_redirect: bool) -> SacrificeFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let good = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD)
        .as_enchantment()
        .id();
    let probe = hidden_redirect.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Hidden Redirect Probe", HIDDEN_GRAVEYARD_REDIRECT)
            .id()
    });
    let victim = scenario.add_creature(P0, "Beast", 5, 5).id();
    let survivor = scenario.add_creature(P0, "Bystander", 2, 2).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    scenario.with_cards_in_hand(P0, &["H1", "H2", "H3"]);
    let mut runner = scenario.build();

    let before = incarnation(&runner, victim);
    let index = costed_ability_index(&runner, good);
    runner
        .act(GameAction::ActivateAbility {
            source_id: good,
            ability_index: index,
        })
        .expect("Greater Good's sacrifice ability must be activatable");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "reach guard: the real sacrifice cost window must open, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("sacrificing an eligible creature must pay the cost");
    pay_until_on_stack(&mut runner, &[victim]);

    SacrificeFixture {
        runner,
        victim,
        survivor,
        before,
        probe,
    }
}

/// CR 608.2h + CR 400.7j: a NON-discard cost move redirected into a hidden zone
/// must KEEP its captured record — `lki` intact — while refusing to resolve
/// live. This is the row of the settlement matrix that the discard cases do not
/// reach, and it is the opposite answer from theirs.
///
/// Why the two differ. CR 701.9c — the rule that makes a card's characteristics
/// UNDEFINED in an unrevealed hidden zone — is discard-scoped by its own text
/// ("If a card is discarded, but an effect causes it to be put into a hidden
/// zone instead…"). A SACRIFICE is CR 701.21a, not a discard, so CR 701.9c
/// never reaches it and nothing makes the permanent's characteristics
/// undefined. CR 608.2h then supplies the positive rule: an effect that needs
/// information from an object no longer in the public zone it was expected to
/// be in uses that object's LAST KNOWN INFORMATION — which for Greater Good's
/// own "draw cards equal to the sacrificed creature's power" is exactly the
/// pre-move `lki` this record captured. Demoting the record to
/// `MembershipOnly` would DESTROY that `lki`, so demotion is not a safe
/// default: it is wrong in the other direction.
///
/// What still fails closed: the record is deliberately NOT re-pinned either.
/// CR 400.7j licenses this ability's effects finding an object its cost moved
/// to a PUBLIC zone, and the library is not one, so the pin stays on the
/// pre-move incarnation and `live_object_id` yields `None` (CR 400.7: the
/// post-move object is a new object). `Captured`-but-stale is the whole point.
///
/// UNDER REVERT, in BOTH directions — this pair is the discriminator:
///   * revert to the demote-on-any-hidden-destination settlement and the hidden
///     arm's `matches!(.., Captured(_))` and `lki.power == Some(5)` assertions
///     fail, because the record becomes `MembershipOnly` and the last known
///     information is gone;
///   * revert to the original UNCONDITIONAL re-pin and
///     `live_object_id(state).is_none()` fails (it yields `Some(victim)`, a live
///     reference into a hidden zone that CR 400.7j withholds), as does the
///     paired `snapshot.incarnation != live.incarnation` guard.
///
/// Both arms run in ONE test so the pair cannot drift, and the PUBLIC arm is a
/// load-bearing positive control: without it, the hidden arm could be satisfied
/// by an authority that published a stale record for some unrelated reason, or
/// by a redirect that silently stopped the payment.
#[test]
fn sacrifice_cost_redirected_to_a_hidden_zone_keeps_its_captured_lki() {
    // ── HIDDEN ARM ────────────────────────────────────────────────────────
    let fixture = pay_greater_good_sacrifice(true);
    let runner = &fixture.runner;
    let probe = fixture.probe.expect("the hidden arm installs the redirect");

    // Reach guard: the fixture's printed static really did parse into a
    // graveyard-destination replacement. Without this, a parser change that
    // silently dropped the clause would leave the permanent in the graveyard
    // and the arm below would be measuring the PUBLIC path while claiming the
    // hidden one.
    let hosted = &runner.state().objects[&probe].replacement_definitions;
    assert_eq!(
        hosted
            .iter_unchecked()
            .filter(|def| def.destination_zone == Some(Zone::Graveyard))
            .count(),
        1,
        "reach guard: the redirect must be hosted as exactly one graveyard-destination \
         replacement, got {hosted:?}"
    );

    let victim = fixture.victim;
    let live = runner.state().objects.get(&victim).unwrap_or_else(|| {
        panic!("reach guard: a NONTOKEN sacrificed permanent keeps its row (CR 704.5d)")
    });
    assert_eq!(
        live.zone,
        Zone::Library,
        "reach guard: CR 701.21a + CR 614.1 — the sacrifice's own graveyard move must \
         have been redirected into the library by the hosted replacement"
    );
    assert!(
        !live.zone.is_public(),
        "reach guard: the cost's own move must have delivered into a HIDDEN zone, \
         otherwise this arm measures nothing"
    );
    assert_ne!(
        live.incarnation, fixture.before,
        "reach guard: CR 400.7 — the cost's own move must make a new object, otherwise \
         a never-re-pinned snapshot could not be told from a re-pinned one"
    );

    let ability = ability_on_stack(runner);
    assert_eq!(
        paid_ids(ability),
        vec![victim],
        "CR 601.2c: membership stays EXACT — the cost really did move this permanent, \
         so the target-candidate exclusion must still see it"
    );
    let record = paid_record(ability, victim);
    assert!(
        matches!(record, CostPaidObjectRecord::Captured(_)),
        "CR 608.2h: a SACRIFICE is not a discard, so CR 701.9c's undefined-characteristics \
         rule does not reach it — the captured record must survive the hidden \
         destination, got {record:?}"
    );
    let snapshot = captured_snapshot(ability, victim);
    assert_eq!(
        snapshot.lki.power,
        Some(5),
        "CR 608.2h: the pre-move last known information is what 'the sacrificed \
         creature's power' reads; demoting this record would destroy it"
    );
    assert_eq!(
        snapshot.lki.name, "Beast",
        "CR 608.2h: the captured record still names the permanent as it last existed"
    );
    assert_ne!(
        snapshot.incarnation, live.incarnation,
        "CR 400.7j: the pin must NOT be refreshed across a move into a hidden zone — \
         only a public-zone delivery earns a live reference"
    );
    assert!(
        record.live_object_id(runner.state()).is_none(),
        "CR 400.7j: the record must still refuse to resolve LIVE — the stale pin is \
         what fails it closed, not the loss of the snapshot"
    );

    // Sibling case: the equally eligible permanent the player did NOT select is
    // untouched and is not recorded, redirect or no redirect.
    assert_eq!(
        runner.state().objects[&fixture.survivor].zone,
        Zone::Battlefield,
        "the unchosen creature stays on the battlefield"
    );
    assert!(
        !paid_ids(ability).contains(&fixture.survivor),
        "only objects the cost actually consumed enter the authority"
    );

    // ── PUBLIC POSITIVE CONTROL ───────────────────────────────────────────
    // The same permanent, the same cost, the same driver — only the redirect is
    // gone, and the record must then be re-pinned and resolve live.
    let control = pay_greater_good_sacrifice(false);
    let runner = &control.runner;
    assert!(
        control.probe.is_none(),
        "the control arm installs no redirect"
    );
    assert_eq!(
        runner.state().objects[&control.victim].zone,
        Zone::Graveyard,
        "reach guard: without the redirect the sacrifice's own move delivers to the \
         owner's graveyard (CR 701.21a)"
    );

    let ability = ability_on_stack(runner);
    let record = paid_record(ability, control.victim);
    assert!(
        matches!(record, CostPaidObjectRecord::Captured(_)),
        "CR 400.7j: a sacrifice delivered to the graveyard — a public zone — keeps full \
         captured provenance, got {record:?}"
    );
    assert_repinned_live(
        runner.state(),
        captured_snapshot(ability, control.victim),
        control.victim,
        Zone::Graveyard,
        control.before,
        "sacrifice, public destination",
    );
}

// ---------------------------------------------------------------------------
// Exile — Coin of Fate (a cost move into a public zone)
// ---------------------------------------------------------------------------

struct CoinBoard {
    runner: GameRunner,
    grave_a: ObjectId,
    grave_b: ObjectId,
    /// A third graveyard creature card that is eligible but not selected, so
    /// the two-card exile cost is a genuine choice rather than a forced set.
    grave_c: ObjectId,
}

fn coin_board() -> CoinBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, white_pool(4));
    let coin = scenario
        .add_artifact_from_oracle(P0, "Coin of Fate", COIN_OF_FATE)
        .id();
    let grave_a = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature A", 2, 2)
        .id();
    let grave_b = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature B", 3, 3)
        .id();
    let grave_c = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature C", 4, 4)
        .id();
    scenario.add_card_to_library_top(P0, "Library Filler");
    let mut runner = scenario.build();

    let index = costed_ability_index(&runner, coin);
    runner
        .act(GameAction::ActivateAbility {
            source_id: coin,
            ability_index: index,
        })
        .expect("Coin of Fate's ability must be activatable with the cost available");
    CoinBoard {
        runner,
        grave_a,
        grave_b,
        grave_c,
    }
}

/// CR 701.13a + CR 400.7j: Coin's cost exiles two graveyard creature cards.
/// Exile is a public zone, so CR 400.7j is exactly the rule that lets Coin's own
/// effect ("An opponent chooses one of the exiled cards") find them — which is
/// why this seam is the one a live-incarnation consumer will later resolve
/// through. Both entries must be re-pinned, and their payment ORDER preserved.
#[test]
fn exile_cost_publishes_live_snapshots_in_payment_order() {
    let mut board = coin_board();
    let (grave_a, grave_b, grave_c) = (board.grave_a, board.grave_b, board.grave_c);
    let before_a = incarnation(&board.runner, grave_a);
    let before_b = incarnation(&board.runner, grave_b);

    pay_until_on_stack(&mut board.runner, &[grave_a, grave_b]);

    let ability = ability_on_stack(&board.runner);
    let ids = paid_ids(ability);
    let cost_pair: Vec<ObjectId> = ids
        .iter()
        .copied()
        .filter(|id| *id == grave_a || *id == grave_b)
        .collect();
    assert_eq!(
        cost_pair,
        vec![grave_a, grave_b],
        "CR 601.2h: both exiled cards are recorded, in the order they were paid; \
         full authority = {ids:?}"
    );
    assert!(
        !ids.contains(&grave_c),
        "the eligible-but-unselected graveyard card was not consumed by this cost"
    );

    for (id, before, label) in [
        (grave_a, before_a, "exile (first paid)"),
        (grave_b, before_b, "exile (second paid)"),
    ] {
        assert_repinned_live(
            board.runner.state(),
            captured_snapshot(ability, id),
            id,
            Zone::Exile,
            before,
            label,
        );
    }
}

// ---------------------------------------------------------------------------
// Equality contract — the replacement must not narrow `ResolvedAbility` identity
// ---------------------------------------------------------------------------

/// The field this replaced was a raw `Vec<ObjectId>`, whose equality compared
/// exactly the ids. `CostPaidObjectSnapshot` derives a FULL `PartialEq`, so a
/// mechanical swap would have folded `lki` and `incarnation` into every identity
/// check that reaches `ResolvedAbility` equality — `GameState`'s own
/// `PartialEq` over `stack`/`waiting_for`, and stack copy/batch run identity
/// most of all. `ResolvedAbility`'s manual `PartialEq` compares this field by
/// object-id sequence only, preserving the previous semantics exactly.
///
/// Revert sensitivity: restoring a plain `a == b` vector comparison (or deriving
/// `PartialEq`) makes the first assertion fail, because the refreshed clone
/// differs only in the pins.
#[test]
fn plural_cost_paid_identity_is_the_object_id_sequence_only() {
    let mut board = coin_board();
    let (grave_a, grave_b) = (board.grave_a, board.grave_b);
    pay_until_on_stack(&mut board.runner, &[grave_a, grave_b]);
    let ability = ability_on_stack(&board.runner).clone();
    assert!(
        ability.cost_paid_objects.len() >= 2,
        "reach guard: this measurement needs a real multi-object payment, got {:?}",
        paid_ids(&ability)
    );

    let mut repinned = ability.clone();
    for record in repinned.cost_paid_objects.iter_mut() {
        let snapshot = record
            .snapshot_mut()
            .expect("a live payment publishes captured records");
        snapshot.incarnation = snapshot.incarnation.wrapping_add(1);
    }
    assert_eq!(
        ability, repinned,
        "two abilities whose costs consumed the same objects in the same order are \
         the same ability; differing pins must not narrow identity"
    );

    let mut reordered = ability.clone();
    reordered.cost_paid_objects.reverse();
    assert_ne!(
        ability, reordered,
        "a DIFFERENT object-id sequence is a different ability — the id-sequence \
         comparison must not be vacuously true"
    );
}

// ---------------------------------------------------------------------------
// Restore contract — missing provenance fails closed
// ---------------------------------------------------------------------------

/// `ResolvedAbility` reaches the wire through `GameState` restore / P2P paths.
/// A `CostPaidObjectSnapshot` written before the incarnation epoch existed
/// cannot prove which incarnation it bound, so serde defaults it to
/// `LEGACY_INCARNATION`, which no live object can ever match: the record reads
/// as stale rather than silently naming whatever object holds the id today
/// (CR 400.7). Measured on a REAL snapshot produced by a real cost payment.
#[test]
fn a_legacy_snapshot_without_an_incarnation_fails_closed() {
    let mut board = coin_board();
    let (grave_a, grave_b) = (board.grave_a, board.grave_b);
    pay_until_on_stack(&mut board.runner, &[grave_a, grave_b]);
    let ability = ability_on_stack(&board.runner).clone();
    let live = captured_snapshot(&ability, grave_a).clone();
    // Reach guard: the record we are about to downgrade really is live now, so
    // the fail-closed assertion below cannot pass for the wrong reason.
    assert_eq!(
        live.live_object_id(board.runner.state()),
        Some(grave_a),
        "the freshly paid snapshot must resolve live before it is downgraded"
    );

    let mut json = serde_json::to_value(&live).expect("a cost-paid snapshot serializes");
    json.as_object_mut()
        .expect("a snapshot serializes as a JSON object")
        .remove("incarnation");
    let legacy: CostPaidObjectSnapshot =
        serde_json::from_value(json).expect("a pre-migration snapshot must still restore");

    assert_eq!(
        legacy.object_id, grave_a,
        "the legacy record still carries its storage id"
    );
    assert_eq!(
        legacy.incarnation, LEGACY_INCARNATION,
        "a record with no captured epoch is pinned to the sentinel"
    );
    assert!(
        !legacy.is_current(board.runner.state()),
        "CR 400.7: the sentinel can never match a live object"
    );
    assert_eq!(
        legacy.live_object_id(board.runner.state()),
        None,
        "fail closed: a legacy record must resolve to nothing rather than rebind by storage id"
    );
}

/// The other half of the restore contract: a persisted ability that predates
/// the snapshot authority carries a raw `cost_paid_object_ids` id list.
///
/// Those ids are exact MEMBERSHIP and no provenance, and the two halves must be
/// answered differently (CR 601.2c vs CR 400.7):
///   * membership PRESERVED — every id comes back, in payment order, so
///     `exclude_cost_paid_object_that_left_battlefield` still knows which
///     objects this ability's own cost consumed;
///   * liveness REFUSED — each migrated record exposes no snapshot and resolves
///     live to nothing, because a reusable storage id cannot prove which
///     incarnation it named.
///
/// Measured on a REAL resolving ability, downgraded to the legacy shape.
#[test]
fn a_legacy_raw_id_payload_preserves_membership_without_live_authority() {
    let mut board = coin_board();
    let (grave_a, grave_b) = (board.grave_a, board.grave_b);
    pay_until_on_stack(&mut board.runner, &[grave_a, grave_b]);
    let ability = ability_on_stack(&board.runner).clone();
    let live_ids = paid_ids(&ability);
    assert!(
        live_ids.len() >= 2,
        "reach guard: this measurement needs a real MULTI-object payment to downgrade, got \
         {live_ids:?}"
    );

    let mut json = serde_json::to_value(&ability).expect("a resolved ability serializes");
    let object = json
        .as_object_mut()
        .expect("a resolved ability serializes as a JSON object");
    assert!(
        object.contains_key("cost_paid_objects"),
        "a populated authority is written to the wire"
    );
    object.remove("cost_paid_objects");
    object.insert(
        "cost_paid_object_ids".to_string(),
        serde_json::Value::Array(
            live_ids
                .iter()
                .map(|id| serde_json::json!(id.0))
                .collect::<Vec<_>>(),
        ),
    );

    let restored: ResolvedAbility =
        serde_json::from_value(json).expect("a pre-migration ability payload must still restore");

    assert_eq!(
        paid_ids(&restored),
        live_ids,
        "CR 601.2c: membership must survive the migration exactly, in payment order — \
         dropping it fails the target-candidate exclusion OPEN for every paid object \
         after the first"
    );
    for record in &restored.cost_paid_objects {
        assert!(
            matches!(record, CostPaidObjectRecord::MembershipOnly(_)),
            "a raw id can only migrate to a membership record: {record:?}"
        );
        assert!(
            record.snapshot().is_none(),
            "CR 608.2h: no LKI may be synthesized for a record that never captured one"
        );
        assert_eq!(
            record.live_object_id(board.runner.state()),
            None,
            "CR 400.7: fail closed — a membership record must never resolve to a live \
             object, even though that object is on the board right now"
        );
        assert!(
            !record.is_current(board.runner.state()),
            "CR 400.7: a membership record can never read as current"
        );
    }
}
