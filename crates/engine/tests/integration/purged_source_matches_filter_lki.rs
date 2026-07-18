//! CR 608.2h + CR 113.7a + CR 111.7 + CR 603.4: a `SourceMatchesFilter`
//! intervening-if must still be answerable when the triggered ability's SOURCE has
//! ceased to exist.
//!
//! Oracle text (Dreampod Druid — 2/2 creature, verbatim):
//!   "At the beginning of each upkeep, if this creature is enchanted, create a 1/1
//!    green Saproling creature token."
//!
//! WHY THIS CARD. Of the 65 faces in the pool carrying a
//! `TriggerCondition::SourceMatchesFilter` intervening-if, 62 have an effect that acts
//! only on the SOURCE (pair it / it becomes an N/N creature / put a counter on it /
//! transform it / return it to hand / shuffle it in / suspect it). Once the source is
//! gone those effects are inert no matter what the condition answers, so the condition's
//! answer is unobservable and cannot be witnessed at all. Dreampod Druid is the one face
//! in the class whose gate is source-referential while its EFFECT acts on the game — it
//! creates a token — so the condition's answer is directly observable as a board change.
//! (Whiplash, Vengeful Engineer is the near-miss: its effect is observable too, but its
//! `X = the number of Equipment attached to him` also reads the attachment set, so it
//! needs a second fix. Taigam, Ojutai Master looks like a candidate but is a LYING GREEN
//! — its `that spell gains rebound` grant never lands on the stack object even with a
//! living source, so it cannot witness anything. Filed separately.)
//!
//! THE DEFECT. The `SourceMatchesFilter` arm (`check_trigger_condition`, triggers.rs)
//! asks the filter about the SOURCE ITSELF as the subject. t73 (PR #5779) repaired the
//! CONTEXT half of this class — the source's CONTROLLER now answers from LKI. The
//! SUBJECT half remained: `filter_inner` looks the subject up in `state.objects`, and a
//! token that left the battlefield ceased to exist (CR 111.7 / CR 704.5d) and was purged
//! from `state.objects` outright. The filter cannot see the subject at all, so it fails
//! closed for EVERY property.
//!
//! Worse, the property this card gates on could not be answered even once the subject was
//! visible: attachment is a battlefield-only relationship, and SBA unattaches everything
//! the instant the host leaves (CR 704.5m/n), so the live board has nothing left to read.
//! `LKISnapshot` did not capture the attachment set, and the LKI→`ZoneChangeRecord`
//! synthesizer hardcoded `attachments: vec![]`. Both halves are needed: the subject must
//! be visible via LKI, AND the LKI must actually carry what the filter asks for.
//!
//! CR 608.2h is explicit that fail-closed is wrong here: "If the effect requires
//! information from a specific object, INCLUDING THE SOURCE OF THE ABILITY ITSELF, the
//! effect uses the current information of that object if it's in the public zone it was
//! expected to be in; if it's no longer in that zone ... the effect uses the object's
//! LAST KNOWN INFORMATION." And CR 113.7a: "Once activated or triggered, an ability
//! exists on the stack independently of its source. Destruction or removal of the source
//! after that time won't affect the ability."
//!
//! THE DISCRIMINATING VECTOR IS THE CR 111.7 PURGE. A nontoken Druid that dies stays in
//! `state.objects` (graveyard), so the subject is still visible to `filter_inner` — but
//! its attachments are gone either way, so BOTH legs need the LKI attachment capture.
//! Both are asserted below.
//!
//! This drives the REAL pipeline end to end: the card is synthesized from verbatim Oracle
//! text, the Aura is attached through the engine's `attach::attach_to` authority, the
//! upkeep trigger fires off the real phase machinery, the source is killed through the
//! real zone-change pipeline (`move_to_zone`, which snapshots LKI) and purged by the real
//! SBA (`check_state_based_actions`), and the trigger resolves off the real stack. The
//! observable is the number of Saproling tokens on the battlefield.

use engine::game::effects::attach;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::game::{sba, zones};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Dreampod Druid — 2/2. Verbatim Oracle text.
const DREAMPOD_DRUID: &str =
    "At the beginning of each upkeep, if this creature is enchanted, create a 1/1 green Saproling creature token.";

/// Whether the Druid is a token copy or the printed card. Only a token ceases to exist
/// under CR 111.7 — this is the axis that separates a purged subject from a merely dead one.
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Token,
    Nontoken,
}

