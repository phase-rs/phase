//! CR 508.1a + CR 608.2h + CR 113.7a + CR 111.7: "attacked this turn" must still be
//! answerable when the ability's SOURCE has ceased to exist.
//!
//! Oracle text (Taigam, Ojutai Master — 3/4, verbatim from card-data):
//!   "Instant, sorcery, and Dragon spells you control can't be countered.
//!    Whenever you cast an instant or sorcery spell from your hand, if Taigam attacked this
//!    turn, that spell gains rebound."
//!
//! Parsed condition (verbatim from card-data `parse_details`):
//!   TriggerCondition::SourceMatchesFilter {
//!       filter: Typed { type_filters: [Creature], properties: [AttackedThisTurn] } }
//!
//! THE DEFECT. `FilterProp::AttackedThisTurn` (and its `BlockedThisTurn` /
//! `AttackedOrBlockedThisTurn` siblings) sat in the explicit FAIL-CLOSED group of
//! `zone_change_record_matches_property` (filter.rs). A purged token source is absent from
//! `state.objects`, so `subject_filter_matches_with_lki` (trigger_matchers.rs) falls back to
//! the LKI snapshot, which synthesizes a `ZoneChangeRecord` and lands in exactly that group.
//! The CR 603.4 re-check at RESOLUTION therefore read FALSE, the trigger was silently removed
//! from the stack, and the spell never gained rebound.
//!
//! WHY FAIL-CLOSED WAS WRONG. The old rationale cited CR 400.7: "a permanent that changes
//! zones becomes a new object with no memory of its previous existence, so the zone-change
//! snapshot captures no attack history."
//!
//! That conflates two different objects. CR 400.7 governs the object that ARRIVES in the new
//! zone — and it stays true; nothing here gives the graveyard card an attack history of its
//! own. But the object this ability asks about is the one that ATTACKED, and CR 608.2h names
//! it precisely: "the effect uses the object's LAST KNOWN INFORMATION ... If an ability states
//! that an object does something, it's the object as it exists — OR AS IT MOST RECENTLY
//! EXISTED — that does it."
//!
//! And the game still has that record. `state.creatures_attacked_this_turn` is a
//! `HashSet<ObjectId>` written at attacker declaration (combat.rs) and cleared ONLY at turn
//! cleanup (turns.rs) — never on a zone change. It is keyed by the battlefield ObjectId, which
//! is exactly the id the LKI record carries. The ledger already outlives the source; the
//! filter simply refused to read it. Failing closed did not protect CR 400.7 — it declined to
//! answer a question the game had the answer to. (The `WasDealtDamageThisTurn` arm immediately
//! above in the same match already reads its turn-scoped ledger by `record.object_id` for
//! exactly this reason.)
//!
//! CONTRAST WITH THE ATTACHMENT HALF (sibling commit, Whiplash). There the underlying FACT is
//! destroyed on exit — SBA unattaches every Equipment the instant its host leaves (CR 704.5n) —
//! so BOTH a purged token and a merely-dead nontoken read wrong, and the repair had to restore
//! the fact from a snapshot. Here the fact SURVIVES in a live ledger, so a merely-dead NONTOKEN
//! source is already answered correctly by the live path (it stays in `state.objects` and the
//! ledger still holds its id). Only the leg where the subject cannot be SEEN at all — the
//! CR 111.7 purged token — was broken. That asymmetry is asserted below, in both directions.
//!
//! SEQUENCING. Taigam is the only observable carrier of this condition in the pool, and his
//! rebound grant was itself inert until PR #5803 (#125) taught the layer pass to reach STACK
//! objects. Landing this fix before that one would have been unwitnessable. This file builds on
//! the harness that PR established (`stack_object_keyword_grants.rs`).
//!
//! The observable is downstream and end-to-end: a spell that has rebound as it resolves is
//! EXILED instead of going to its owner's graveyard (CR 702.88a displacing CR 608.2n), and arms
//! exactly one next-upkeep recast (CR 603.7a).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{sba, zones};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Taigam, Ojutai Master — 3/4. VERBATIM Oracle text (verified against card-data.json).
const TAIGAM: &str = "Instant, sorcery, and Dragon spells you control can't be countered.\nWhenever you cast an instant or sorcery spell from your hand, if Taigam attacked this turn, that spell gains rebound.";

