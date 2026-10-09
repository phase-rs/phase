//! Woodwork Prodigy (FRA): upkeep trigger prepares itself when unprepared.
//!
//! Verbatim front-face Oracle:
//!   "At the beginning of your upkeep, if this creature isn't prepared, it
//!    becomes prepared. (While it's prepared, you may cast a copy of its
//!    spell. Doing so unprepares it.)"
//!
//! Twin design driving one shared upkeep event: twin-a starts unprepared (its
//! trigger must fire and prepare it) while twin-b is prepared up front
//! through the real `prepare_object` path (its trigger must be suppressed by
//! the same event). The pair discriminates the
//! `Not(SourceMatchesFilter{creature + Prepared})` gate exactly — an
//! unconditional trigger would fire for both twins, a flipped polarity for
//! the wrong twin. CR 603.4 + CR 722.3a.
//!
//! Fixture-free like `fra_bloodline_recollector.rs`: both twins are built
//! with `from_oracle_text`, and the Prepare back face the Biblioplex gate
//! (`has_prepare_face`) requires is staged synthetically the same way.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WOODWORK_ORACLE: &str = "At the beginning of your upkeep, if this creature isn't prepared, it becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)";
const DOOM_BLADE_ORACLE: &str = "Destroy target creature.";
const CLOUDSHIFT_ORACLE: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

fn stack_entries_for(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn is_prepared(runner: &GameRunner, object: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|o| o.prepared.as_ref())
        .is_some()
}

fn setup_twins() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let unprepared = scenario
        .add_creature(P0, "Woodwork Prodigy", 2, 2)
        .from_oracle_text(WOODWORK_ORACLE)
        .id();
    let prepared = scenario
        .add_creature(P0, "Woodwork Prodigy", 2, 2)
        .from_oracle_text(WOODWORK_ORACLE)
        .id();
    // Removal + blink for the departure regressions below (mirrors
    // fra_bloodline_recollector.rs casting: default zero-cost builders).
    let doom_blade = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM_BLADE_ORACLE)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT_ORACLE)
        .id();
    let mut runner = scenario.build();

    // The Biblioplex gate (`has_prepare_face`, CR 722.3a reminder) no-ops
    // `prepare_object` on creatures without a Prepare back face, so stage one
    // on both twins exactly like `fra_bloodline_recollector.rs`.
    for twin in [unprepared, prepared] {
        runner.state_mut().objects.get_mut(&twin).unwrap().back_face = Some(BackFaceData {
            layout_kind: Some(LayoutKind::Prepare),
            ..BackFaceData::default()
        });
    }

    // Discriminating negative: the parsed Woodwork ability tree must contain
    // no Effect::Unimplemented. Before the prepared-gate fix the "if this
    // creature isn't prepared" clause was unparsed residue, so the trigger
    // either never fired or never gated.
    let parsed_json =
        serde_json::to_string(&runner.state().objects[&unprepared].trigger_definitions)
            .expect("serialize triggers");
    assert!(
        !parsed_json.contains("\"Unimplemented\""),
        "Woodwork Prodigy's parsed ability tree must contain no Effect::Unimplemented"
    );

    // Stage the suppression twin through the real prepare path (not a bare
    // field set), so its designation matches what resolution produces. Fail
    // loudly if staging itself did not take.
    {
        let mut events = Vec::new();
        engine::game::effects::prepare::prepare_object(runner.state_mut(), prepared, &mut events);
    }
    assert!(
        is_prepared(&runner, prepared),
        "staging must leave the suppression twin prepared"
    );
    assert!(
        !is_prepared(&runner, unprepared),
        "the firing twin must start unprepared"
    );
    (runner, unprepared, prepared, doom_blade, cloudshift)
}