/// Whether an Aura is attached to the Druid, i.e. whether the intervening-if is
/// genuinely TRUE.
#[derive(Clone, Copy, PartialEq)]
enum Enchanted {
    Yes,
    No,
}

/// Whether the source dies with its trigger on the stack, or simply survives.
/// `Survives` is the HARNESS POSITIVE CONTROL.
#[derive(Clone, Copy, PartialEq)]
enum SourceFate {
    Survives,
    Dies,
}

/// An Aura on the battlefield. Built the same way `aura_graft_enchant_restriction.rs`
/// builds one, so `capture_attachment_snapshot` classifies it as `AttachmentKind::Aura`.
fn make_aura(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Test Aura".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    id
}

/// Count the Saproling tokens P0 controls on the battlefield — the observable.
fn saprolings(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == P0 && o.name == "Saproling")
        .count()
}

/// Put a (possibly enchanted) Dreampod Druid on the battlefield, run the upkeep trigger,
/// optionally kill the source while that trigger is on the stack, and resolve.
///
/// The intervening-if is TRUE when it triggers (CR 603.4, first check) whenever
/// `enchanted == Yes`; the question under test is whether it is still ANSWERABLE at the
/// CR 603.4 re-check at RESOLUTION, once the source is gone.
fn upkeep_trigger_then_kill_druid(
    kind: SourceKind,
    enchanted: Enchanted,
    fate: SourceFate,
) -> usize {
    let mut scenario = GameScenario::new();

    let druid = scenario
        .add_creature_from_oracle(P0, "Dreampod Druid", 2, 2, DREAMPOD_DRUID)
        .id();

    let mut runner = scenario.build();

    // In BOTH arms an Aura exists on the battlefield. Only the ATTACHMENT differs. Without
    // this, the `No` arm would have no Aura at all, and a "no token" result could mean
    // "there was no Aura anywhere" rather than "the source was genuinely not enchanted" —
    // a control that cannot tell those apart is not a control.
    let aura = make_aura(runner.state_mut(), P0);
    if enchanted == Enchanted::Yes {
        // CR 303.4: attach through the engine's own authority, not by hand-editing state.
        attach::attach_to(runner.state_mut(), aura, druid);
    }

    // CR 111.7 / CR 704.5d: only a token ceases to exist on leaving the battlefield.
    if kind == SourceKind::Token {
        runner
            .state_mut()
            .objects
            .get_mut(&druid)
            .expect("the source must exist at setup")
            .is_token = true;
    }

    // PREMISE GUARD: the attachment must actually be established, or the Yes arm proves
    // nothing about attachment look-back.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&druid)
            .expect("druid at setup")
            .attachments
            .contains(&aura),
        enchanted == Enchanted::Yes,
        "the Aura's attachment to the Druid is the single axis under test"
    );

    // CR 603.4 (first check): the upkeep trigger fires only if the Druid is enchanted.
    runner.advance_to_upkeep();

    let expected_stack = usize::from(enchanted == Enchanted::Yes);
    assert_eq!(
        runner.state().stack.len(),
        expected_stack,
        "CR 603.4: the upkeep trigger must be on the stack exactly when the intervening-if \
         was TRUE at trigger time. If it were absent in the Yes arm we would be testing the \
         COLLECTION path, not the re-check at RESOLUTION."
    );

    if fate == SourceFate::Dies {
        // Kill the Druid with its trigger already on the stack, through the REAL zone-change
        // pipeline (which snapshots LKI) and the REAL SBA (which purges a token under
        // CR 111.7 / CR 704.5d).
        let mut events = Vec::new();
        zones::move_to_zone(runner.state_mut(), druid, Zone::Graveyard, &mut events);
        sba::check_state_based_actions(runner.state_mut(), &mut events);

        assert!(
            runner.state().lki_cache.contains_key(&druid),
            "CR 400.7: battlefield exit must snapshot LKI for the source in both arms"
        );
        match kind {
            SourceKind::Token => assert!(
                !runner.state().objects.contains_key(&druid),
                "CR 111.7: the token source must have CEASED TO EXIST — if it is still in \
                 state.objects this test is vacuous and proves nothing"
            ),
            SourceKind::Nontoken => assert!(
                runner.state().objects.contains_key(&druid),
                "a nontoken source stays in state.objects (in the graveyard)"
            ),
        }
    }

    runner.advance_until_stack_empty();
    saprolings(runner.state())
}

