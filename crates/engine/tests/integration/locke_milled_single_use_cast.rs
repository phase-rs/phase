//! CR 601.2a + CR 611.2a: Locke, Treasure Hunter — a
//! duration-scoped cast permission over a MILLED batch, capped at one.
//!
//! Locke is the first shipped card to pair a non-exile pool with a serialized
//! `single_use` grant, and it reached three separate gaps that each looked local
//! and were not:
//!
//!   1. **Routing.** The clause lowered to
//!      `Unimplemented { name: "unrepresentable_cast_cap" }`. The `from among`
//!      batch authority selects `CastMechanism::LingeringPermission` for a PAID
//!      cast, and `for_batch_bounds` refuses that mechanism a printed cap of one
//!      because it records an INDEPENDENT permission per object with no shared
//!      budget. The shape that does carry a grant-scoped budget of one already
//!      existed — `CastingPermission::PlayFromExile { single_use: true }`, which
//!      Chandra, Hope's Beacon +1 has used since it shipped — and the batch
//!      surfaces simply had no route to it.
//!
//!   2. **The cap was unenforceable off-exile.** The capture that writes the
//!      spent-grant ledger was gated on `source_zone == Zone::Exile`
//!      (`casting_costs.rs`), while the eligibility gate that READS the ledger
//!      (`play_from_exile_permission_source_at_index`) is zone-agnostic. A
//!      graveyard-sourced cast therefore never wrote the ledger and the gate kept
//!      passing, so a grant printing "a spell" authorized a SECOND one.
//!
//!   3. **The sibling sweep was zone-blind in the wrong direction.**
//!      `consume_single_use_play_from_exile` iterated `state.exile` alone, so
//!      milled siblings sitting in graveyards kept a permission the engine had
//!      just declared spent.
//!
//! Fixes 2 and 3 are only observable once 1 lands, which is why they ship
//! together: before the routing fix nothing constructed a `single_use` grant over
//! a non-exile pool, so neither defect had a carrier.
//!
//! Verbatim Oracle text below is from Scryfall `cards/named` (`exact=`), not from
//! memory.

use engine::ai_support::legal_actions;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastingPermission, Duration, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall `cards/named?exact=Locke, Treasure Hunter`).
/// Only the triggered ability is used; the evasion static is irrelevant here and
/// is kept so the parse under test is the card's, not a fragment's.
const LOCKE: &str = "Locke can't be blocked by creatures with greater power.\n\
     Mug — Whenever Locke attacks, each player mills a card. If a land card was \
     milled this way, create a Treasure token. Until end of turn, you may cast a \
     spell from among those cards.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .zone
}

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// CR 116.2a: a land is PLAYED, never cast, so it surfaces as
/// `GameAction::PlayLand` — a different action than `can_cast` inspects.
fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// The `single_use` `PlayFromExile` grants recorded on `id`, with the duration
/// each one carries. Reads the permission rather than the legal-action surface,
/// so a test can distinguish "the grant was never installed" from "the grant is
/// installed but something else suppresses the action".
fn single_use_grant_durations(runner: &GameRunner, id: ObjectId) -> Vec<Duration> {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .filter_map(|permission| match permission {
            CastingPermission::PlayFromExile {
                duration,
                single_use: true,
                ..
            } => Some(duration.clone()),
            _ => None,
        })
        .collect()
}

/// Two players, Locke attacking, one known card on top of each library.
///
/// Both milled cards are given a zero mana cost so the cap under test is the
/// GRANT's and not the player's mana. `false` is the land flag: CR 305.1 makes a
/// land a special action rather than a cast, and Locke's grant is `mode: Cast`,
/// so a milled land would be uncastable for a reason unrelated to the cap.
struct Milled {
    runner: GameRunner,
    locke: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
}