/// Only a token ceases to exist under CR 111.7 — this is the axis that separates a subject the
/// filter cannot SEE from one that is merely dead.
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Token,
    Nontoken,
}

/// Whether Taigam was declared as an attacker — the axis that turns the intervening-if on and
/// off.
#[derive(Clone, Copy, PartialEq)]
enum Attacked {
    Yes,
    No,
}

/// Whether Taigam dies with his trigger on the stack, or survives to its resolution.
/// `Survives` is the HARNESS POSITIVE CONTROL.
#[derive(Clone, Copy, PartialEq)]
enum SourceFate {
    Survives,
    Dies,
}

/// Kill `id` through the REAL pipeline: `move_to_zone` (which snapshots LKI) followed by the
/// REAL SBA (which purges a token under CR 111.7 / CR 704.5d).
fn kill(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), id, Zone::Graveyard, &mut events);
    sba::check_state_based_actions(runner.state_mut(), &mut events);
}

/// Put Taigam on the battlefield, optionally swing with him, cast an instant from hand,
/// optionally kill him while his trigger is on the stack, and resolve everything.
///
/// Returns the spell's final zone. Rebound EXILES the spell as it resolves (CR 702.88a);
/// without rebound it goes to the graveyard (CR 608.2n). That is the observable.
fn taigam_attacks_then_dies(kind: SourceKind, attacked: Attacked, fate: SourceFate) -> Zone {
    let mut scenario = GameScenario::new();
    // PLACE the scenario at PreCombatMain rather than advancing into it from turn 1: the draw
    // step would deck P0 against an empty library and end the game before combat.
    scenario.at_phase(Phase::PreCombatMain);

    let taigam = scenario
        .add_creature_from_oracle(P0, "Taigam, Ojutai Master", 3, 4, TAIGAM)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Instant", true, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();

    // CR 111.7 / CR 704.5d: only a token ceases to exist on leaving the battlefield.
    if kind == SourceKind::Token {
        runner
            .state_mut()
            .objects
            .get_mut(&taigam)
            .expect("the source must exist at setup")
            .is_token = true;
    }

    if attacked == Attacked::Yes {
        // CR 508.1a: declaring Taigam as an attacker is what writes his id into
        // `state.creatures_attacked_this_turn`.
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(taigam, AttackTarget::Player(P1))])
            .expect("Taigam must be able to attack");
        runner.combat_damage();
        runner.advance_to_phase(Phase::PostCombatMain);
    }

    // PREMISE GUARD: the ledger is the whole subject of this test. Assert it says what we think
    // it says BEFORE the source dies — otherwise a green below could mean "the ledger was
    // consulted" or "the ledger was empty and something else answered".
    assert_eq!(
        runner
            .state()
            .creatures_attacked_this_turn
            .contains(&taigam),
        attacked == Attacked::Yes,
        "PREMISE: `creatures_attacked_this_turn` must hold Taigam's id exactly when he attacked"
    );

    // CR 603.4 (first check): casting the instant is the trigger event.
    runner.cast(spell).commit();

    if fate == SourceFate::Dies {
        // Kill Taigam with his trigger ALREADY on the stack. CR 113.7a: the ability exists on
        // the stack independently of its source.
        kill(&mut runner, taigam);

        assert!(
            runner.state().lki_cache.contains_key(&taigam),
            "CR 400.7: battlefield exit must snapshot LKI for the source in both arms"
        );
        match kind {
            SourceKind::Token => assert!(
                !runner.state().objects.contains_key(&taigam),
                "CR 111.7: the token source must have CEASED TO EXIST — if it is still in \
                 state.objects this leg is vacuous and proves nothing about the LKI path"
            ),
            SourceKind::Nontoken => assert!(
                runner.state().objects.contains_key(&taigam),
                "a nontoken source stays in state.objects (in the graveyard)"
            ),
        }

        // THE LEDGER SURVIVED THE SOURCE. This is the fact the fix rests on, stated as an
        // assertion rather than a claim: the attack history is NOT cleared by the zone change.
        assert_eq!(
            runner
                .state()
                .creatures_attacked_this_turn
                .contains(&taigam),
            attacked == Attacked::Yes,
            "`creatures_attacked_this_turn` is cleared ONLY at turn cleanup (turns.rs), never on \
             a zone change — the attack history must still be there after the source is gone. \
             That is precisely why fail-closed was the wrong answer."
        );
    }

    runner.advance_until_stack_empty();
    runner.state().objects[&spell].zone
}

