//! CR733 P2 coverage for copy-token births (CR 707.2).
//!
//! Copy births share `ResolvedTokenCreationCommand` with ordinary CR 111.1
//! births: the journaled axis is object creation for both — an object came into
//! existence and its id and timestamp were drawn — and CR 707.2 governs only how
//! the body was derived upstream of that seam. So the body is parameterized
//! (`ResolvedTokenBody::{Spec, Copy}`) rather than forked into a sibling family.
//!
//! Like the ordinary family this applier MATERIALIZES its subject, so its
//! precondition is inverted: the recorded id must be ABSENT and re-application
//! must fail closed rather than silently duplicating the token.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedCopyBodyModifications, ResolvedRulesCommand, ResolvedTokenBody,
    ResolvedTokenCreationReplayInvariantError,
};
use engine::types::zones::Zone;

/// Replicate (Sorcery) — verbatim Oracle text. A plain copy with no CR 707.9
/// exceptions, so it takes the liminal seam and its body is complete at entry.
const REPLICATE_ORACLE: &str = "Create a token that's a copy of target creature you control.";

/// Citanul Woodreaders — verbatim Scryfall Oracle text. Its Kicker keyword and
/// its kicked-gated entry trigger are the live-only mutation this fixture
/// measures. CR 707.2: a copy acquires cast-time choices ("whether it was
/// kicked") only "for an object on the stack" — a token copy of a permanent is
/// not one, so it never acquires the kicked-ness. CR 702.33a: the Kicker
/// keyword itself IS acquired with the rules text, but it "functions while the
/// spell with kicker is on the stack", so it is inert on a permanent — which is
/// why the seam strips it rather than leaving it. CR 603.4: the entry trigger's
/// intervening "if it was kicked" is therefore checked against a condition that
/// can never hold.
const CITANUL_WOODREADERS_ORACLE: &str =
    "Kicker {2}{G} (You may pay an additional {2}{G} as you cast this spell.)\n\
     When this creature enters, if it was kicked, draw two cards.";

/// CR 707.2 + CR 603.4: the copy seam's post-birth finalize
/// (`finalize_copied_token`) strips spell-casting-only keywords and
/// cast-payment-gated triggers off the token. That runs AFTER the birth is
/// journaled, and the journaled `CopyTokenSpec` still carries the unstripped
/// copiable values — so a replay that only materializes the body installs a
/// token holding a Kicker keyword and an "if it was kicked" trigger the live
/// token does not have.
///
/// This is the Copy-body half of the injection dispatch in
/// `apply_resolved_token_creation`. It is a separate arm from the ordinary
/// spec-body one on purpose: the Copy arm must NOT call
/// `inject_resolved_token_abilities` (that would grant catalog text the copy is
/// not entitled to), so the two arms cannot be collapsed and each needs its own
/// production-path test.
///
/// REVERT-PROBE (discriminating, RUN): delete the `ResolvedTokenBody::Copy` arm
/// of that dispatch ⇒ the replayed token keeps `Keyword::Kicker` and the
/// cast-gated trigger, and both assertions below fail.
#[test]
fn copy_token_birth_replays_the_cast_only_strip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let original = scenario
        .add_creature_from_oracle(P0, "Citanul Woodreaders", 1, 4, CITANUL_WOODREADERS_ORACLE)
        .id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Replicate", true, REPLICATE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // Reach guard on the SOURCE: the fixture only measures a strip if there is
    // something to strip. A parser change that stopped producing either of these
    // would otherwise turn the parity assertions below into empty == empty.
    let source = &runner.state().objects[&original];
    assert!(
        source
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Kicker(_))),
        "reach guard: the copy source must carry a spell-casting-only keyword, got {:?}",
        source.keywords
    );
    assert!(
        !source.trigger_definitions.is_empty(),
        "reach guard: the copy source must carry the kicked-gated entry trigger"
    );

    let committed = runner.cast(spell_id).target_object(original).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 707.2: the resolved effect must create a copy token");
    let live = &state.objects[&token_id];
    assert_eq!(
        live.name, "Citanul Woodreaders",
        "CR 707.2 reach guard: the copy acquired the source's copiable name"
    );
    // CR 707.2 reach guard: the live strip actually happened, so "replayed ==
    // live" below is a real constraint rather than a tautology on two
    // identically-unstripped objects.
    assert!(
        !live
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Kicker(_))),
        "CR 707.2 + CR 702.33a: the live copy token must not keep Kicker, got {:?}",
        live.keywords
    );

    let birth = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .find_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("the copy birth is journaled");
    // The journaled body is the UNSTRIPPED one — this is what makes the replay
    // arm load-bearing rather than a redundant re-application.
    let recorded_keywords = match &birth.body {
        ResolvedTokenBody::Copy { copy, .. } => copy.values.keywords.clone(),
        ResolvedTokenBody::Spec { .. } => panic!("a copy token must journal a Copy body"),
    };
    assert!(
        recorded_keywords
            .iter()
            .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Kicker(_))),
        "the recorded copiable values predate the CR 707.2 strip, got {recorded_keywords:?}"
    );

    let mut replay = pre_state;
    engine::game::effects::token::apply_resolved_token_creation(&mut replay, &birth)
        .expect("the recorded copy birth must replay against its captured predecessor");
    let replayed = &replay.objects[&token_id];

    // THE DISCRIMINATORS. Body-only materialization keeps both.
    assert_eq!(
        replayed.keywords, live.keywords,
        "CR 707.2 + CR 702.33a: replay must apply the same cast-only keyword strip \
         the live copy seam did"
    );
    assert_eq!(
        replayed.trigger_definitions.len(),
        live.trigger_definitions.len(),
        "CR 603.4: replay must strip the same cast-payment-gated trigger — live {:?} vs \
         replayed {:?}",
        live.trigger_definitions,
        replayed.trigger_definitions
    );
}