/// CR 603.4 + CR 722.3a: at the shared upkeep event the unprepared twin's
/// trigger fires and prepares it (emitting `BecamePrepared`), while the
/// already-prepared twin's trigger is suppressed by the intervening-if.
#[test]
fn woodwork_prodigy_upkeep_prepares_unprepared_twin_only() {
    let (mut runner, unprepared, prepared, _, _) = setup_twins();

    // One shared upkeep event: exactly one Woodwork trigger (the unprepared
    // twin's) reaches the stack.
    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, unprepared),
        1,
        "the unprepared twin's upkeep trigger must be on the stack"
    );
    assert_eq!(
        stack_entries_for(&runner, prepared),
        0,
        "the prepared twin's trigger must be suppressed by the intervening-if"
    );

    // Resolve through priority, collecting events to confirm the
    // BecamePrepared emission for the firing twin only. The EffectResolved
    // presence below is the positive control for the recheck test's absence
    // assertion: dispatch runs on this path, so absence there means removal.
    let mut became_prepared = false;
    let mut effect_resolved = false;
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the upkeep trigger");
        became_prepared |= result.events.iter().any(
            |e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == unprepared),
        );
        effect_resolved |= result.events.iter().any(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::BecomePrepared,
                    source_id,
                    ..
                } if *source_id == unprepared
            )
        });
    }
    assert!(
        runner.state().stack.is_empty(),
        "the upkeep trigger must fully resolve"
    );
    assert!(
        became_prepared,
        "a GameEvent::BecamePrepared for the unprepared twin must have been emitted"
    );
    assert!(
        effect_resolved,
        "GameEvent::EffectResolved{{BecomePrepared}} for the firing twin must be emitted on the normal path"
    );
    assert!(
        is_prepared(&runner, unprepared),
        "the firing twin must be prepared after resolution"
    );
    assert!(
        is_prepared(&runner, prepared),
        "the suppressed twin must still be prepared — suppression read live state"
    );
}

/// CR 603.4 requires a live resolution-time recheck of the intervening-if:
/// if the firing twin becomes prepared after triggering but before
/// resolution, the ability must do nothing (no second `BecamePrepared`).
#[test]
fn woodwork_prodigy_intervening_if_is_rechecked_on_resolution() {
    let (mut runner, unprepared, prepared, _, _) = setup_twins();
    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, unprepared),
        1,
        "the unprepared twin's upkeep trigger must be on the stack"
    );

    // Prepare the firing twin before its trigger resolves.
    {
        let mut events = Vec::new();
        engine::game::effects::prepare::prepare_object(runner.state_mut(), unprepared, &mut events);
    }
    assert!(is_prepared(&runner, unprepared));

    // Discriminating assertion: the resolver pushes EffectResolved even when
    // prepare_object no-ops (idempotent), so only a skipped dispatch proves
    // the CR 603.4 recheck removed the ability. BecamePrepared absence alone
    // would also hold if the recheck were bypassed.
    let mut became_prepared_on_resolution = false;
    let mut effect_resolved_on_resolution = false;
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the upkeep trigger");
        became_prepared_on_resolution |= result.events.iter().any(
            |e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == unprepared),
        );
        effect_resolved_on_resolution |= result.events.iter().any(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::BecomePrepared,
                    source_id,
                    ..
                } if *source_id == unprepared
            )
        });
    }
    assert!(
        runner.state().stack.is_empty(),
        "the upkeep trigger must fully resolve"
    );
    assert!(
        !became_prepared_on_resolution,
        "no BecamePrepared may be emitted on resolution once the twin is already prepared"
    );
    assert!(
        !effect_resolved_on_resolution,
        "no EffectResolved{{BecomePrepared}} may be dispatched once the recheck removes the ability"
    );
    assert!(
        is_prepared(&runner, prepared),
        "the suppressed twin must still be prepared"
    );
}