fn locke_attacks() -> Milled {
    let mut scenario = GameScenario::new();
    // CR 504.1: start past the draw step. Without this the scenario advances
    // through P0's draw on the way to combat and the card seeded on top of the
    // library is in hand before Locke ever attacks.
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario
        .add_creature_from_oracle(P0, "Locke, Treasure Hunter", 3, 3, LOCKE)
        .id();
    let mine = scenario
        .add_spell_to_library_top(P0, "My Milled Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let theirs = scenario
        .add_spell_to_library_top(P1, "Their Milled Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    runner.advance_until_stack_empty();
    Milled {
        runner,
        locke,
        mine,
        theirs,
    }
}

/// Does this parse still carry the honest `from among` cast-cap refusal anywhere?
///
/// Searched over the serialized tree rather than by walking `AbilityDefinition`
/// by hand: the gap is identified by a NAME that is a single `const` in the
/// parser (`UNREPRESENTABLE_CAST_CAP_GAP`), so a string search over the exported
/// shape cannot drift from it the way a hand-written arm walk can, and the tests
/// here care about "anywhere in the chain" rather than about a position.
fn has_unrepresentable_cast_cap_gap(parsed: &engine::parser::oracle::ParsedAbilities) -> bool {
    serde_json::to_string(parsed)
        .expect("a parsed ability tree serializes")
        .contains("unrepresentable_cast_cap")
}

/// Does this parse install a single-use `PlayFromExile` grant anywhere?
fn has_single_use_grant(parsed: &engine::parser::oracle::ParsedAbilities) -> bool {
    let json = serde_json::to_string(parsed).expect("a parsed ability tree serializes");
    json.contains("\"PlayFromExile\"") && json.contains("\"single_use\":true")
}

/// The `card_filter` of every `single_use` `PlayFromExile` grant in a parse.
///
/// Returns the filters structurally rather than as serialized text: a substring
/// check for one type name passes on a filter that kept only that leg, which is
/// the narrowed-restriction defect in miniature.
fn single_use_grant_card_filters(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> Vec<Option<TargetFilter>> {
    fn walk(definition: &AbilityDefinition, out: &mut Vec<Option<TargetFilter>>) {
        if let Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    card_filter,
                    single_use: true,
                    ..
                },
            ..
        } = definition.effect.as_ref()
        {
            out.push(card_filter.clone());
        }
        if let Some(sub) = definition.sub_ability.as_deref() {
            walk(sub, out);
        }
        if let Some(alt) = definition.else_ability.as_deref() {
            walk(alt, out);
        }
    }
    let mut found = Vec::new();
    for definition in &parsed.abilities {
        walk(definition, &mut found);
    }
    for execute in parsed.triggers.iter().filter_map(|t| t.execute.as_deref()) {
        walk(execute, &mut found);
    }
    found
}

/// Walk from the declare-attackers step to the postcombat main phase.
///
/// The scenario driver's `advance_to_phase` passes priority in pairs and stops
/// the moment the engine surfaces something that is not a priority window.
/// CR 509.1 makes declare-blockers a turn-based action rather than a priority
/// window, so the walk has to answer it explicitly. No blocks are declared —
/// this test is about the cast permission, not about combat.
fn advance_past_combat(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("the defending player may always decline to block");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is always legal");
            }
            other => panic!("combat stalled on an unexpected prompt: {other:?}"),
        }
        runner.advance_until_stack_empty();
    }
    panic!("combat did not reach the postcombat main phase");
}

/// The published set is the MILLED batch and nothing else,
/// and it is published *because* the grant references it.
///
/// Two guarantees in one test, because they share a setup and each is the other's
/// reach guard.
///
/// **(1) The demand link.** Tracked-set publication is DEMAND-DRIVEN:
/// `publish_tracked_set_for_resolution` publishes only when
/// `next_sub_needs_tracked_set` finds a downstream consumer such as
/// `GrantCastingPermission { target: TrackedSet }`. Before this change Locke's
/// cast clause was an `Unimplemented` node that referenced nothing, so the mill
/// published NOTHING (measured: `tracked_object_sets` empty). The routing fix
/// restores the link implicitly rather than by a separate step — which is exactly
/// why it needs pinning. If a refactor ever breaks the link, publication silently
/// stops and the `TrackedSet { id: 0 }` sentinel falls through to
/// `resolve_tracked_set_sentinel`'s fail-open rung, which binds an unrelated
/// earlier set. That failure is invisible at the action surface (cards are still
/// offered — the WRONG cards), so it has to fail loudly here.
///
/// **(2) No Treasure contamination.** Locke creates a token BETWEEN the mill and
/// the cast clause. A token swept into the published set would be offered as a
/// castable member of "those cards", which the card does not say. Could not be
/// tested before the routing fix, because nothing published.
#[test]
fn the_published_set_is_exactly_the_milled_cards_and_excludes_the_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario
        .add_creature_from_oracle(P0, "Locke, Treasure Hunter", 3, 3, LOCKE)
        .id();
    // A LAND on top, so the conditional "If a land card was milled this way"
    // fires and a Treasure exists to contaminate the set with. Without it this
    // test's contamination half would be vacuous.
    let milled_land = scenario
        .add_spell_to_library_top(P0, "Milled Land", false)
        .as_land()
        .id();
    let theirs = scenario
        .add_spell_to_library_top(P1, "Their Milled Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    runner.advance_until_stack_empty();

    // Reach guard for the contamination half: a Treasure really was created.
    let treasures: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| obj.zone == Zone::Battlefield && obj.name == "Treasure")
        .map(|(id, _)| *id)
        .collect();
    assert!(
        !treasures.is_empty(),
        "reach guard: milling a land must create the Treasure, or the exclusion \
         assertion below has nothing to exclude"
    );

    let published: Vec<Vec<ObjectId>> = runner
        .state()
        .tracked_object_sets
        .values()
        .cloned()
        .collect();
    assert_eq!(
        published.len(),
        1,
        "the demand link must publish exactly one tracked set for this resolution; \
         an EMPTY map means `next_sub_needs_tracked_set` no longer sees the grant \
         and the `TrackedSet {{ id: 0 }}` sentinel will fail open onto an unrelated set"
    );
    let mut members = published.into_iter().next().expect("checked above");
    members.sort();
    let mut expected = vec![milled_land, theirs];
    expected.sort();
    assert_eq!(
        members, expected,
        "\"those cards\" is the milled batch — the Treasure created \
         between the mill and the cast clause is not one of them"
    );
    for treasure in treasures {
        assert!(
            single_use_grant_durations(&runner, treasure).is_empty(),
            "the Treasure must not carry the cast grant"
        );
    }
}