#[test]
fn copy_token_birth_journals_and_replays_exactly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A distinctive body so the reach guard can prove the COPY actually copied,
    // rather than that some token merely appeared.
    let original = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Replicate", true, REPLICATE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let committed = runner.cast(spell_id).target_object(original).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 707.2 reach guard: a copy token actually came into existence AND
    // carries the copied characteristics. Without this the journal assertion
    // below could pass vacuously on a run that never minted a copy.
    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 707.2: the resolved effect must create a copy token");
    let token = &state.objects[&token_id];
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(
        token.name, "Grizzly Bears",
        "CR 707.2: the copy acquires the copiable name of the original"
    );
    assert_eq!(
        (token.power, token.toughness),
        (Some(2), Some(2)),
        "CR 707.2: the copy acquires the original's copiable power and toughness"
    );

    // The discriminating assertion: the copy birth is journaled as an exact
    // resolved command. Before this family the copy seams recorded nothing.
    let births: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        births.len(),
        1,
        "the copy-token authority must journal exactly one resolved creation"
    );

    let birth = &births[0];
    // Body-shape guard: this fixture must reach the COPY arm, not the ordinary
    // spec arm, or the test would be exercising the already-covered family.
    match &birth.body {
        ResolvedTokenBody::Copy {
            copy,
            modifications,
        } => {
            assert_eq!(
                copy.values.name, "Grizzly Bears",
                "the recorded body carries the copiable values that were copied"
            );
            assert_eq!(
                *modifications,
                ResolvedCopyBodyModifications::NoExceptions,
                "Replicate has no CR 707.9 exceptions"
            );
        }
        ResolvedTokenBody::Spec { .. } => {
            panic!("a copy token must journal a Copy body, not an ordinary Spec body")
        }
    }
    assert_eq!(
        birth.entry_timestamp, token.timestamp,
        "CR 613.7d: the journaled timestamp is the one the copy received"
    );
    assert!(
        birth.resulting_next_object_id > token_id.0,
        "the recorded high-water is above the id it allocated"
    );

    // Replay-exactness: applying the recorded command to the pre-resolution
    // state materializes the SAME object at the SAME id with the SAME timestamp,
    // with no re-draw from `next_object_id` or `next_timestamp`.
    let mut replay = pre_state;
    assert!(
        !replay.objects.contains_key(&token_id),
        "the copy does not exist before the command is applied"
    );
    engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth)
        .expect("the recorded copy birth must replay against its captured predecessor");

    let replayed = &replay.objects[&token_id];
    assert!(replayed.is_token, "replay materializes a token");
    assert_eq!(
        replayed.timestamp, birth.entry_timestamp,
        "CR 613.7d: replay installs the recorded timestamp instead of re-drawing one"
    );
    assert_eq!(
        replayed.name, token.name,
        "replay installs the same copied body the resolve path built"
    );
    assert_eq!(
        (replayed.power, replayed.toughness),
        (token.power, token.toughness)
    );
    assert_eq!(
        replayed.tapped, token.tapped,
        "CR 614.1: replay installs the recorded post-replacement tapped state"
    );
    assert!(
        replay.battlefield.contains(&token_id),
        "replay adds the copy to the battlefield zone list, not just the object map"
    );

    // The inverted precondition: this applier CREATES its subject, so a second
    // application is a typed invariant failure rather than a silent duplicate.
    assert_eq!(
        engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth),
        Err(ResolvedTokenCreationReplayInvariantError::ObjectAlreadyExists(token_id)),
        "a copy-token birth is not idempotent: re-applying it must fail closed"
    );

    // CR 302.6: the applier installs the RECORDED entry turn, never the live
    // one. Advancing `turn_number` on the replay state before applying is what
    // makes this non-vacuous — the assertion fails if the applier reads
    // `state.turn_number`, which is what it did before this command carried the
    // turn. A wrong entered-turn would let a replayed creature attack when its
    // summoning sickness should still forbid it.
    let mut shifted = outcome.state().clone();
    shifted.objects.remove(&token_id);
    shifted.battlefield.retain(|id| *id != token_id);
    shifted.turn_number = birth.entry_turn + 5;
    engine::game::effects::token::apply_resolved_token_creation(&mut shifted, birth)
        .expect("the recorded birth must replay regardless of the live turn");
    assert_eq!(
        shifted.objects[&token_id].entered_battlefield_turn,
        Some(birth.entry_turn),
        "CR 302.6: replay stamps the recorded entry turn, not the live one"
    );
    assert_ne!(
        shifted.objects[&token_id].entered_battlefield_turn,
        Some(shifted.turn_number),
        "CR 302.6: the live turn was deliberately shifted, so matching it would mean \
         the applier re-read game state instead of the record"
    );

    // Fail-closed on a mismatched recorded allocator draw: an id at or above its
    // own high-water cannot describe an allocation that happened.
    let mut tampered = birth.clone();
    tampered.resulting_next_object_id = token_id.0;
    let mut fresh = outcome.state().clone();
    fresh.objects.remove(&token_id);
    assert_eq!(
        engine::game::effects::token::apply_resolved_token_creation(&mut fresh, &tampered),
        Err(
            ResolvedTokenCreationReplayInvariantError::IdAboveHighWater {
                id: token_id,
                high_water: token_id.0,
            }
        ),
        "replay verifies the recorded high-water before installing anything"
    );
}