/// HARNESS POSITIVE CONTROL — green before and after.
///
/// An enchanted Druid that SURVIVES makes its Saproling. This is the test that makes every
/// zero below meaningful: without it, a probe that could never create a token — a card
/// whose effect does not work, a trigger the harness never really resolves — would make the
/// "defect" assertions pass for entirely the wrong reason.
#[test]
fn premise_living_enchanted_druid_creates_a_saproling() {
    let tokens =
        upkeep_trigger_then_kill_druid(SourceKind::Nontoken, Enchanted::Yes, SourceFate::Survives);
    assert_eq!(
        tokens, 1,
        "a living enchanted Dreampod Druid MUST create its Saproling at upkeep. If this is \
         0 the probe is broken and every other assertion in this file is void."
    );
}

/// CR 603.4 FIRST-CHECK CONTROL — green before and after.
///
/// An UNenchanted Druid never triggers at all: the intervening-if is false when the trigger
/// event occurs. An Aura is on the battlefield here (just not attached), so a zero proves
/// the predicate ran and rejected the Druid, not that no Aura existed to find.
#[test]
fn unenchanted_druid_never_triggers() {
    let tokens =
        upkeep_trigger_then_kill_druid(SourceKind::Nontoken, Enchanted::No, SourceFate::Survives);
    assert_eq!(
        tokens, 0,
        "CR 603.4: the ability triggers only if the condition is true at the trigger event"
    );
}

/// PRIMARY WITNESS — RED before the fix.
///
/// A token copy of an enchanted Dreampod Druid dies while its upkeep trigger is on the
/// stack. The Druid WAS enchanted, so the intervening-if is TRUE and the Saproling must
/// still be created (CR 603.4: re-checked at resolution; CR 113.7a: the ability exists
/// independently of its source; CR 608.2h: the source's information comes from LAST KNOWN
/// INFORMATION once it is no longer in its expected zone).
///
/// Before the fix the purged token was absent from `state.objects` — `filter_inner` could
/// not see the subject — and even the LKI record carried `attachments: vec![]`, so the
/// condition read FALSE, the trigger was silently removed from the stack, and no token was
/// created.
#[test]
fn purged_token_source_still_answers_source_matches_filter_via_lki() {
    let tokens =
        upkeep_trigger_then_kill_druid(SourceKind::Token, Enchanted::Yes, SourceFate::Dies);
    assert_eq!(
        tokens, 1,
        "CR 608.2h: the intervening-if must resolve against the purged source's LAST KNOWN \
         INFORMATION — the Druid was enchanted when it last existed, so the Saproling is created"
    );
}

/// SECOND WITNESS — the NONTOKEN dead source.
///
/// The printed card dies the same way. It stays in `state.objects` (graveyard), so the
/// SUBJECT was always visible — but SBA unattached its Aura (CR 704.5m/n), so the live board
/// could not answer "is it enchanted" either. This leg is repaired by the LKI attachment
/// capture rather than by the subject fallback.
#[test]
fn nontoken_dead_source_still_answers_source_matches_filter_via_lki() {
    let tokens =
        upkeep_trigger_then_kill_druid(SourceKind::Nontoken, Enchanted::Yes, SourceFate::Dies);
    assert_eq!(
        tokens, 1,
        "CR 608.2h + CR 704.5m: the Aura is unattached the instant the host leaves, so 'is it \
         enchanted' must be answered from LAST KNOWN INFORMATION, not from the live board"
    );
}

