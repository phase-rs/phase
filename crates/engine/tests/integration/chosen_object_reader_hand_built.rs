//! Hand-built reader tests for `TargetFilter::ChosenCard` — the CR 607.2d
//! remembered-object linkage (`ChooseObjectsIntoTrackedSet` → `RememberCard` →
//! readers) that Zenos yae Galvus's parser arms consume in phase 2.
//!
//! The clauses under test have no parser arm yet, so these tests build the AST
//! directly (never from the fixture): a choice chain records the chosen creature
//! through the REAL `Effect::RememberCard` resolver, then reads it from
//!   * a mass pump (`Effect::PumpAll`, CR 611.2a/c) — Probes A and C,
//!   * a leaves-the-battlefield look-back trigger (`valid_card = ChosenCard`,
//!     CR 603.6c + CR 603.10a) — Probe B, and
//!   * the same pump behind a REAL `ChangesZone` ETB trigger resolved through
//!     the trigger pipeline — Probe D.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 607.2d: an ability that causes a player to "choose a [value]" and an
//!     ability that refers to "the chosen [value]" are linked.
//!   - CR 608.2c: the effect follows its instructions; the resolution-time
//!     choice is recorded onto the source.
//!   - CR 611.2a: a continuous effect lasts as long as stated ("until end of
//!     turn").
//!   - CR 611.2c: the affected set of an ability-generated continuous effect is
//!     determined when that effect begins.
//!   - CR 400.7: a zone change creates a new object, so the reader pins both
//!     the stable `ObjectId` and `incarnation` and matches only that occurrence.
//!   - CR 603.6c: leaves-the-battlefield abilities trigger on the zone change.
//!   - CR 603.10a: leaves-the-battlefield abilities look back in time.
//!   - CR 609.3: an effect that attempts to do something impossible does only
//!     as much as possible.

use super::rules::{
    GameAction, GameRunner, GameScenario, ObjectId, Phase, WaitingFor, Zone, P0, P1,
};
use engine::game::layers::evaluate_layers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChosenAttribute, ControllerRef, Duration, Effect, FilterProp,
    PtValue, QuantityExpr, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::identifiers::TrackedSetId;
use engine::types::triggers::TriggerMode;

/// A creature an opponent controls — the eligible pool for the choice head.
fn opponent_creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
}

/// The choice head: "choose a creature an opponent controls" (CR 607.2d).
fn choice_head() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ChooseObjectsIntoTrackedSet {
            chooser: TargetFilter::Controller,
            filter: opponent_creature_filter(),
            min: 1,
            max: Some(1),
            cardinality: None,
            eligibility: None,
        },
    )
}

/// The writer step: record the just-chosen object (the chain's fresh tracked
/// set) onto the source as `ChosenAttribute::Card` (CR 608.2c).
fn remember_card_step() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RememberCard {
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
        },
    )
}

/// The reader step: "-2/-2 until end of turn to each creature other than the
/// source and the remembered object" — the population is fixed at resolution
/// (CR 611.2c) and the reader excludes exactly the `ChosenCard` (CR 607.2d).
fn pump_all_step() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PumpAll {
            power: PtValue::Fixed(-2),
            toughness: PtValue::Fixed(-2),
            target: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::Another]),
                    ),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::ChosenCard),
                    },
                ],
            },
        },
    )
    .duration(Duration::UntilEndOfTurn)
}

/// Choose → remember. Minimal linked-choice chain for the departure probe.
fn remember_only_chain() -> AbilityDefinition {
    choice_head().sub_ability(remember_card_step())
}

/// Choose → remember → mass pump. The full phase-2-shaped chain.
fn choice_chain() -> AbilityDefinition {
    choice_head().sub_ability(remember_card_step().sub_ability(pump_all_step()))
}