/// CR 611.2a: a duration stated by one clause must not promote a LATER capped
/// clause that states none.
///
/// `ParseContext::stated_clause_duration` is the channel that carries a peeled
/// duration to the `from among` mechanism decision, and its failure direction is
/// OPEN: a set/clear lifecycle (rather than save/restore) would leave clause N's
/// duration standing while clause N+1 is lowered, turning a CR 608.2g
/// resolution-window one-shot into a lingering permission the card never printed.
/// That is strictly more permissive than the instruction, and it is invisible to a
/// card-by-card parse delta unless the corpus happens to contain such a pair.
///
/// No printed card exercises this shape today, which is the point: a guard on a
/// currently-unreachable path is the difference between a latent fail-open and a
/// live one.
///
/// DISCRIMINATING: the same second sentence WITH its own leading duration does
/// promote, so this is not "the second clause stopped parsing".
#[test]
fn a_stated_duration_does_not_leak_into_the_next_clause() {
    let leaked = parse_oracle_text(
        "Whenever this creature attacks, until end of turn, creatures you control \
         get +1/+1. Exile the top card of each player's library. You may cast a \
         spell from among those cards.",
        "Leak Probe",
        &[],
        &[],
        &[],
    );
    assert!(
        has_unrepresentable_cast_cap_gap(&leaked),
        "CR 608.2g: the cast clause states no duration of its own, so it must keep \
         its refusal — the duration printed on the FIRST clause is not its"
    );
    assert!(
        !has_single_use_grant(&leaked),
        "a leaked duration must not promote the later clause to a lingering grant"
    );

    // Reach guard: the identical cast sentence, carrying its OWN leading duration,
    // does promote. Without this row the assertions above could pass because the
    // grammar stopped being recognized at all — which is exactly how the first
    // draft of this test went green (its text did not chunk into a chain and the
    // whole line lowered to an unrelated `static_structure` gap).
    let stated = parse_oracle_text(
        "Whenever this creature attacks, until end of turn, creatures you control \
         get +1/+1. Exile the top card of each player's library. Until end of turn, \
         you may cast a spell from among those cards.",
        "Reach Probe",
        &[],
        &[],
        &[],
    );
    assert!(
        has_single_use_grant(&stated),
        "reach guard: the same clause WITH its own stated duration must promote to \
         the single-use grant"
    );
    assert!(
        !has_unrepresentable_cast_cap_gap(&stated),
        "reach guard: the promoted clause must no longer carry the refusal"
    );
}