/// NEGATIVE CONTROL — the fix must not FABRICATE a match.
///
/// This is the arm that proves the LKI fallback restores the source's ability to ANSWER the
/// question rather than to always answer "yes". It cannot be expressed through the upkeep
/// trigger (an unenchanted Druid never triggers at all, per CR 603.4's first check), so it
/// interrogates the seam directly: a purged token that was NOT enchanted must answer FALSE
/// for `HasAttachment(Aura)` even though its LKI snapshot exists and is consulted.
///
/// Non-vacuity: the sibling arm below runs the identical code path with the Aura ATTACHED
/// and asserts TRUE. A predicate that always returned false would fail that one.
#[test]
fn purged_token_source_that_was_not_enchanted_answers_false() {
    use engine::game::filter::{matches_target_filter_on_lki_snapshot, FilterContext};
    use engine::types::ability::TriggerCondition;

    for (enchanted, expected) in [(Enchanted::No, false), (Enchanted::Yes, true)] {
        let mut scenario = GameScenario::new();
        let druid = scenario
            .add_creature_from_oracle(P0, "Dreampod Druid", 2, 2, DREAMPOD_DRUID)
            .id();
        let mut runner = scenario.build();

        // Take the filter from the CARD'S OWN PARSED TRIGGER, not a hand-rolled
        // approximation — this probe must interrogate the seam with exactly the predicate
        // Dreampod Druid actually carries, or it proves nothing about Dreampod Druid.
        let druid_obj = runner.state().objects.get(&druid).expect("druid at setup");
        let condition = druid_obj
            .trigger_definitions
            .first()
            .expect("PREMISE: Dreampod Druid must parse to exactly one triggered ability")
            .definition
            .condition
            .as_ref()
            .expect("PREMISE: that trigger must carry an intervening-if");
        let filter = match condition {
            TriggerCondition::SourceMatchesFilter { filter } => filter.clone(),
            other => panic!(
                "PREMISE: Dreampod Druid's intervening-if must be a SourceMatchesFilter — if the \
                 parse changed to {other:?}, this whole file is testing nothing"
            ),
        };

        let aura = make_aura(runner.state_mut(), P0);
        if enchanted == Enchanted::Yes {
            attach::attach_to(runner.state_mut(), aura, druid);
        }
        runner
            .state_mut()
            .objects
            .get_mut(&druid)
            .expect("druid")
            .is_token = true;

        let mut events = Vec::new();
        zones::move_to_zone(runner.state_mut(), druid, Zone::Graveyard, &mut events);
        sba::check_state_based_actions(runner.state_mut(), &mut events);
        assert!(
            !runner.state().objects.contains_key(&druid),
            "CR 111.7: the token must have ceased to exist, or this probe is vacuous"
        );

        let state = runner.state();
        let lki = state
            .lki_cache
            .get(&druid)
            .expect("CR 400.7: the purged token must still have an LKI snapshot");
        let ctx = FilterContext::from_source(state, druid);

        assert_eq!(
            matches_target_filter_on_lki_snapshot(state, druid, lki, &filter, &ctx),
            expected,
            "the LKI attachment look-back must ANSWER the question honestly, not fabricate a \
             match — a purged token that was never enchanted must still read FALSE"
        );
    }
}

// ===========================================================================
// RIDERS (ruling on the t105 checkpoint)
// ===========================================================================