/// Drive the choice head on `host` to its prompt, assert the published
/// range/eligible set, then select `chosen`.
fn drive_choice(
    runner: &mut GameRunner,
    host: ObjectId,
    chosen: ObjectId,
    expected_eligible: &[ObjectId],
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: 0,
        })
        .expect("activate the choice chain");
    runner.advance_until_stack_empty();

    let WaitingFor::ChooseObjectsSelection {
        min, max, eligible, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the choice head must park on ChooseObjectsSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (*min, *max),
        (1, Some(1)),
        "an exact one-of choice must publish (1, Some(1))"
    );
    let mut actual = eligible.clone();
    actual.sort();
    let mut expected_refs: Vec<TargetRef> = expected_eligible
        .iter()
        .map(|id| TargetRef::Object(*id))
        .collect();
    expected_refs.sort();
    assert_eq!(
        actual, expected_refs,
        "the eligible pool must be exactly the opponent creatures, got {eligible:?}"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(chosen)],
        })
        .expect("select the remembered creature");
    runner.advance_until_stack_empty();
}

/// The remembered ids recorded on `host` via `ChosenAttribute::Card`.
fn remembered_cards(runner: &GameRunner, host: ObjectId) -> Vec<ObjectId> {
    runner.state().objects[&host]
        .chosen_attributes
        .iter()
        .filter_map(|attribute| match attribute {
            ChosenAttribute::Card(pin) => Some(pin.object_id),
            _ => None,
        })
        .collect()
}

/// `(power, toughness)` after the current layer state.
fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

// ─── Probe A: the pump excludes exactly the source and the remembered object ──

/// CR 607.2d + CR 611.2a/c: after the choice, `RememberCard` records the chosen
/// id and the pump shrinks every creature OTHER than the source (`Another`) and
/// the remembered object (`Not{ChosenCard}`). The host reads 4/4 (base, excluded
/// by `Another`), the remembered object 3/3 (base, excluded by the reader), and
/// both non-chosen creatures 3/3 → 1/1.
#[test]
fn choice_remembers_and_pump_excludes_source_and_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = {
        let mut builder = scenario.add_creature(P0, "Reader Host", 4, 4);
        builder.with_ability_definition(choice_chain());
        builder.id()
    };
    let chosen = scenario.add_creature(P1, "Chosen Face", 3, 3).id();
    let other_opponent = scenario.add_creature(P1, "Other Opponent", 3, 3).id();
    let own_other = scenario.add_creature(P0, "Own Other", 3, 3).id();

    let mut runner = scenario.build();
    drive_choice(&mut runner, host, chosen, &[chosen, other_opponent]);

    // Positive reach-guard: the REAL `RememberCard` resolver ran and recorded
    // exactly the chosen id (an absent prompt or a broken writer fails here).
    assert_eq!(
        remembered_cards(&runner, host),
        vec![chosen],
        "the chosen creature must be the sole remembered card (CR 608.2c)"
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(&runner, host),
        (Some(4), Some(4)),
        "the pump's source is excluded via FilterProp::Another (it must not shrink itself)"
    );
    assert_eq!(
        pt(&runner, chosen),
        (Some(3), Some(3)),
        "the remembered object is excluded via Not{{ChosenCard}} (it must not shrink)"
    );
    assert_eq!(
        pt(&runner, other_opponent),
        (Some(1), Some(1)),
        "a non-chosen opponent creature must shrink by -2/-2"
    );
    assert_eq!(
        pt(&runner, own_other),
        (Some(1), Some(1)),
        "a non-chosen creature of any controller must shrink by -2/-2"
    );
}

// ─── Probe B: LKI departure reader ───────────────────────────────────────────