/// Locke's grant binds the set THIS resolution published, never an
/// unrelated earlier one.
///
/// `resolve_tracked_set_sentinel` (`game/targeting.rs`) resolves the
/// `TrackedSet { id: 0 }` sentinel through a ladder, and its third rung is
/// `latest_tracked_set_id` — the most recently published set, whatever produced
/// it. `tracked_object_sets` is append-only and never cleared, so that rung is
/// FAIL-OPEN: with nothing published for the current chain it silently binds a
/// stale, unrelated set, the parse stays clean, and the controller is offered
/// arbitrary cards.
///
/// Locke reaches rung 1 (`chain_tracked_set_id`) today because the mill publishes
/// on demand from the grant that references it. That makes the hazard LATENT
/// rather than fixed — and a latent fail-open with no guard is how it comes back.
/// This test makes rung 3 observable: a prior, unrelated tracked set is published
/// first, so if Locke's chain ever stops publishing, the sentinel falls to that
/// set and the assertions below fail with the stale card carrying Locke's grant.
///
/// POSITIVE REACH GUARD: the stale set must actually exist and be DISTINCT from
/// Locke's, or "the stale card has no Locke grant" is vacuously true.
#[test]
fn lockes_grant_binds_this_resolutions_set_not_a_stale_published_one() {
    // Chandra, Hope's Beacon's +1 clause, on an ETB — a different card, whose only
    // job here is to publish a tracked set BEFORE Locke's trigger runs.
    const PRIOR_PUBLISHER: &str = "When this creature enters, exile the top card of your \
         library. Until the end of your next turn, you may cast an instant or sorcery \
         spell from among those exiled cards.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario
        .add_creature_from_oracle(P0, "Locke, Treasure Hunter", 3, 3, LOCKE)
        .id();
    let publisher = scenario
        .add_creature_to_hand_from_oracle(P0, "Prior Publisher", 1, 1, PRIOR_PUBLISHER)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Top of P0's library, added bottom-first: `add_spell_to_library_top` pushes
    // each new card above the previous one, so `stale` ends up on top and the
    // publisher's exile takes it; Locke's mill then takes `mine`.
    let mine = scenario
        .add_spell_to_library_top(P0, "My Milled Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let stale = scenario
        .add_spell_to_library_top(P0, "Stale Set Member", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let theirs = scenario
        .add_spell_to_library_top(P1, "Their Milled Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(publisher).commit();
    runner.advance_until_stack_empty();
    let sets_before = runner.state().tracked_object_sets.len();
    assert_eq!(
        sets_before, 1,
        "reach guard: the prior card must publish a tracked set, or there is no \
         stale set for the sentinel to fall onto and this test proves nothing"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().tracked_object_sets.len(),
        2,
        "reach guard: Locke's mill must publish its OWN set alongside the stale one"
    );
    assert_eq!(
        zone_of(&runner, stale),
        Zone::Exile,
        "reach guard: the stale set's member is the exiled card, not a milled one"
    );

    // The load-bearing assertion. Locke's grant is `UntilEndOfTurn`; the prior
    // publisher's is `UntilEndOfNextTurnOf`, so the duration identifies WHICH
    // grant reached the card. Rung 3 would put Locke's grant on the stale card.
    assert!(
        !single_use_grant_durations(&runner, stale).contains(&Duration::UntilEndOfTurn),
        "Locke's grant must bind the set its own resolution published — \
         finding it on a member of an unrelated earlier set means the sentinel fell \
         through to `latest_tracked_set_id`"
    );
    for (label, id) in [("controller's", mine), ("opponent's", theirs)] {
        assert_eq!(
            single_use_grant_durations(&runner, id),
            vec![Duration::UntilEndOfTurn],
            "{label} milled card must carry Locke's grant"
        );
    }
}

/// CR 611.2a + CR 601.2a: the clause installs a single-use cast grant scoped to
/// the milled batch and bounded by the printed "until end of turn".
///
/// POSITIVE REACH GUARDS, all three required: both cards actually moved
/// Library → Graveyard (the mill ran), Locke is still attacking (the trigger
/// fired from the real combat step rather than from a hand-built ability), and
/// the grant is recorded on a card that is NOT in exile. Without the third, this
/// test would pass unchanged against the pre-fix exile-only machinery.
///
/// DISCRIMINATING: reverting the routing promotion leaves the clause as
/// `Unimplemented { name: "unrepresentable_cast_cap" }`, so no permission is
/// recorded at all and `single_use_grant_durations` returns empty.
#[test]
fn locke_grants_a_single_use_cast_until_end_of_turn() {
    let Milled {
        runner,
        locke,
        mine,
        theirs,
    } = locke_attacks();

    assert_eq!(
        (zone_of(&runner, mine), zone_of(&runner, theirs)),
        (Zone::Graveyard, Zone::Graveyard),
        "reach guard: each player must have milled their top card"
    );
    assert_eq!(
        zone_of(&runner, locke),
        Zone::Battlefield,
        "reach guard: the attack trigger must have fired from a live attacker"
    );

    for (label, id) in [("controller's", mine), ("opponent's", theirs)] {
        assert_eq!(
            single_use_grant_durations(&runner, id),
            vec![Duration::UntilEndOfTurn],
            "{label} milled card must carry the single-use grant bounded by the \
             printed \"Until end of turn\" — a `Duration::Permanent` here means the \
             placeholder was never patched by `apply_duration_to_effect`"
        );
        assert_ne!(
            zone_of(&runner, id),
            Zone::Exile,
            "reach guard ({label}): the grant must be installed on a GRAVEYARD-resident \
             card — this is what the exile-only machinery could not do"
        );
    }
}

/// CR 601.2a: "you may cast **a** spell" is ONE cast across the whole window,
/// shared by every card in the batch.
///
/// This is the assertion the two runtime fixes exist for. With either of them
/// reverted the sibling keeps its grant: without the capture widening the
/// spent-grant ledger is never written (the capture was gated on
/// `source_zone == Zone::Exile` and Locke's pool is the graveyard), and without
/// the zone-blind sweep `consume_single_use_play_from_exile` iterates `state.exile`
/// and never reaches a card sitting in a graveyard.
///
/// POSITIVE REACH GUARDS: the controller's milled card is castable at the action
/// surface before the cast, and BOTH milled cards carry the grant. A bare "the
/// sibling lost its grant" assertion would pass vacuously if the grant had never
/// been installed, which is exactly how this class of test goes green against a
/// broken engine.
///
/// CR 601.3 + CR 611.2a: a type restriction stated as a SUFFIX, as a HEAD, or as
/// BOTH is never silently dropped by the promotion.
///
/// The promotion reads the type gate off the cast HEAD (`parse_cast_type_gate`)
/// and discards the caller's target, which it must — that target carries an
/// exile-zone leg that is wrong for a milled pool. But
/// `parse_from_among_exiled_this_way` lifts a SUFFIX gate ("cast a spell from
/// among the **instant or sorcery** cards exiled this way") into exactly that
/// discarded target.
///
/// Two widenings, measured, one guard:
///   * suffix only — promoted with `card_filter: None`, authorizing every member
///     of the tracked set regardless of type.
///   * **head AND suffix together** — "cast an **artifact** spell from among the
///     **instant or sorcery** cards exiled this way" promoted with
///     `card_filter: Some(Artifact)`, keeping the head and dropping the suffix,
///     so an artifact that is neither an instant nor a sorcery became castable.
///     An earlier guard that asked only `head_gate.is_none()` caught the first
///     and passed the second; the guard now compares the discarded restriction
///     against the installed filter, which covers both with one question.
///
/// The refusal is an engine lowering limitation, not a rule — no CR speaks to
/// where in a sentence a restriction is printed. CR 601.3 is cited for why the
/// restriction matters at all: a player may begin to cast a spell only if an
/// effect allows it, and a grant restricted to instants does not allow an
/// artifact.
///
/// ASSERTS THE ABSENCE OF THE GRANT, not a gap name, and the cap-of-two row is
/// why. That sibling has always refused through the pre-existing path and both
/// land on the same generic `effect_structure` gap — a property of the sentence
/// shape, not of this guard, so pinning the name would pin unrelated behaviour.
#[test]
fn a_dropped_type_gate_refuses_instead_of_widening_the_grant() {
    for (label, oracle) in [
        (
            "suffix only",
            "Exile the top five cards of your library. Until end of turn, you may cast \
             a spell from among the instant or sorcery cards exiled this way.",
        ),
        (
            "head and suffix together",
            "Exile the top five cards of your library. Until end of turn, you may cast \
             an artifact spell from among the instant or sorcery cards exiled this way.",
        ),
    ] {
        let parsed = parse_oracle_text(oracle, "Gate Probe", &[], &[], &[]);
        assert!(
            !has_single_use_grant(&parsed),
            "{label}: a type restriction the promotion cannot carry must refuse the \
             clause, never produce a grant that authorizes more than the card does"
        );
    }

    // The cap-of-two sibling refuses through the pre-existing path. Identical
    // outcome, which is what establishes that the guard lands these clauses where
    // the family already lands rather than inventing a failure mode.
    let cap_two = parse_oracle_text(
        "Exile the top five cards of your library. Until end of turn, you may cast \
         up to two spells from among the instant or sorcery cards exiled this way.",
        "Cap Two",
        &[],
        &[],
        &[],
    );
    assert!(
        !has_single_use_grant(&cap_two),
        "the pre-existing refusal path must also install no grant"
    );

    // DISCRIMINATING CONTROLS. Without these every assertion above would pass if
    // the promotion had simply stopped working.
    //
    // (1) No printed restriction at all: nothing can be lost, so it still
    //     promotes. This is what proves the guard fires on the dropped
    //     restriction and not on the `exiled this way` surface itself.
    let ungated = parse_oracle_text(
        "Exile the top five cards of your library. Until end of turn, you may cast \
         a spell from among the cards exiled this way.",
        "Ungated",
        &[],
        &[],
        &[],
    );
    assert!(
        has_single_use_grant(&ungated),
        "control: with no printed type restriction there is nothing to lose, so \
         the same surface must still promote"
    );

    // (2) HEAD-stated restriction: recovered by `parse_cast_type_gate`, so the
    //     discarded target carries nothing the installed filter lacks.
    let head_gated = parse_oracle_text(
        "Exile the top five cards of your library. Until end of turn, you may cast \
         an instant or sorcery spell from among them.",
        "Head Gate",
        &[],
        &[],
        &[],
    );
    assert!(
        has_single_use_grant(&head_gated),
        "control: a HEAD-stated type restriction must still promote"
    );
    // BOTH legs asserted, not just one: a filter that retained only `Instant`
    // would satisfy a substring check for `Instant` while still dropping half the
    // printed restriction — the same silent narrowing in miniature.
    let filters = single_use_grant_card_filters(&head_gated);
    assert_eq!(
        filters,
        vec![Some(TargetFilter::Typed(TypedFilter::new(
            TypeFilter::AnyOf(vec![TypeFilter::Instant, TypeFilter::Sorcery])
        )))],
        "control: the promoted grant must carry BOTH printed legs of the type \
         restriction, or this row would pass on exactly the narrowed filter the \
         guard exists to prevent"
    );
}

/// CR 116.2a + CR 611.2a: playing one granted land SPENDS the single-use budget,
/// so its sibling in the batch becomes unplayable.
///
/// The cast path spends its grant through `consume_single_use_play_from_exile`;
/// the land path reached `finalize_committed_land_play` with no authorization at
/// all for a graveyard-resident land, because the capture was gated on the exile
/// zone. `record_graveyard_play_permission` only handles a static
/// `GraveyardCastPermission`, so nothing spent the budget and every sibling land
/// in the batch stayed playable.
///
/// THE SECOND LAND DROP IS LOAD-BEARING FOR THE ACTION-GATE HALF, and the two
/// halves differ — measured, because the obvious reading is wrong for one of
/// them. `graveyard_lands_playable_by_permission` is a PERMISSION sweep and does
/// not consult the CR 116.2a one-per-turn limit, so the discovery assertion below
/// discriminates with or without an extra land drop. The `act(PlayLand)`
/// assertion does not: the action gate enforces the limit, so without a
/// `MayPlayAdditionalLand` source it would error for the wrong reason and pass
/// against a broken engine. Both assertions are kept because they check different
/// surfaces agree; the extra land drop is what keeps the second one honest.
/// Reverting the consumption fails the discovery assertion.
#[test]
fn playing_one_granted_land_spends_the_batch_budget_for_its_sibling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Cross-Owner Mill Source",
            1,
            1,
            "Whenever this creature attacks, each player mills a card. You may play \
             a land from among those cards this turn.",
        )
        .id();
    // CR 116.2a: a second legal land drop, so the one-per-turn limit cannot
    // masquerade as the grant being spent.
    scenario
        .add_creature(P0, "Additional Land Drop", 1, 1)
        .with_static(StaticMode::MayPlayAdditionalLand);
    let my_land = scenario
        .add_spell_to_library_top(P0, "My Milled Land", false)
        .as_land()
        .id();
    let their_land = scenario
        .add_spell_to_library_top(P1, "Their Milled Land", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .expect("the source must be able to attack");
    runner.advance_until_stack_empty();
    advance_past_combat(&mut runner);

    // Reach guards. Both lands must be milled into their OWNERS' graveyards and
    // both must be playable before the budget is spent, or the negative below is
    // an empty-set accident.
    assert_eq!(
        (zone_of(&runner, my_land), zone_of(&runner, their_land)),
        (Zone::Graveyard, Zone::Graveyard),
        "reach guard: both lands must have been milled"
    );
    let playable_before =
        engine::game::casting::graveyard_lands_playable_by_permission(runner.state(), P0);
    for (label, id) in [("controller's", my_land), ("opponent's", their_land)] {
        assert!(
            playable_before.iter().any(|(obj, _)| *obj == id),
            "reach guard ({label}): both milled lands must be playable before the \
             grant is spent"
        );
    }

    let card_id = runner.state().objects[&my_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: my_land,
            card_id,
        })
        .expect("the first granted land play must be accepted");
    assert_eq!(
        zone_of(&runner, my_land),
        Zone::Battlefield,
        "reach guard: the first land must actually have been played"
    );

    // The load-bearing pair. A second land drop IS available, so a still-playable
    // sibling here means the budget was never spent.
    assert!(
        !engine::game::casting::graveyard_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(obj, _)| *obj == their_land),
        "CR 116.2a + CR 611.2a: the grant printed a budget of ONE, so playing the first land \
         must make its sibling unplayable — a second land drop is available, so \
         the one-per-turn limit is not what is stopping it"
    );
    let their_card_id = runner.state().objects[&their_land].card_id;
    assert!(
        runner
            .act(GameAction::PlayLand {
                object_id: their_land,
                card_id: their_card_id,
            })
            .is_err(),
        "the action gate must also refuse the sibling, not merely hide it from \
         discovery — the two halves have to agree"
    );
}