/// Applied Geometry (Sorcery) — verbatim Oracle text. Its CR 707.9b exceptions
/// all parse to `SetPower` / `SetToughness` / `AddSubtype` / `AddType`, none of
/// which force the committed-object path, and "Put six +1/+1 counters on it"
/// parses as a SEPARATE effect rather than as entry counters. So which seam this
/// card takes depends entirely on its TARGET: a plain permanent leaves
/// `etb_counters` empty (liminal, `Folded`), while a permanent that itself
/// enters with counters makes `etb_counters` non-empty (direct, `Deferred`).
const APPLIED_GEOMETRY_ORACLE: &str = "Create a token that's a copy of target non-Aura permanent \
     you control, except it's a 0/0 Fractal creature in addition to its other types. Put six \
     +1/+1 counters on it.";

/// Spike Feeder — verbatim Oracle text. Its intrinsic CR 614.1c entry-counter
/// replacement is what makes a copy of it take the direct seam.
const SPIKE_FEEDER_ORACLE: &str = "This creature enters with two +1/+1 counters on it.\n\
     {2}, Remove a +1/+1 counter from this creature: Put a +1/+1 counter on target creature.\n\
     Remove a +1/+1 counter from this creature: You gain 2 life.";

#[test]
fn copy_token_birth_folds_immediate_exceptions_and_replays_them() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let original = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Applied Geometry", true, APPLIED_GEOMETRY_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let committed = runner.cast(spell_id).target_object(original).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 707.2 reach guard: a copy token exists AND carries the copied body.
    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 707.2: the resolved effect must create a copy token");
    let token = &state.objects[&token_id];
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(
        token.name, "Grizzly Bears",
        "CR 707.2: the copy acquires the copiable name of the original"
    );

    // CR 707.9b reach guard: the exception actually applied to the copy. Without
    // this the body assertion below could pass on a run whose exception never
    // ran at all.
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Fractal"),
        "CR 707.9b: the 'in addition to its other types' exception must have applied"
    );

    let births: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(births.len(), 1, "the copy birth is journaled exactly once");

    // Seam reach guard: a plain copy target leaves `etb_counters` empty, so this
    // fixture must take the LIMINAL seam and record its exceptions as Folded —
    // meaning the body was complete at entry and replay can reproduce it.
    let birth = &births[0];
    let folded = match &birth.body {
        ResolvedTokenBody::Copy {
            modifications: ResolvedCopyBodyModifications::Folded { modifications, .. },
            ..
        } => modifications.clone(),
        other => panic!("expected a Copy body marked Folded, got {other:?}"),
    };
    assert!(
        !folded.is_empty(),
        "the folded marker must carry the exceptions it stamped onto the body"
    );

    // Replay-exactness WITH exceptions: the replayed body must carry the folded
    // modification, not just the bare copiable values.
    let mut replay = pre_state;
    engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth)
        .expect("a folded copy birth must replay against its captured predecessor");
    let replayed = &replay.objects[&token_id];
    assert!(
        replayed
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Fractal"),
        "CR 707.9b: replay reapplies the folded exception from the record"
    );
    // The exception set the body to 0/0; the six +1/+1 counters that took the
    // live token to 6/6 journal through the COUNTERS family, so this command
    // must NOT reproduce them. Asserting the birth body stays 0/0 is what proves
    // the birth command does not double-apply another family's work.
    assert_eq!(
        (replayed.power, replayed.toughness),
        (Some(0), Some(0)),
        "CR 707.9b: replay installs the exception's base body only — the CR 122.6a \
         entry counters belong to the counters family, not to the birth command"
    );
    assert_eq!(
        (token.power, token.toughness),
        (Some(6), Some(6)),
        "reach guard: the live token really did receive the six +1/+1 counters, so \
         the 0/0 replay body above is a deliberate exclusion, not a missing effect"
    );
}