/// CR 603.6c + CR 603.10a + CR 607.2d: a leaves-the-battlefield trigger whose
/// `valid_card` is the remembered-object reader fires only for the remembered
/// object's departure. The non-chosen departure is the paired negative (stack
/// stays empty, life unchanged); the chosen departure queues exactly one
/// trigger that resolves to +3 life.
#[test]
fn chosen_departure_fires_lki_trigger_only_for_remembered_object() {
    let lifetime_trigger = TriggerDefinition::new(TriggerMode::LeavesBattlefield)
        .valid_card(TargetFilter::ChosenCard)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = {
        let mut builder = scenario.add_creature(P0, "Reader Host", 4, 4);
        builder.with_ability_definition(remember_only_chain());
        builder.with_trigger_definition(lifetime_trigger);
        builder.id()
    };
    let chosen = scenario.add_creature(P1, "Chosen Face", 3, 3).id();
    let other_opponent = scenario.add_creature(P1, "Other Opponent", 3, 3).id();

    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    drive_choice(&mut runner, host, chosen, &[chosen, other_opponent]);
    assert_eq!(
        remembered_cards(&runner, host),
        vec![chosen],
        "the choice must be recorded before the departure probes"
    );

    // Negative: the NON-chosen opponent departs — the look-back reader must not
    // re-identify it, so no trigger queues and no life is gained.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(other_opponent, Zone::Graveyard, other_opponent),
            &mut events,
        ),
        "the non-chosen departure must terminate, not park on a replacement choice"
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "a non-remembered departure must not queue the ChosenCard trigger"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "the silent non-chosen departure must not gain life"
    );

    // Positive: the REMEMBERED object departs — the LKI arm matches the
    // record's own pre-change occurrence against the source's remembered pin.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(chosen, Zone::Graveyard, chosen),
            &mut events,
        ),
        "the remembered departure must terminate, not park on a replacement choice"
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the remembered object's departure must queue exactly the LTB trigger (CR 603.10a)"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 3,
        "the queued trigger's GainLife{{3}} execute must resolve for the trigger's controller"
    );
}

// ─── Probe C: no legal choice → CR 609.3 ─────────────────────────────────────

/// CR 609.3: with no opponent creature to choose, the effect does only as much
/// as possible — the prompt is still raised at `(0, Some(0))` with an empty
/// eligible set, the chain resolves an empty selection, nothing is remembered,
/// and the pump still shrinks the creatures it can affect.
#[test]
fn no_legal_choice_still_pumps_and_remembers_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = {
        let mut builder = scenario.add_creature(P0, "Reader Host", 4, 4);
        builder.with_ability_definition(choice_chain());
        builder.id()
    };
    let own_other = scenario.add_creature(P0, "Own Other", 3, 3).id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: 0,
        })
        .expect("activate the choice chain");
    runner.advance_until_stack_empty();

    let WaitingFor::ChooseObjectsSelection {
        min, max, eligible, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the choice head must still park with no eligible object, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (*min, *max),
        (0, Some(0)),
        "CR 609.3 clamps an impossible exact choice to the achievable (0, Some(0)) range"
    );
    assert!(
        eligible.is_empty(),
        "no opponent creature exists to offer, got {eligible:?}"
    );

    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("an empty selection resolves the impossible choice (CR 609.3)");
    runner.advance_until_stack_empty();

    assert!(
        remembered_cards(&runner, host).is_empty(),
        "an empty choice must record no ChosenAttribute::Card"
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(&runner, host),
        (Some(4), Some(4)),
        "the source stays excluded from the pump"
    );
    assert_eq!(
        pt(&runner, own_other),
        (Some(1), Some(1)),
        "CR 609.3: the pump still applies to every creature it can affect"
    );
}

// ─── Probe D: the same chain as a REAL ChangesZone ETB trigger ───────────────

/// The host's ETB trigger: "When this creature enters, [choose an opponent's
/// creature, remember it, shrink every other creature except the source and the
/// remembered one]". Hand-built (Zenos yae Galvus's clause has no parser arm
/// yet — phase 2) but driven through the REAL trigger pipeline.
fn etb_choice_chain_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
        .execute(choice_chain())
}