/// HARNESS POSITIVE CONTROL — green before and after.
///
/// A LIVING Taigam who attacked grants rebound, so the spell is exiled. Without this, a probe
/// whose spell could never be exiled — a broken cast, a trigger that never resolves — would
/// make every "defect" assertion below pass for entirely the wrong reason.
#[test]
fn premise_living_taigam_who_attacked_grants_rebound() {
    let zone = taigam_attacks_then_dies(SourceKind::Nontoken, Attacked::Yes, SourceFate::Survives);
    assert_eq!(
        zone,
        Zone::Exile,
        "CR 702.88a: a living Taigam who attacked MUST grant rebound, exiling the spell as it \
         resolves. If this is Graveyard the probe is broken and every assertion here is void."
    );
}

/// CR 603.4 FIRST-CHECK CONTROL — green before and after.
///
/// A Taigam who did NOT attack never triggers: the intervening-if is false when the spell is
/// cast. The spell resolves normally into the graveyard (CR 608.2n).
#[test]
fn living_taigam_who_did_not_attack_grants_nothing() {
    let zone = taigam_attacks_then_dies(SourceKind::Nontoken, Attacked::No, SourceFate::Survives);
    assert_eq!(
        zone,
        Zone::Graveyard,
        "CR 603.4: the ability triggers only if 'Taigam attacked this turn' is true at the \
         trigger event"
    );
}

/// PRIMARY WITNESS — RED before the fix (spell went to the Graveyard; must be Exile).
///
/// A token copy of Taigam attacks, then DIES in response to the spell his own trigger is
/// watching. CR 113.7a: the ability exists on the stack independently of its source. CR 603.4:
/// the intervening-if is re-checked at resolution. CR 608.2h: that re-check reads the source's
/// LAST KNOWN INFORMATION — and Taigam, as he most recently existed, attacked this turn. So the
/// spell still gains rebound and is exiled (CR 702.88a).
///
/// Before the fix, the purged token was invisible to the live filter, the LKI fallback
/// synthesized a `ZoneChangeRecord`, and `AttackedThisTurn` fail-closed to FALSE — even though
/// `state.creatures_attacked_this_turn` still held his id the whole time.
#[test]
fn purged_token_taigam_still_answers_attacked_this_turn_via_lki() {
    let zone = taigam_attacks_then_dies(SourceKind::Token, Attacked::Yes, SourceFate::Dies);
    assert_eq!(
        zone,
        Zone::Exile,
        "CR 608.2h + CR 508.1a: 'if Taigam attacked this turn' must be answered from the source \
         as it MOST RECENTLY EXISTED. He attacked; the turn-scoped ledger still holds his id; \
         the spell gains rebound and is EXILED as it resolves (CR 702.88a)."
    );
}

/// THE ASYMMETRY — green before AND after. This is a CONTROL, not a witness.
///
/// A NONTOKEN Taigam who dies the same way stays in `state.objects` (graveyard), so the LIVE
/// filter can still see the subject and read the surviving ledger by his id. This leg was
/// therefore ALREADY correct before the fix — unlike the attachment half of this class
/// (Whiplash), where SBA destroys the underlying fact on exit and BOTH legs were broken.
///
/// It is asserted here so that the difference between "the fact survives, the subject is
/// invisible" (this defect) and "the fact itself is destroyed" (the attachment defect) is
/// pinned rather than assumed.
#[test]
fn nontoken_dead_taigam_was_already_correct_via_the_live_path() {
    let zone = taigam_attacks_then_dies(SourceKind::Nontoken, Attacked::Yes, SourceFate::Dies);
    assert_eq!(
        zone,
        Zone::Exile,
        "a merely-dead nontoken source is still in state.objects, so the LIVE path reads the \
         surviving `creatures_attacked_this_turn` ledger by his id. This must hold both before \
         and after the fix."
    );
}