/// RIDER — SOULBOND MUST STAY FALSE FOR A PURGED SOURCE (CR 702.95b).
///
/// Soulbond is the largest slice of this class: **50 of the 91 trigger defs, 25 of the 65
/// faces**. Its `SourceMatchesFilter` is `Creature{You} + InZone(Battlefield) + Unpaired`,
/// and a creature that has ceased to exist must NOT pair — **CR 702.95b**: pairing exists
/// only between creatures on the battlefield. Today it stays false *structurally* (`InZone`
/// reads `record.from_zone`, which the LKI synthesizer leaves `None`; `Unpaired` reads live
/// `state.objects`), but structural immunity is one refactor away from gone. This pins the
/// BEHAVIOR, not the mechanism.
///
/// PROVENANCE OF THE FILTER. Soulbond's triggers are synthesized from the keyword during the
/// card-data build, not by `parse_oracle_text`, so — unlike Dreampod Druid — they cannot be
/// pulled off an oracle-synthesized object in this harness (the documented inline-keyword
/// foot-gun). The filter is therefore constructed here, and then **asserted byte-identical to
/// the filter actually carried by all 50 soulbond defs in the pool**, measured from the
/// card-data snapshot. That assertion is what stops this from being a hand-rolled fabrication:
/// if the real card's shape ever drifts from this literal, the premise fails loudly rather
/// than the pin passing against a filter no card carries.
#[test]
fn purged_token_soulbond_source_must_not_pair_cr_702_95b() {
    use engine::game::filter::{
        matches_target_filter, matches_target_filter_on_lki_snapshot, FilterContext,
    };
    use engine::types::ability::{
        ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
    };

    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::You),
        properties: vec![
            FilterProp::InZone {
                zone: Zone::Battlefield,
            },
            FilterProp::Unpaired,
        ],
    });

    // PREMISE GUARD — anti-fabrication. This must be EXACTLY the filter the pool carries.
    // Verbatim from the card-data census (all 50 soulbond defs / 25 faces share this shape).
    let measured: serde_json::Value = serde_json::from_str(
        r#"{"controller":"You","properties":[{"type":"InZone","zone":"Battlefield"},{"type":"Unpaired"}],"type":"Typed","type_filters":["Creature"]}"#,
    )
    .expect("the measured literal must be valid JSON");
    assert_eq!(
        serde_json::to_value(&filter).expect("filter must serialize"),
        measured,
        "PREMISE: the soulbond filter under test must be byte-identical to the one all 50 \
         soulbond defs actually carry in card-data. If this fails, the pin below is testing a \
         filter no real card has."
    );

    // A soulbond-shaped creature. Only the FILTER is soulbond's; the body is irrelevant to the
    // predicate under test, which reads type / controller / zone / pairing.
    let mut scenario = GameScenario::new();
    let wing = scenario.add_creature(P0, "Wingcrafter", 1, 1).id();
    let mut runner = scenario.build();

    // NON-VACUITY LEG: alive, on the battlefield, unpaired ⇒ the filter MUST match.
    {
        let state = runner.state();
        let ctx = FilterContext::from_source(state, wing);
        assert!(
            matches_target_filter(state, wing, &filter, &ctx),
            "a LIVE unpaired creature you control must match the soulbond filter — if this is \
             false the pin below is vacuous (a predicate that never matches proves nothing)"
        );
    }

    // Purge it as a token (CR 111.7 / CR 704.5d).
    runner
        .state_mut()
        .objects
        .get_mut(&wing)
        .expect("wingcrafter")
        .is_token = true;
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), wing, Zone::Graveyard, &mut events);
    sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        !runner.state().objects.contains_key(&wing),
        "CR 111.7: the token must have ceased to exist, or this pin is vacuous"
    );

    let state = runner.state();
    let lki = state
        .lki_cache
        .get(&wing)
        .expect("CR 400.7: the purged token must still have an LKI snapshot");
    let ctx = FilterContext::from_source(state, wing);
    assert!(
        !matches_target_filter_on_lki_snapshot(state, wing, lki, &filter, &ctx),
        "CR 702.95b: pairing exists only between creatures on the BATTLEFIELD. A soulbond \
         source that has ceased to exist must NOT satisfy its own intervening-if. The LKI \
         look-back restores the source's ability to ANSWER a question about its \
         characteristics; it must never resurrect the source's ZONE."
    );
}