/// A STATIC `ExileCastPermission { play_mode: Play }` with a this-turn pool and a
/// `SourceController` grantee — the one shape that reaches the defect below.
///
/// SYNTHETIC, and deliberately so: no printed card has this shape today
/// (measured over the card-data export: 13 `ExileCastPermission` statics, none
/// this-turn + play + source-controller). The parser does support it — this text
/// lowers to exactly that static — so it is a latent path, not an impossible one.
/// Uba Mask looks like the natural carrier and CANNOT reach it: its
/// `EachPlayerOwnExiles` pool filters on `exiled_by`, which zone exit clears.
const THIS_TURN_EXILE_PLAY_SOURCE: &str =
    "You may play lands and cast spells from among cards exiled with this artifact this turn.";

/// CR 116.2a: a static exile-play permission must not reach a land that has LEFT
/// exile, even though the land is still in the permission's pool.
///
/// Widening the play-land capture to graveyards was right for object-attached
/// `PlayFromExile` grants, which travel with the card. It was wrong for the STATIC
/// fallback, and the reason is invisible from the capture site: a this-turn pool
/// (`cards_exiled_with_source_this_turn`) is keyed by `ObjectId`, which is stable
/// across zone changes, and is cleared only at turn end — never on zone exit. So a
/// land exiled with the source and then moved to a graveyard in the same turn is
/// still in the pool, and the widened capture let the action gate play it FROM THE
/// GRAVEYARD through a permission whose printed scope is cards *exiled* with the
/// source. Measured with the guard neutralized: discovery did not offer it, and
/// `PlayLand` returned `Ok` and moved it to the battlefield.
///
/// This is the fourth discovery/admission/completion zone disagreement on this
/// branch, and the first the reviewer's library-land probe could not reach,
/// because the land has to have been in exile first.
///
/// The exile is STAGED — link, this-turn pool entry and exiling player — the same
/// way `uba_mask_draw_to_exile_play.rs` stages its permission predicate, because
/// no printed card both exiles a land with this shape of source and is the source.
/// The two zone moves are the production `zones::move_to_zone` primitive, so the
/// zone-exit cleanup that matters here really runs.
///
/// POSITIVE REACH GUARDS: the land is playable while it is in exile, and it is
/// still a member of the source's pool after it moves. Without the second, the
/// negatives below would pass merely because the pool had been pruned.
#[test]
fn a_static_exile_permission_does_not_reach_a_land_that_left_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "This-Turn Exile Source", THIS_TURN_EXILE_PLAY_SOURCE)
        .id();
    let land = scenario.add_card_to_library_top(P0, "Staged Land");
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Exile, &mut events);
    {
        let state = runner.state_mut();
        state.exile_links.push(ExileLink {
            exiled_id: land,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
        state
            .cards_exiled_with_source_this_turn
            .entry(source)
            .or_default()
            .push(land);
        state.objects.get_mut(&land).unwrap().exiled_by = Some(P0);
    }
    assert!(
        can_play_land(&runner, land),
        "reach guard: the land must be playable through the static source while it \
         is in exile, or the negatives below prove nothing"
    );

    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Graveyard, &mut events);
    assert_eq!(
        zone_of(&runner, land),
        Zone::Graveyard,
        "reach guard: the land must actually have left exile"
    );
    assert!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&source)
            .is_some_and(|pool| pool.contains(&land)),
        "reach guard: the land must still be in the source's this-turn pool after \
         leaving exile — this is what makes the defect reachable at all"
    );

    assert!(
        !can_play_land(&runner, land),
        "the static permission covers cards in exile; it must not surface a land \
         that has moved to a graveyard"
    );
    let card_id = runner.state().objects[&land].card_id;
    assert!(
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .is_err(),
        "CR 116.2a: the action gate must refuse it — this is the half that was \
         broken; admitting it moves the land onto the battlefield from a zone it is \
         not in, through a permission that never offered it"
    );
    assert_eq!(
        zone_of(&runner, land),
        Zone::Graveyard,
        "the refused play must leave the land where it was"
    );
}