#[test]
fn copy_token_birth_with_post_birth_modifications_refuses_to_replay() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Copying a permanent that itself enters with counters (CR 614.1c) makes
    // `etb_counters` non-empty, which routes the copy to the DIRECT seam — the
    // one that applies its exceptions after the birth.
    let original = scenario
        .add_creature_from_oracle(P0, "Spike Feeder", 0, 0, SPIKE_FEEDER_ORACLE)
        .id();
    // Spike Feeder is printed 0/0, so it needs its two entry counters to survive
    // CR 704.5f while the copy spell resolves.
    scenario.with_counter(original, CounterType::Plus1Plus1, 2);
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Applied Geometry", true, APPLIED_GEOMETRY_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let committed = runner.cast(spell_id).target_object(original).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 707.2 reach guard: a copy token exists and copied the right source.
    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 707.2: the resolved effect must create a copy token");
    let token = &state.objects[&token_id];
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(token.name, "Spike Feeder");

    // CR 707.9b reach guard: the post-birth modification actually ran on the
    // live object, which is what makes this birth unreplayable.
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Fractal"),
        "CR 707.9b: the exception must have applied AFTER the birth, via the unjournaled seam"
    );

    let births: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        births.len(),
        1,
        "the direct copy seam must still journal the birth even though it cannot be replayed"
    );

    // The safety-arm reach guard: this fixture must record the DEFERRED body. If
    // it recorded NoExceptions or Folded, the refusal below would be unreachable
    // and direct+modification births would be silently journaled as replayable —
    // the fail-open hole this arm exists to close.
    let birth = &births[0];
    let deferred_count = match &birth.body {
        ResolvedTokenBody::Copy {
            modifications:
                ResolvedCopyBodyModifications::DeferredToUnjournaledSeam { modifications },
            ..
        } => modifications.len(),
        other => panic!(
            "expected a Copy body marked DeferredToUnjournaledSeam, got {other:?}; the direct \
             seam's post-birth modifications would otherwise replay as if complete"
        ),
    };
    assert!(
        deferred_count > 0,
        "the deferred marker must carry the modifications it could not replay"
    );

    // The refusal: replay declines rather than installing a body missing the
    // post-birth modifications.
    let mut replay = pre_state;
    assert!(
        !replay.objects.contains_key(&token_id),
        "the copy does not exist before the command is applied"
    );
    assert_eq!(
        engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth),
        Err(
            ResolvedTokenCreationReplayInvariantError::UnreplayableCopyModifications {
                object: token_id,
                count: deferred_count,
            }
        ),
        "CR 707.9: a birth whose exceptions were applied by the unjournaled seam must fail closed"
    );

    // Fail-closed means nothing was installed — a partial materialization must
    // not hide behind the error.
    assert!(
        !replay.objects.contains_key(&token_id),
        "the refusal must materialize nothing"
    );
    assert!(
        !replay.battlefield.contains(&token_id),
        "the refusal must not touch the battlefield zone list either"
    );
}