/// NEGATIVE CONTROL — a purged token that did NOT attack must still grant nothing.
///
/// The fix must restore the source's ability to ANSWER the question, not to always answer
/// "yes". A purged token Taigam who never attacked must not fabricate rebound.
#[test]
fn purged_token_taigam_that_did_not_attack_grants_nothing() {
    let zone = taigam_attacks_then_dies(SourceKind::Token, Attacked::No, SourceFate::Dies);
    assert_eq!(
        zone,
        Zone::Graveyard,
        "the LKI look-back must answer HONESTLY — a Taigam who never attacked grants no rebound, \
         purged or not"
    );
}

/// DISCRIMINATING CONTROL — the look-back is keyed by the SOURCE'S id, not by the board.
///
/// The runtime negative control above cannot reach the code under test: a Taigam who did not
/// attack never triggers at all (CR 603.4 first check), so the resolution-time re-check never
/// runs. This probe therefore interrogates the seam directly, with Taigam's OWN parsed filter,
/// and asks the question the runtime cannot:
///
///   A purged token Taigam who did NOT attack, while ANOTHER creature DID attack this turn.
///
/// The ledger is non-empty, so a fix that consulted it without keying on the record's id — say
/// `!state.creatures_attacked_this_turn.is_empty()` — would answer TRUE here and fabricate
/// rebound for a Taigam who never swung. It must answer FALSE.
///
/// Non-vacuity: the sibling leg runs the identical code path with TAIGAM as the attacker and
/// asserts TRUE. A predicate that always returned false would fail that one.
#[test]
fn purged_token_attack_lookback_is_keyed_by_the_source_not_the_board() {
    use engine::game::filter::{matches_target_filter_on_lki_snapshot, FilterContext};
    use engine::types::ability::TriggerCondition;

    for (taigam_attacks, expected) in [(false, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let taigam = scenario
            .add_creature_from_oracle(P0, "Taigam, Ojutai Master", 3, 4, TAIGAM)
            .id();
        // THE DECOY. A second creature that attacks in the arm where Taigam does not, so the
        // ledger is NON-EMPTY either way. Without it, a `!is_empty()` fix would pass.
        let decoy = scenario.add_creature(P0, "Decoy Attacker", 2, 2).id();
        let mut runner = scenario.build();

        // Take the filter from the CARD'S OWN PARSED TRIGGER, not a hand-rolled approximation —
        // this probe must interrogate the seam with exactly the predicate Taigam actually
        // carries, or it proves nothing about Taigam.
        let condition = runner
            .state()
            .objects
            .get(&taigam)
            .expect("taigam at setup")
            .trigger_definitions
            .first()
            .expect("PREMISE: Taigam must parse to exactly one triggered ability")
            .definition
            .condition
            .clone()
            .expect("PREMISE: that trigger must carry an intervening-if");
        let filter = match condition {
            TriggerCondition::SourceMatchesFilter { filter } => filter,
            other => panic!(
                "PREMISE: Taigam's intervening-if must be a SourceMatchesFilter — if the parse \
                 changed to {other:?}, this whole file is testing nothing"
            ),
        };

        let attacker = if taigam_attacks { taigam } else { decoy };
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("the chosen attacker must be able to attack");
        runner.combat_damage();
        runner.advance_to_phase(Phase::PostCombatMain);

        // The ledger is NON-EMPTY in BOTH arms — that is the point of the decoy.
        assert!(
            !runner.state().creatures_attacked_this_turn.is_empty(),
            "PREMISE: something must have attacked in both arms, or the decoy proves nothing"
        );

        runner
            .state_mut()
            .objects
            .get_mut(&taigam)
            .expect("taigam")
            .is_token = true;
        kill(&mut runner, taigam);
        assert!(
            !runner.state().objects.contains_key(&taigam),
            "CR 111.7: the token must have ceased to exist, or this probe is vacuous"
        );

        let state = runner.state();
        let lki = state
            .lki_cache
            .get(&taigam)
            .expect("CR 400.7: the purged token must still have an LKI snapshot");
        let ctx = FilterContext::from_source(state, taigam);

        assert_eq!(
            matches_target_filter_on_lki_snapshot(state, taigam, lki, &filter, &ctx),
            expected,
            "the attack look-back must be keyed by the RECORD'S OBJECT ID. With Taigam \
             attacking it must read TRUE; with only a decoy attacking it must read FALSE even \
             though `creatures_attacked_this_turn` is non-empty."
        );
    }
}