/// CR 700.7 + CR 722.3a + CR 603.4: a prepared source that lost its creature
/// type (One with the Stars: "loses all other card types") stays bound to
/// "this creature", so the negated gate still suppresses its trigger. Only
/// the property-only filter shape (no creature-type gate) makes this hold —
/// a `TypedFilter::creature()` gate would spuriously fire here. Paired
/// positive: an unprepared type-stripped twin still fires, proving this is
/// not vacuous suppression-of-everything.
#[test]
fn woodwork_prodigy_type_loss_keeps_prepared_gate() {
    let (mut runner, unprepared, prepared, _, _) = setup_twins();
    for twin in [unprepared, prepared] {
        runner
            .state_mut()
            .objects
            .get_mut(&twin)
            .unwrap()
            .card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
    }
    for twin in [unprepared, prepared] {
        assert!(
            !runner.state().objects[&twin]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "type stripping must leave a non-creature permanent"
        );
    }
    assert!(
        is_prepared(&runner, prepared),
        "stripping types must not clear the prepared designation"
    );
    assert!(
        !is_prepared(&runner, unprepared),
        "the firing twin must still start unprepared"
    );

    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, unprepared),
        1,
        "the unprepared type-stripped twin's trigger must still fire"
    );
    assert_eq!(
        stack_entries_for(&runner, prepared),
        0,
        "the prepared type-stripped twin's trigger must stay suppressed"
    );
}

/// CR 400.7 + CR 110.1 + CR 722.3a: a trigger pending for a source that dies
/// before resolution must not prepare the graveyard card — the departed
/// object is a new object, not a battlefield permanent. The SelfRef
/// incarnation authority yields no target, so the departed card must remain
/// unprepared without a `BecamePrepared` event.
#[test]
fn woodwork_prodigy_trigger_does_not_prepare_departed_source() {
    let (mut runner, unprepared, _, doom_blade, _) = setup_twins();
    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, unprepared),
        1,
        "the unprepared twin's upkeep trigger must be on the stack"
    );

    let outcome = runner.cast(doom_blade).target_object(unprepared).resolve();
    outcome.assert_zone(&[unprepared], Zone::Graveyard);
    assert!(
        outcome.state().objects[&unprepared].prepared.is_none(),
        "the departed graveyard card must remain unprepared"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the upkeep trigger must fully resolve"
    );
    assert!(
        !outcome.events().iter().any(
            |event| matches!(event, GameEvent::BecamePrepared { object_id } if *object_id == unprepared)
        ),
        "a departed source must not gain the prepared designation"
    );
}

/// CR 400.7 + CR 722.3a: a same-id blink return is a new incarnation — the
/// pending trigger must not prepare it. Positive control: the returned
/// permanent's own next upkeep fires a fresh trigger normally.
#[test]
fn woodwork_prodigy_trigger_does_not_prepare_blinked_source() {
    let (mut runner, unprepared, _, _, cloudshift) = setup_twins();
    runner.advance_to_upkeep();
    assert_eq!(
        stack_entries_for(&runner, unprepared),
        1,
        "the unprepared twin's upkeep trigger must be on the stack"
    );

    let outcome = runner.cast(cloudshift).target_object(unprepared).resolve();
    // Reach guards: exactly one battlefield Woodwork is unprepared (the
    // blinked twin fresh); the prepared twin stays prepared.
    let fresh: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.name == "Woodwork Prodigy" && o.zone == Zone::Battlefield && o.prepared.is_none()
        })
        .map(|o| o.id)
        .collect();
    assert_eq!(
        fresh.len(),
        1,
        "the blink must return exactly one fresh unprepared Woodwork"
    );

    assert!(
        runner.state().stack.is_empty(),
        "the upkeep trigger must fully resolve"
    );
    assert!(
        !outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::BecamePrepared { .. })),
        "a blinked source must not gain the prepared designation from the stale trigger"
    );
    assert!(
        runner.state().objects[&fresh[0]].prepared.is_none(),
        "the returned permanent must still be unprepared"
    );

    // Positive control: the returned permanent's next upkeep fires normally.
    advance_to_next_upkeep_with_trigger(&mut runner, fresh[0]);
    assert_eq!(
        stack_entries_for(&runner, fresh[0]),
        1,
        "the returned permanent's next upkeep must fire a fresh trigger"
    );
}

/// Drive priority/combat windows until `source`'s trigger reaches the stack
/// (mirrors the `fra_bloodline_recollector.rs` advance loop).
fn advance_to_next_upkeep_with_trigger(runner: &mut GameRunner, source: ObjectId) {
    for _ in 0..400 {
        if stack_entries_for(runner, source) > 0 {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while advancing");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            other => panic!("unexpected waiting state while advancing: {other:?}"),
        }
    }
    panic!("the next upkeep trigger never fired");
}