/// Verbatim Oracle text of Chiss-Goria, Forge Tyrant's attack trigger (Scryfall
/// `cards/named?exact=Chiss-Goria, Forge Tyrant`).
const CHISS_GORIA_TRIGGER: &str = "Whenever Chiss-Goria attacks, exile the top five cards of \
     your library. You may cast an artifact spell from among them this turn. If you do, it has \
     affinity for artifacts.";

/// A creature face from Oracle text, for the public coverage authority.
fn creature_face(name: &str, oracle: &str) -> engine::types::card::CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    engine::types::card::CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// CR 608.2c + CR 611.2f: a lingering cast grant with an "If you do, it …" rider is reported
/// as UNSUPPORTED, not silently counted as supported with an inert rider.
///
/// Chiss-Goria's grant is exercised at a later priority window, but its rider
/// ("If you do, it has affinity for artifacts") runs during the same resolution
/// as the grant, when no spell has been cast yet. Measured before this guard: the
/// grant installed correctly and the chosen artifact never gained affinity — no
/// keyword, no transient effect. The card still counted as supported. That is the
/// defect: a coverage claim larger than the card's behaviour.
///
/// ASSERTED THROUGH THE PUBLIC COVERAGE AUTHORITY (`card_face_gaps`), not by
/// string-matching the parse, because the claim under test is "the card reports
/// itself unsupported". A census count could pass while the verdict reads green.
///
/// DISCRIMINATING CONTROLS. Without them the refusal could pass because promotion
/// had stopped working:
///   * the SAME clause with the rider removed still promotes, and reports no gap
///     from this guard — it is the rider, not the grant, that is refused;
///   * Locke, Treasure Hunter (a lingering grant with no rider) still promotes.
#[test]
fn a_lingering_grant_with_an_if_you_do_rider_is_reported_unsupported() {
    let chiss = creature_face("Chiss-Goria, Forge Tyrant", CHISS_GORIA_TRIGGER);
    let gaps = engine::game::coverage::card_face_gaps(&chiss);
    assert!(
        gaps.iter()
            .any(|gap| gap.contains("cast_rider_on_lingering_grant")),
        "Chiss-Goria's inert affinity rider must surface as an explicit coverage gap, \
         got {gaps:?}"
    );
    assert!(
        engine::game::coverage::card_face_has_unimplemented_parts(&chiss),
        "the public verdict must read UNSUPPORTED"
    );
    let chiss_parse = parse_oracle_text(CHISS_GORIA_TRIGGER, "Chiss-Goria", &[], &[], &[]);
    assert!(
        !has_single_use_grant(&chiss_parse),
        "no grant may be installed whose rider would silently do nothing"
    );

    // Control 1: the identical clause without the rider still promotes.
    let no_rider = parse_oracle_text(
        "Whenever this creature attacks, exile the top five cards of your library. You may \
         cast an artifact spell from among them this turn.",
        "No Rider",
        &[],
        &[],
        &[],
    );
    assert!(
        has_single_use_grant(&no_rider),
        "control: without the rider there is nothing to lose, so the grant must still \
         promote — the guard fires on the rider, not on the grant"
    );

    // Control 2: Locke is a lingering grant with no rider and stays supported.
    let locke = creature_face("Locke, Treasure Hunter", LOCKE);
    assert!(
        !engine::game::coverage::card_face_gaps(&locke)
            .iter()
            .any(|gap| gap.contains("cast_rider_on_lingering_grant")),
        "control: Locke has no rider and must not be touched by this guard"
    );
    let locke_parse = parse_oracle_text(LOCKE, "Locke, Treasure Hunter", &[], &[], &[]);
    assert!(
        has_single_use_grant(&locke_parse),
        "control: Locke must still promote to the single-use grant"
    );
}