/// CR 603.6a/c + CR 607.2d + CR 608.2c: the discriminating REAL-trigger-path
/// test for the remembered-object reader. The choice chain is the EXECUTE of an
/// actual `ChangesZone` ETB trigger: the host moves hand → battlefield through
/// the production zone-change pipeline (`move_object_for_test`), the trigger is
/// collected by `process_triggers`, and its resolution parks on the real
/// `WaitingFor::ChooseObjectsSelection` prompt answered with
/// `GameAction::SelectTargets`.
///
/// `Effect::RememberCard` writes `ChosenAttribute::Card` to the LIVE source,
/// NOT to the resolution chain's latched `TriggerSourceContext`, so the pump's
/// `Not{ChosenCard}` exclusion only observes the fresh choice if
/// `source_context_from_filter` layers the live entry over the stale latched
/// snapshot for an exact-live source. Without that overlay the chosen creature
/// shrinks to 1/1 (revert-probe); with it the chosen creature stays 3/3.
#[test]
fn real_etb_trigger_chain_excludes_the_just_remembered_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The host starts in HAND so its entry through the real zone-change
    // pipeline raises the ETB trigger (a builder-placed battlefield permanent
    // would never fire `ChangesZone`).
    let host = {
        let mut builder = scenario.add_creature_to_hand(P0, "Reader Host", 4, 4);
        builder.with_trigger_definition(etb_choice_chain_trigger());
        builder.id()
    };
    let chosen = scenario.add_creature(P1, "Chosen Face", 3, 3).id();
    let other_opponent = scenario.add_creature(P1, "Other Opponent", 3, 3).id();
    let own_other = scenario.add_creature(P0, "Own Other", 3, 3).id();

    let mut runner = scenario.build();

    // Real zone change + real trigger scan: the host's own ETB trigger lands on
    // the stack with its latched `TriggerSourceContext`.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(host, Zone::Battlefield, host),
            &mut events,
        ),
        "the hand -> battlefield entry must terminate, not park on a replacement choice"
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == host),
        "the host's ChangesZone ETB must be on the stack after the real move + scan"
    );

    // Resolve the trigger through the real stack until its choice prompt.
    runner.advance_until_stack_empty();

    let WaitingFor::ChooseObjectsSelection {
        min, max, eligible, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the triggered chain must park on ChooseObjectsSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        (*min, *max),
        (1, Some(1)),
        "an exact one-of choice must publish (1, Some(1))"
    );
    let mut actual = eligible.clone();
    actual.sort();
    let mut expected_refs = vec![TargetRef::Object(chosen), TargetRef::Object(other_opponent)];
    expected_refs.sort();
    assert_eq!(
        actual, expected_refs,
        "the eligible pool must be exactly the two opponent creatures, got {eligible:?}"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(chosen)],
        })
        .expect("select the remembered creature");
    runner.advance_until_stack_empty();

    // Positive reach-guard: the REAL `RememberCard` resolver ran inside the
    // trigger's resolution and recorded exactly the chosen id on the live host
    // (the writer itself is not in question here; the READER is).
    assert_eq!(
        remembered_cards(&runner, host),
        vec![chosen],
        "the triggered chain's RememberCard must record the chosen creature (CR 608.2c)"
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        pt(&runner, chosen),
        (Some(3), Some(3)),
        "the just-remembered creature must be excluded via Not{{ChosenCard}} on the \
         SAME trigger resolution — the live ChosenAttribute::Card writer must be \
         visible to the reader even though the latched trigger context predates it"
    );
    assert_eq!(
        pt(&runner, host),
        (Some(4), Some(4)),
        "the trigger source is excluded via FilterProp::Another (it must not shrink itself)"
    );
    assert_eq!(
        pt(&runner, other_opponent),
        (Some(1), Some(1)),
        "a non-chosen opponent creature must shrink by -2/-2"
    );
    assert_eq!(
        pt(&runner, own_other),
        (Some(1), Some(1)),
        "a non-chosen creature of any controller must shrink by -2/-2"
    );
}