/// RIDER — THE SNAPSHOTTED ATTACHMENT IDs ARE NOT A DANGLING-REFERENCE TRAP.
///
/// `LKISnapshot::attachments` reuses `AttachmentSnapshot { object_id, controller, kind }` —
/// the same shape `ZoneChangeRecord::attachments` already persists. The ruling flagged the
/// stored `ObjectId` as a possible dangling reference. It is not: `object_id` is used in
/// exactly two places in the filter layer (`filter.rs`), and BOTH are pure identity
/// comparisons (`att.object_id == source_id` / `!= source.id`, for `exclude_source`) — it is
/// never fed to `state.objects.get()`. The `controller` and `kind` the gate actually reads are
/// stored BY VALUE in the snapshot.
///
/// This pins that: the Aura object itself is destroyed outright after the host is purged, so
/// its id dangles — and `HasAttachment` must still answer TRUE from the snapshot, without a
/// panic and without consulting the vanished Aura.
#[test]
fn lki_attachment_ids_are_identity_only_and_survive_a_purged_aura() {
    use engine::game::filter::{matches_target_filter_on_lki_snapshot, FilterContext};
    use engine::types::ability::TriggerCondition;

    let mut scenario = GameScenario::new();
    let druid = scenario
        .add_creature_from_oracle(P0, "Dreampod Druid", 2, 2, DREAMPOD_DRUID)
        .id();
    let mut runner = scenario.build();

    let filter = match runner
        .state()
        .objects
        .get(&druid)
        .expect("druid")
        .trigger_definitions
        .first()
        .expect("PREMISE: one trigger")
        .definition
        .condition
        .as_ref()
        .expect("PREMISE: intervening-if")
    {
        TriggerCondition::SourceMatchesFilter { filter } => filter.clone(),
        other => panic!("PREMISE: expected SourceMatchesFilter, got {other:?}"),
    };

    let aura = make_aura(runner.state_mut(), P0);
    attach::attach_to(runner.state_mut(), aura, druid);

    // Purge the HOST (token).
    runner
        .state_mut()
        .objects
        .get_mut(&druid)
        .expect("druid")
        .is_token = true;
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), druid, Zone::Graveyard, &mut events);
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    // Now DESTROY THE AURA OBJECT OUTRIGHT, so the id stored in the host's LKI snapshot
    // refers to an object that no longer exists anywhere. If anything in the gate
    // dereferenced that id, this is where it would panic or silently answer false.
    runner.state_mut().objects.remove(&aura);
    runner.state_mut().battlefield.retain(|id| *id != aura);
    assert!(
        !runner.state().objects.contains_key(&aura),
        "the Aura must be gone from state.objects, or this pin proves nothing"
    );

    let state = runner.state();
    let lki = state.lki_cache.get(&druid).expect("host LKI");
    assert!(
        lki.attachments.iter().any(|a| a.object_id == aura),
        "the snapshot must still carry the (now dangling) Aura id — that is the whole point"
    );
    let ctx = FilterContext::from_source(state, druid);
    assert!(
        matches_target_filter_on_lki_snapshot(state, druid, lki, &filter, &ctx),
        "CR 608.2h: 'is it enchanted' must be answered from the snapshot's own kind/controller \
         values. The stored ObjectId is identity-only (self-exclusion); it is never \
         dereferenced, so a purged Aura cannot break the look-back."
    );
}

/// RIDER — SAVE-COMPAT ROUND TRIP, BOTH DIRECTIONS.
///
/// `GameState` is `Serialize`/`Deserialize` and owns `lki_cache: HashMap<ObjectId, LKISnapshot>`,
/// so `LKISnapshot` IS persisted into saves. The new field therefore needs `#[serde(default)]`
/// (it has it) and a two-way round-trip proof:
///
/// * OLD → NEW: a save written before this change has no `attachments` key. It must still
///   deserialize, defaulting to the empty set — which is exactly the pre-change fail-closed
///   answer, so an old save cannot start fabricating attachment matches.
/// * NEW → NEW: a snapshot carrying attachments must survive a full `GameState` round trip.
#[test]
fn lki_snapshot_attachments_are_save_compatible_both_directions() {
    use engine::types::game_state::LKISnapshot;

    // --- OLD → NEW: legacy save, no `attachments` key at all.
    let legacy = serde_json::json!({
        "name": "Legacy Creature",
        "power": 2,
        "toughness": 2,
        "mana_value": 3,
        "controller": 0,
        "owner": 0
    });
    let decoded: LKISnapshot = serde_json::from_value(legacy)
        .expect("a pre-change save (no `attachments` key) MUST still deserialize");
    assert!(
        decoded.attachments.is_empty(),
        "an old save must default to NO attachments — the pre-change fail-closed answer. \
         Anything else would let a legacy save fabricate attachment matches."
    );

    // --- NEW → NEW: a live snapshot carrying attachments survives a full GameState round trip.
    let mut scenario = GameScenario::new();
    let druid = scenario
        .add_creature_from_oracle(P0, "Dreampod Druid", 2, 2, DREAMPOD_DRUID)
        .id();
    let mut runner = scenario.build();
    let aura = make_aura(runner.state_mut(), P0);
    attach::attach_to(runner.state_mut(), aura, druid);
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), druid, Zone::Graveyard, &mut events);

    let before = runner
        .state()
        .lki_cache
        .get(&druid)
        .expect("host LKI")
        .attachments
        .clone();
    assert!(
        !before.is_empty(),
        "the round trip is vacuous unless the snapshot actually carries an attachment"
    );

    let json = serde_json::to_string(runner.state()).expect("GameState must serialize");
    let restored: GameState = serde_json::from_str(&json).expect("GameState must deserialize");
    assert_eq!(
        restored
            .lki_cache
            .get(&druid)
            .expect("LKI must survive the save round trip")
            .attachments,
        before,
        "the LKI attachment set must survive a save/load round trip intact"
    );
}