/// CR 116.2a + CR 305.1: a land milled from an OPPONENT's library is offered by
/// the land-play surface and the submitted action succeeds.
///
/// The cast fix's land companion had the same defect in the opposite direction.
/// Discovery (`graveyard_lands_playable_by_permission`) was widened to see
/// cross-owner grants, but the admission gate in `engine.rs` pre-checked
/// `player_data.graveyard.contains(&object_id)` — the ACTING player's own
/// graveyard — before consulting that authority. So the land appeared in legal
/// actions and was rejected when submitted: an offer the engine would not honor,
/// which is the mirror image of the cast bug where the engine would have honored
/// an action it never offered. The owner test was redundant with the lookup it
/// guarded, because that lookup already answers the permission question for every
/// graveyard.
///
/// BOTH HALVES ARE ASSERTED, and that pairing is the point — either one alone
/// passes while the two disagree.
///
/// Uses a `mode: Play` grant over a cross-owner mill, which is the general shape:
/// CR 701.17a puts each milled card into ITS OWNER'S graveyard, so a grant over
/// "those cards" necessarily spans graveyards as soon as more than one player
/// mills. Locke's own grant is `mode: Cast` and so can never reach the land path,
/// which is why the land half needs its own carrier.
#[test]
fn an_opponent_owned_milled_land_is_offered_and_playable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Cross-Owner Mill Source",
            1,
            1,
            "Whenever this creature attacks, each player mills a card. You may play \
             a land from among those cards this turn.",
        )
        .id();
    let their_land = scenario
        .add_spell_to_library_top(P1, "Their Milled Land", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .expect("the source must be able to attack");
    runner.advance_until_stack_empty();
    advance_past_combat(&mut runner);

    // Reach guards: the land really is in the OPPONENT's graveyard carrying a
    // live `mode: Play` grant. Without these the assertions below could pass over
    // an empty set or over a card the acting player already owns.
    assert_eq!(
        (
            zone_of(&runner, their_land),
            runner.state().objects[&their_land].owner
        ),
        (Zone::Graveyard, P1),
        "reach guard: the land under test must be opponent-OWNED and in the \
         opponent's graveyard"
    );

    // Half 1 — discovery offers it.
    assert!(
        engine::game::casting::graveyard_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(id, _)| *id == their_land),
        "CR 116.2a: a `mode: Play` grant naming this player authorizes the land \
         wherever it sits, so the land-play sweep must surface it"
    );

    // Half 2 — the production action gate honors what discovery offered.
    let card_id = runner.state().objects[&their_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: their_land,
            card_id,
        })
        .expect("an offered land play must be accepted by the action gate");
    assert_eq!(
        zone_of(&runner, their_land),
        Zone::Battlefield,
        "playing the opponent's milled land must actually move it to the battlefield"
    );
}

/// CR 601.2a: the OPPONENT's milled card is a member of the batch and must be
/// castable through the production pipeline, not merely carry the permission.
///
/// Locke prints "each player mills a card" and then "you may cast a spell from
/// among **those** cards". Nothing in CR 601.2a ties a granted cast permission to
/// the card's owner or to which graveyard the card sits in, so the opponent's
/// milled card is inside the printed permission. Before the discovery fix the
/// grant was installed on it correctly and never offered:
/// `graveyard_spell_objects_available_to_cast` scanned only the caster's own
/// graveyard and then skipped `obj.owner != player`, while the admission gate
/// `castable_from_current_zone` had no owner test at all — the two halves
/// disagreed, and the engine would have accepted a cast it never offered.
///
/// DISCRIMINATING, and the whole point of the test: it drives the cast through
/// `runner.cast(...)` off `legal_actions`, so it cannot pass by some other route
/// that happens to make the card castable. Reverting
/// `non_owner_graveyard_play_from_exile_grants` fails it at the reach guard with
/// the card still holding a valid grant.
#[test]
fn locke_casts_the_opponents_milled_card_through_the_production_pipeline() {
    let Milled {
        mut runner,
        mine,
        theirs,
        ..
    } = locke_attacks();
    advance_past_combat(&mut runner);

    // Reach guard: the opponent-owned card is genuinely in the OPPONENT's
    // graveyard, not somewhere the owner-scoped walk would have found anyway.
    assert_eq!(
        (
            zone_of(&runner, theirs),
            runner.state().objects[&theirs].owner
        ),
        (Zone::Graveyard, P1),
        "reach guard: the card under test must be an opponent-OWNED card in the \
         opponent's graveyard, or this proves nothing about the owner boundary"
    );
    assert!(
        can_cast(&runner, theirs),
        "CR 601.2a: a card milled from the opponent's library is a member of \
         \"those cards\" and must be offered — the permission names the player, \
         not the owner"
    );

    runner.cast(theirs).commit();
    // CR 601.2a: the card is on the stack as a spell — this is what proves the
    // production cast pipeline actually moved it, and it has to be checked BEFORE
    // resolution. CR 608.2n puts a resolved instant/sorcery into its owner's
    // graveyard, so the post-resolution zone is `Graveyard` again and asserting on
    // that would be indistinguishable from the card never having been cast.
    assert_eq!(
        zone_of(&runner, theirs),
        Zone::Stack,
        "the opponent's milled card must reach the stack when cast through the \
         production pipeline"
    );
    runner.advance_until_stack_empty();

    // CR 601.2a: the cap is grant-scoped, not owner-scoped. Spending it on the
    // opponent's card must close the controller's own card out too.
    assert!(
        !can_cast(&runner, mine) && single_use_grant_durations(&runner, mine).is_empty(),
        "spending the single-use grant on the opponent's milled card must strip \
         it from the controller's own milled card as well"
    );
}

/// NOT PROVEN HERE: that the cap holds across the OWNER boundary at the action
/// surface as well as in the permission state. That is
/// `locke_casts_the_opponents_milled_card_through_the_production_pipeline`.
#[test]
fn locke_authorizes_exactly_one_cast_from_the_milled_batch() {
    let Milled {
        mut runner,
        mine,
        theirs,
        ..
    } = locke_attacks();

    // CR 307.1: the seeded cards are SORCERIES, so the grant is only exercisable
    // at sorcery speed. Walking all the way to the postcombat main phase is also
    // the load-bearing half of "Until end of turn": the permission has to survive
    // the rest of the combat phase to be worth anything.
    advance_past_combat(&mut runner);
    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "reach guard: the test must actually reach a sorcery-speed window, or the \
         castability assertions below would be measuring the timing restriction"
    );

    assert!(
        can_cast(&runner, mine),
        "reach guard: the controller's milled card must be castable from the \
         graveyard through the grant before it is spent, or the cap assertion \
         below proves nothing"
    );
    assert_eq!(
        (
            single_use_grant_durations(&runner, mine).len(),
            single_use_grant_durations(&runner, theirs).len()
        ),
        (1, 1),
        "reach guard: both members of the milled batch must hold the grant before \
         it is spent"
    );

    runner.cast(mine).commit();
    runner.advance_until_stack_empty();

    assert!(
        single_use_grant_durations(&runner, theirs).is_empty(),
        "CR 601.2a: the grant printed a cap of ONE, so spending it must strip the \
         permission from every sibling in the batch — including the ones that are \
         not in exile"
    );
    assert!(
        !can_cast(&runner, theirs) && !can_cast(&runner, mine),
        "no member of a spent single-use batch may remain castable"
    );
}
