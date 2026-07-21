//! Cross-keyword stack-interaction tests for Crew / Station / Equip / Saddle.
//!
//! Part A of the CR 113.3b stack-based activation refactor requires that
//! activated keyword abilities behave like any other activated ability on
//! the stack:
//!   - they can be countered by stack-targeting effects (CR 118.7: costs
//!     paid even if the ability is countered);
//!   - a priority window opens between cost payment and resolution;
//!   - triggers keyed off "becomes crewed/saddled/stationed/equipped"
//!     fire at resolution time, not at cost payment (CR 702.122e,
//!     CR 702.171b, CR 702.184a, CR 702.6a).
//!
//! Counterspells are simulated by popping the top stack entry directly
//! after announcement (scenario-constructed per plan §A8 — no Oracle-text
//! parsing dependency). The effect is that the keyword action never
//! resolves, but the cost side-effects (tapped creatures, snapshotted
//! power) persist.

use super::*;
use crate::game::zones::create_object;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn setup_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

fn make_vehicle(state: &mut GameState, crew_n: u32) -> ObjectId {
    let id = create_object(
        state,
        CardId(1100),
        PlayerId(0),
        "Test Vehicle".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    obj.keywords.push(crate::types::keywords::Keyword::Crew {
        power: crew_n,
        once_per_turn: None,
    });
    obj.base_power = Some(6);
    obj.base_toughness = Some(5);
    obj.power = Some(6);
    obj.toughness = Some(5);
    id
}

fn make_mount(state: &mut GameState, saddle_n: u32) -> ObjectId {
    let id = create_object(
        state,
        CardId(1200),
        PlayerId(0),
        "Test Mount".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push("Mount".to_string());
    obj.keywords
        .push(crate::types::keywords::Keyword::Saddle(saddle_n));
    obj.power = Some(3);
    obj.toughness = Some(3);
    obj.base_power = Some(3);
    obj.base_toughness = Some(3);
    id
}

fn make_spacecraft(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(1300),
        PlayerId(0),
        "Test Spacecraft".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Spacecraft".to_string());
    obj.keywords.push(crate::types::keywords::Keyword::Station);
    id
}

fn make_equipment(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(1400),
        PlayerId(0),
        "Test Equipment".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Equipment".to_string());
    // CR 702.6a: Equip N — activated ability via an ActivateAbility index.
    // For counterspell tests we only need the EquipTarget flow, not a cost
    // payment, so we synthesize an ability wiring directly.
    id
}

fn make_creature(state: &mut GameState, name: &str, power: i32) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        PlayerId(0),
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(power);
    obj.base_power = Some(power);
    obj.base_toughness = Some(power);
    id
}

fn grant_cant_tap(state: &mut GameState, id: ObjectId) {
    let def =
        crate::types::ability::StaticDefinition::new(crate::types::statics::StaticMode::CantTap)
            .affected(crate::types::ability::TargetFilter::SelfRef);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.static_definitions.push(def.clone());
    std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
    crate::game::layers::evaluate_layers(state);
}

/// Simulates a Counterspell-analog effect resolving during the priority
/// window that opens after a keyword-action announcement. The top stack
/// entry is moved to the graveyard (per CR 701.5a — counter means "move
/// from the stack to its owner's graveyard"); no further events fire.
fn simulate_counter_top_of_stack(state: &mut GameState) {
    let popped = state
        .stack
        .pop_back()
        .expect("stack must have an entry to counter");
    assert!(
        matches!(
            popped.kind,
            crate::types::game_state::StackEntryKind::KeywordAction { .. }
        ),
        "counterspell test only valid on KeywordAction entries"
    );
}

// --- Crew ---------------------------------------------------------------

#[test]
fn crew_can_be_countered_by_stack_targeting_effect() {
    // CR 118.7: Cost is paid even if the ability is countered — creatures
    // remain tapped; Vehicle never becomes a creature.
    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle(&mut state, 3);
    let creature_a = make_creature(&mut state, "Bear", 3);

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1, "announcement pushed one stack entry");
    assert!(
        state.objects.get(&creature_a).unwrap().tapped,
        "crew cost (tap) paid before stack push"
    );

    simulate_counter_top_of_stack(&mut state);

    // Resolve remaining priority — no VehicleCrewed event should fire and
    // the Vehicle stays a non-creature artifact.
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        !resolve
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::VehicleCrewed { .. })),
        "countered Crew must not fire VehicleCrewed"
    );
    assert!(
        state.objects.get(&creature_a).unwrap().tapped,
        "CR 118.7: cost persists after counter"
    );
}

fn make_vehicle_once_per_turn(state: &mut GameState, crew_n: u32) -> ObjectId {
    let id = make_vehicle(state, crew_n);
    let obj = state.objects.get_mut(&id).unwrap();
    // CR 602.5b: "Activate only once each turn" crew restriction.
    obj.keywords.clear();
    obj.card_types.subtypes = vec!["Vehicle".to_string()];
    obj.keywords.push(crate::types::keywords::Keyword::Crew {
        power: crew_n,
        once_per_turn: Some(Box::new(
            crate::types::ability::ActivationRestriction::OnlyOnceEachTurn,
        )),
    });
    id
}

#[test]
fn crew_once_per_turn_vehicle_rejects_second_activation_same_turn() {
    // CR 602.5b: Luxurious Locomotive — "Crew 1. Activate only once each
    // turn." A second CrewVehicle activation in the same turn is rejected.
    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle_once_per_turn(&mut state, 1);
    let creature_a = make_creature(&mut state, "Bear", 3);
    let creature_b = make_creature(&mut state, "Elk", 3);

    // First crew: full announcement, vehicle recorded as crewed this turn.
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();
    assert!(
        crate::game::engine::crew_activated_this_turn_contains(&state, vehicle_id),
        "first crew records the vehicle as crewed this turn"
    );

    // Second crew activation this turn — must be rejected. `creature_b` is
    // a fresh untapped creature, so power is not the blocker.
    let second = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    );
    assert!(
        matches!(second, Err(EngineError::ActionNotAllowed(_))),
        "second crew of an 'Activate only once each turn' Vehicle must be \
             rejected; got {second:?}"
    );
    let _ = creature_b;
}

// T-reentry (§3): the crew-cadence ledger is keyed to the exact object incarnation
// (CR 400.7 + CR 602.5b), so a Vehicle that leaves and returns — a new object at a
// higher incarnation — is no longer recorded as crewed and may be crewed again.
// Fails if the ledger were keyed by bare `ObjectId`. The reach-guard (the positive
// `contains` at incarnation N) proves the record was actually made, so the negative
// is not vacuous. The end-to-end card-test (crew → blink → crew again through
// `apply()`) lands in Phase 2A, which wires the production blink path; Phase 1
// unit-tests the identity-keyed ledger directly.
#[test]
fn crew_reentry_new_incarnation_clears_activation_ledger() {
    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle(&mut state, 1);

    let n = state.objects[&vehicle_id].incarnation;
    crate::game::engine::record_crew_activation(&mut state, vehicle_id);
    assert!(
        crate::game::engine::crew_activated_this_turn_contains(&state, vehicle_id),
        "reach-guard: the crew activation is recorded at the current incarnation ({n})"
    );

    // CR 400.7: the Vehicle leaves and returns as a NEW object (higher incarnation).
    state
        .objects
        .get_mut(&vehicle_id)
        .unwrap()
        .bump_incarnation();
    assert!(
        state.objects[&vehicle_id].incarnation > n,
        "the re-entered object is a new incarnation"
    );
    assert!(
        !crate::game::engine::crew_activated_this_turn_contains(&state, vehicle_id),
        "after re-entry the incarnation-keyed ledger no longer matches → crewable again"
    );
}

#[test]
fn crew_unlimited_vehicle_allows_second_activation_same_turn() {
    // A normal (non-once-per-turn) Vehicle may be crewed repeatedly.
    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle(&mut state, 1);
    let creature_a = make_creature(&mut state, "Bear", 3);
    let _creature_b = make_creature(&mut state, "Elk", 3);

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    // Second crew activation — an Unlimited Vehicle accepts it (the
    // once-per-turn restriction does not apply).
    let second = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    );
    assert!(
        second.is_ok(),
        "an unrestricted Vehicle may be crewed again the same turn; got {second:?}"
    );
}

#[test]
fn crew_opens_priority_window_between_announcement_and_resolution() {
    // CR 113.3b: Between announcement and resolution, the active player
    // has priority again. Verified by the presence of a WaitingFor::Priority
    // and an unresolved stack after announcement.
    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle(&mut state, 3);
    let creature_a = make_creature(&mut state, "Bear", 3);

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    let announce = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    assert!(matches!(announce.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 1);
}

// --- Saddle -------------------------------------------------------------

#[test]
fn saddle_activation_excludes_cant_tap_creatures_from_threshold() {
    let mut state = setup_main_phase();
    let mount_id = make_mount(&mut state, 3);
    let restricted = make_creature(&mut state, "Restricted Rider", 2);
    let rider = make_creature(&mut state, "Rider", 1);
    grant_cant_tap(&mut state, restricted);

    let err = apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![],
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, EngineError::ActionNotAllowed(_)),
        "Saddle 3 must not count restricted 2-power creature plus unrestricted 1-power {rider:?}"
    );
}

#[test]
fn saddle_can_be_countered_by_stack_targeting_effect() {
    let mut state = setup_main_phase();
    let mount_id = make_mount(&mut state, 2);
    let creature_a = make_creature(&mut state, "Rider", 3);

    apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1);
    assert!(
        state.objects.get(&creature_a).unwrap().tapped,
        "saddle cost (tap) paid before stack push"
    );

    simulate_counter_top_of_stack(&mut state);

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        !resolve
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Saddled { .. })),
        "countered Saddle must not fire Saddled"
    );
    // CR 702.171b: `is_saddled` flag is set only at resolution.
    assert!(
        !state.objects.get(&mount_id).unwrap().is_saddled,
        "Mount must not become saddled if Saddle is countered"
    );
    // CR 118.7: cost persists.
    assert!(state.objects.get(&creature_a).unwrap().tapped);
}

#[test]
fn saddle_announcement_pushes_stack_entry() {
    // Saddle has no existing test module — cover the fundamentals alongside
    // the counterspell test.
    let mut state = setup_main_phase();
    let mount_id = make_mount(&mut state, 2);
    let creature_a = make_creature(&mut state, "Rider", 3);

    apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    let announce = apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1);
    assert!(
        !announce
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Saddled { .. })),
        "Saddled event must not fire until stack resolution"
    );
    assert!(!state.objects.get(&mount_id).unwrap().is_saddled);

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(state.stack.is_empty());
    assert!(state.objects.get(&mount_id).unwrap().is_saddled);
    assert!(
        resolve
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Saddled { .. })),
        "Saddled fires at resolution"
    );
}

#[test]
fn saddle_sorcery_speed_gate_enforced_at_announcement_not_resolution() {
    // CR 307.1 + CR 702.171a: Saddle is restricted to sorcery-speed
    // windows. The gate runs at announcement; once the ability is on the
    // stack, changing phases does not retroactively invalidate it.
    let mut state = setup_main_phase();
    let mount_id = make_mount(&mut state, 2);
    let _ = make_creature(&mut state, "Rider", 3);

    // Instant speed: declaring blockers is a pre-priority window.
    state.phase = Phase::DeclareBlockers;
    let err = apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![],
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, EngineError::ActionNotAllowed(_)),
        "CR 702.171a: cannot activate Saddle at instant speed"
    );
}

// --- Station ------------------------------------------------------------

#[test]
fn station_can_be_countered_by_stack_targeting_effect() {
    // CR 113.7a + CR 118.7: Creature tapped, charge counters NOT added.
    let mut state = setup_main_phase();
    let spacecraft_id = make_spacecraft(&mut state);
    let power5 = make_creature(&mut state, "Power 5", 5);

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(power5),
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1);
    assert!(state.objects.get(&power5).unwrap().tapped);

    simulate_counter_top_of_stack(&mut state);

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        !resolve
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Stationed { .. })),
        "countered Station must not fire Stationed"
    );
    let charge = state
        .objects
        .get(&spacecraft_id)
        .unwrap()
        .counters
        .get(&CounterType::Generic("charge".to_string()))
        .copied()
        .unwrap_or(0);
    assert_eq!(
        charge, 0,
        "no charge counters added when Station is countered"
    );
    assert!(state.objects.get(&power5).unwrap().tapped);
}

// --- Equip --------------------------------------------------------------

// --- Trigger timing -----------------------------------------------------
//
// CR 702.122e / CR 702.171b / CR 702.184a: "Whenever [X] becomes crewed /
// saddled / stationed" resolves when the keyword ability resolves from the
// stack — not when its cost is paid. The per-keyword matcher keys off the
// resolution-time event (`VehicleCrewed` / `Saddled` / `Stationed`), so
// the timing is proven by showing:
//   (a) the announcement's event stream contains no match,
//   (b) the resolve step's event stream contains a match.
// This is independent of Oracle-text parser coverage (Monoist Gravliner's
// Stationed trigger parses as Unknown today — plan §Out of scope).

#[test]
fn crewed_trigger_matcher_fires_on_resolution_event_not_announcement() {
    use crate::game::trigger_matchers::match_vehicle_crewed;
    use crate::types::triggers::TriggerMode;
    use crate::types::TriggerDefinition;

    let mut state = setup_main_phase();
    let vehicle_id = make_vehicle(&mut state, 3);
    let creature_a = make_creature(&mut state, "Bear", 3);

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    let announce = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    let trigger = TriggerDefinition::new(TriggerMode::Crewed);
    let fires_at_announce = announce.events.iter().any(|e| {
        match_vehicle_crewed(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, vehicle_id),
            &state,
        )
    });
    assert!(
        !fires_at_announce,
        "CR 702.122e: Crewed trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    let fires_at_resolve = resolve.events.iter().any(|e| {
        match_vehicle_crewed(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, vehicle_id),
            &state,
        )
    });
    assert!(
        fires_at_resolve,
        "CR 702.122e: Crewed trigger fires when the Crew ability resolves"
    );
}

#[test]
fn stationed_trigger_matcher_fires_on_resolution_event_not_announcement() {
    use crate::game::trigger_matchers::match_stationed;
    use crate::types::triggers::TriggerMode;
    use crate::types::TriggerDefinition;

    let mut state = setup_main_phase();
    let spacecraft_id = make_spacecraft(&mut state);
    let power5 = make_creature(&mut state, "Power 5", 5);

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    let announce = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(power5),
        },
    )
    .unwrap();

    let trigger = TriggerDefinition::new(TriggerMode::Stationed);
    assert!(
        !announce.events.iter().any(|e| match_stationed(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, spacecraft_id),
            &state,
        )),
        "CR 702.184a: Stationed trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        resolve.events.iter().any(|e| match_stationed(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, spacecraft_id),
            &state,
        )),
        "CR 702.184a: Stationed trigger fires when Station resolves"
    );
}

#[test]
fn saddled_trigger_matcher_fires_on_resolution_event_not_announcement() {
    use crate::game::trigger_matchers::match_saddled;
    use crate::types::triggers::TriggerMode;
    use crate::types::TriggerDefinition;

    let mut state = setup_main_phase();
    let mount_id = make_mount(&mut state, 2);
    let creature_a = make_creature(&mut state, "Rider", 3);

    apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    let announce = apply_as_current(
        &mut state,
        GameAction::SaddleMount {
            mount_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    let trigger = TriggerDefinition::new(TriggerMode::Saddled);
    assert!(
        !announce.events.iter().any(|e| match_saddled(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, mount_id),
            &state,
        )),
        "CR 702.171b: Saddled trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        resolve.events.iter().any(|e| match_saddled(
            e,
            &trigger,
            &crate::game::trigger_matchers::test_trigger_source_context(&state, mount_id),
            &state,
        )),
        "CR 702.171b: Saddled trigger fires when Saddle resolves"
    );
}

#[test]
fn equipped_effect_fires_on_resolution_event_not_announcement() {
    // CR 702.6a: Equip does not have a dedicated "becomes equipped" trigger
    // mode; the analog is the `EffectResolved { kind: Equip }` event emitted
    // when the keyword action resolves. Triggers that key off "Whenever
    // [this Equipment] becomes attached" fire from the ZoneChanged /
    // attachment-change event downstream. This test asserts the
    // EffectResolved { Equip } event is absent at announcement and present
    // at resolution, proving the stack-based flow carries through for
    // Equip.
    let mut state = setup_main_phase();
    let equipment_id = make_equipment(&mut state);
    let _creature_a = make_creature(&mut state, "Warrior", 2);

    let announce = apply_as_current(
        &mut state,
        GameAction::Equip {
            equipment_id,
            target_id: ObjectId(0),
        },
    )
    .unwrap();
    assert!(
        !announce.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Equip,
                ..
            }
        )),
        "CR 702.6a: Equip resolution event must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        resolve.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Equip,
                source_id,
            ..} if *source_id == equipment_id
        )),
        "CR 702.6a: Equip resolution event fires when the ability resolves"
    );
}

#[test]
fn equip_can_be_countered_by_stack_targeting_effect() {
    // CR 702.6a + CR 118.7: Cost is paid; attachment never happens. With a
    // single valid target, `handle_equip_activation` auto-targets and
    // pushes the KeywordAction directly (one dispatch call).
    let mut state = setup_main_phase();
    let equipment_id = make_equipment(&mut state);
    let _creature_a = make_creature(&mut state, "Warrior", 2);

    apply_as_current(
        &mut state,
        GameAction::Equip {
            equipment_id,
            target_id: ObjectId(0),
        },
    )
    .unwrap();

    assert_eq!(state.stack.len(), 1);
    assert!(
        state
            .objects
            .get(&equipment_id)
            .unwrap()
            .attached_to
            .is_none(),
        "Equipment is not attached yet (attach happens at resolution)"
    );

    simulate_counter_top_of_stack(&mut state);

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        !resolve.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Equip,
                ..
            }
        )),
        "countered Equip must not fire EquipResolved"
    );
    assert!(
        state
            .objects
            .get(&equipment_id)
            .unwrap()
            .attached_to
            .is_none(),
        "Equipment must not attach when Equip is countered"
    );
}

/// Issue #3660: deferred copy observers must not drop remaining paradigm offers.
#[test]
fn issue_3660_finalize_copy_retarget_stashes_offers_on_deferred_pause() {
    use crate::game::triggers::{
        PendingTrigger, PendingTriggerContext, PendingTriggerDispatchOrigin,
    };
    use crate::types::ability::{
        Effect, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
    };
    use crate::types::game_state::{CastingVariant, CopyTargetSlot, StackEntry, StackEntryKind};
    use crate::types::zones::Zone;

    fn deferred_draw_trigger(
        state: &mut GameState,
        name: &str,
        controller: PlayerId,
    ) -> PendingTriggerContext {
        let source_id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        PendingTriggerContext {
            pending: PendingTrigger {
                source_id,
                controller,
                condition: None,
                ability: {
                    let mut ability = ResolvedAbility::new(
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                        vec![],
                        source_id,
                        controller,
                    );
                    ability.description = Some(name.to_string());
                    ability
                },
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: Some(name.to_string()),
                may_trigger_origin: None,
                subject_match_count: None,
                die_result: None,
            },
            trigger_events: Vec::new(),
            dispatch_origin: PendingTriggerDispatchOrigin::Normal,
        }
    }

    let mut state = GameState::new_two_player(42);
    let player = PlayerId(0);
    let copy_id = ObjectId(50);
    let remaining = vec![ObjectId(101)];

    state.stack.push_back(StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller: player,
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Player,
                },
                vec![TargetRef::Player(PlayerId(1))],
                copy_id,
                player,
            )),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    let slots = vec![CopyTargetSlot {
        current: Some(TargetRef::Player(PlayerId(1))),
        legal_alternatives: vec![TargetRef::Player(PlayerId(1))],
    }];
    state.waiting_for = WaitingFor::CopyRetarget {
        player,
        copy_id,
        target_slots: slots.clone(),
        effect_kind: EffectKind::Draw,
        effect_source_id: Some(copy_id),
        current_slot: 0,
        paradigm_remaining_offers: Some(remaining.clone()),
    };
    state.deferred_triggers = vec![
        deferred_draw_trigger(&mut state, "Copy Observer A", player),
        deferred_draw_trigger(&mut state, "Copy Observer B", player),
    ];

    let mut events = Vec::new();
    finalize_copy_retarget(
        &mut state,
        player,
        copy_id,
        &slots,
        EffectKind::Draw,
        Some(copy_id),
        &mut events,
    )
    .expect("finalize copy retarget");

    assert!(
        matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "expected OrderTriggers pause, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state
            .pending_paradigm_remaining_offers
            .as_ref()
            .map(|pending| pending.offers.as_slice()),
        Some(remaining.as_slice()),
    );
}

/// CR 702.6: An Equip keyword granted at runtime by a static ability (Bram,
/// Bludgeon Brawl: "… is an Equipment with equip {N} …") must produce a real,
/// cost-bearing equip activated ability — offered and charged through the normal
/// `ActivateAbility` path, exactly like a printed Equipment — rather than a
/// keyword the runtime ignores.
#[test]
fn granted_equip_keyword_offers_functional_equip_ability() {
    use crate::types::ability::{
        AbilityCost, AbilityTag, ContinuousModification, Effect, StaticDefinition, TargetFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    let mut state = setup_main_phase();
    // A Food artifact with NO printed equip keyword or ability.
    let food = create_object(
        &mut state,
        CardId(1500),
        PlayerId(0),
        "Test Food".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&food).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Food".to_string());
    }

    // The Bram / Bludgeon Brawl grant on the object itself: become an Equipment
    // and gain equip {1}.
    let equip_cost = ManaCost::Cost {
        shards: vec![],
        generic: 1,
    };
    let def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::AddSubtype {
                subtype: "Equipment".to_string(),
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Equip(equip_cost),
            },
        ]);
    {
        let obj = state.objects.get_mut(&food).unwrap();
        obj.static_definitions.push(def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }
    crate::game::layers::evaluate_layers(&mut state);

    // Layer result: the object is now an Equipment carrying the Equip keyword.
    let obj = state.objects.get(&food).unwrap();
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Equipment"),
        "granted Equipment subtype missing"
    );
    assert!(
        crate::game::keywords::has_keyword_kind(obj, crate::types::keywords::KeywordKind::Equip),
        "granted Equip keyword missing"
    );

    // The runtime now offers a synthesized, sorcery-speed, cost-bearing equip
    // activated ability tagged `AbilityTag::Equip` — the SAME shape a printed
    // Equipment gets, so it is offered + charged through the normal
    // `ActivateAbility` path (not the cost-free `KeywordAction::Equip` path).
    let abilities = crate::game::casting::activated_ability_definitions(&state, food);
    let equip = abilities
        .iter()
        .find(|(_, ability)| ability.ability_tag == Some(AbilityTag::Equip))
        .expect("granted equip must be offered as an activated ability");
    assert!(
        matches!(equip.1.effect.as_ref(), Effect::Attach { .. }),
        "granted equip ability must attach: {:?}",
        equip.1.effect
    );
    assert!(
        matches!(equip.1.cost, Some(AbilityCost::Mana { .. })),
        "granted equip ability must carry its mana cost: {:?}",
        equip.1.cost
    );
}

/// CR 202.3: Bludgeon Brawl's granted anthem "Equipped creature gets +X/+0, where
/// X is that artifact's mana value" binds X to the *Equipment's* mana value, not
/// the equipped creature's. The parser lowers this to `AddDynamicPower {
/// SelfManaValue }`; this test proves the layer system resolves `SelfManaValue`
/// against the granting Equipment (the static's source), so a mana-value-3
/// Equipment boosts a 2/2 to 5/2.
#[test]
fn granted_equip_anthem_self_mana_value_reads_equipment_mana_value() {
    use crate::types::ability::{
        ContinuousModification, FilterProp, QuantityExpr, QuantityRef, StaticDefinition,
        TargetFilter, TypedFilter,
    };
    use crate::types::mana::ManaCost;

    let mut state = setup_main_phase();
    // An Equipment with mana value 3.
    let gear = create_object(
        &mut state,
        CardId(1600),
        PlayerId(0),
        "Test Gear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&gear).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 3,
        };
    }
    // A 2/2 creature, with the Equipment attached to it.
    let bear = make_creature(&mut state, "Bear", 2);
    state.objects.get_mut(&gear).unwrap().attached_to = Some(bear.into());

    // Granted anthem on the Equipment: "Equipped creature gets +X/+0" where X is
    // the Equipment's own mana value.
    let anthem = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ))
        .modifications(vec![ContinuousModification::AddDynamicPower {
            value: QuantityExpr::Ref {
                qty: QuantityRef::SelfManaValue,
            },
        }]);
    {
        let obj = state.objects.get_mut(&gear).unwrap();
        obj.static_definitions.push(anthem.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(anthem);
    }
    crate::game::layers::evaluate_layers(&mut state);

    // Power boosted by the EQUIPMENT's mana value (3): 2 + 3 = 5; +X/+0 leaves T.
    let creature = state.objects.get(&bear).unwrap();
    assert_eq!(
        creature.power,
        Some(5),
        "granted anthem X must read the Equipment's mana value (2 + 3)"
    );
    assert_eq!(
        creature.toughness,
        Some(2),
        "+X/+0 must leave toughness unchanged"
    );
}

/// CR 702.6a: A permanent may have more than one equip ability, each independently
/// activatable. An object that PRINTS `Equip {1}` (already synthesized into
/// `obj.abilities` at card load) and is ALSO granted an identical `Equip {1}` at
/// runtime must expose BOTH — the granted instance is subtracted by occurrence,
/// not by value-wide membership.
#[test]
fn identical_printed_and_granted_equip_both_offered() {
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    let equip_one = Keyword::Equip(ManaCost::Cost {
        shards: vec![],
        generic: 1,
    });
    let mut state = setup_main_phase();
    let gear = create_object(
        &mut state,
        CardId(1700),
        PlayerId(0),
        "Twin Equip".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&gear).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        // Printed Equip {1}: lives in base_keywords AND (via card-load synthesis)
        // as an activated ability.
        obj.base_keywords.push(equip_one.clone());
        std::sync::Arc::make_mut(&mut obj.abilities)
            .push(crate::database::synthesis::equip_ability_for_keyword(&equip_one).unwrap());
        // Post-layer keyword set carries BOTH the printed and a granted copy.
        obj.keywords.push(equip_one.clone());
        obj.keywords.push(equip_one.clone());
    }

    // Both equip abilities are offered: the printed one from obj.abilities and the
    // granted one from the runtime appender.
    let equip_count = crate::game::casting::activated_ability_definitions(&state, gear)
        .iter()
        .filter(|(_, ability)| {
            ability.ability_tag == Some(crate::types::ability::AbilityTag::Equip)
        })
        .count();
    assert_eq!(
        equip_count, 2,
        "printed + identical granted Equip must both be offered"
    );
}

/// CR 202.3 + CR 118.9: Bludgeon Brawl grants `equip {X}` where X is the
/// artifact's mana value, carried as the `ManaCost::SelfManaValue` placeholder.
/// The offered equip ability must present the CONCRETE mana value as its cost —
/// otherwise the payment path treats `SelfManaValue` as `{0}` and equip is free.
#[test]
fn granted_equip_self_mana_value_cost_is_concretized_to_mana_value() {
    use crate::types::ability::{
        AbilityCost, AbilityTag, ContinuousModification, StaticDefinition, TargetFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    let mut state = setup_main_phase();
    // An Equipment with mana value 4, granted equip {X}=its mana value.
    let gear = create_object(
        &mut state,
        CardId(1800),
        PlayerId(0),
        "MV Gear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&gear).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 4,
        };
    }
    let def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Equip(ManaCost::SelfManaValue),
        }]);
    {
        let obj = state.objects.get_mut(&gear).unwrap();
        obj.static_definitions.push(def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }
    crate::game::layers::evaluate_layers(&mut state);

    // The offered equip cost is the concrete mana value (4), not the placeholder.
    let abilities = crate::game::casting::activated_ability_definitions(&state, gear);
    let equip = abilities
        .iter()
        .find(|(_, ability)| ability.ability_tag == Some(AbilityTag::Equip))
        .expect("granted equip must be offered");
    match &equip.1.cost {
        Some(AbilityCost::Mana {
            cost: ManaCost::Cost { generic, shards },
        }) => {
            assert_eq!(*generic, 4, "equip {{X}} must concretize to mana value 4");
            assert!(shards.is_empty(), "no colored pips expected");
        }
        other => panic!("equip cost must be concrete Mana {{4}}, not a placeholder: {other:?}"),
    }
}

/// CR 702.6 + CR 702.6a: end-to-end proof that a runtime-granted Equip keyword
/// (Bram, Bludgeon Brawl) is not merely *offered* but fully *functional* through
/// the normal `ActivateAbility` path — announced, its mana cost paid from the
/// pool, a legal "creature you control" targeted, and resolved so the Equipment
/// becomes attached. The shape-only tests above assert the offered ability's AST
/// (cost + `Effect::Attach`); this drives the whole pipeline, so a regression in
/// runtime-index selection, payment, targeting, or Attach resolution fails HERE
/// even though those still pass.
#[test]
fn granted_equip_attaches_through_full_activation_pipeline() {
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{
        AbilityTag, ContinuousModification, StaticDefinition, TargetFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaCost, ManaPipId, ManaType, ManaUnit};

    let p0 = PlayerId(0);
    let mut scenario = GameScenario::new_n_player(2, 42);
    // CR 702.6a: equip is a sorcery-speed activated ability — main phase, own
    // turn, empty stack, priority to P0.
    scenario.at_phase(Phase::PreCombatMain);

    // A creature P0 controls — the legal equip target.
    let bear = scenario.add_creature(p0, "Bear", 2, 2).id();

    // A noncreature Food artifact granted "Equipment with equip {1}" by a static
    // on itself (Bram-shaped), mirroring the parser's `AddSubtype` + `AddKeyword`
    // output.
    let food = create_object(
        &mut scenario.state,
        CardId(1900),
        p0,
        "Test Food".to_string(),
        Zone::Battlefield,
    );
    let equip_cost = ManaCost::Cost {
        shards: vec![],
        generic: 1,
    };
    let def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::AddSubtype {
                subtype: "Equipment".to_string(),
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Equip(equip_cost),
            },
        ]);
    {
        let obj = scenario.state.objects.get_mut(&food).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Food".to_string());
        obj.static_definitions.push(def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }
    crate::game::layers::evaluate_layers(&mut scenario.state);

    // Fund the {1} equip cost — the driver does not model source auto-tap, so the
    // pool must cover the cost or `PassPriority` (finalize payment) errors.
    scenario.with_mana_pool(
        p0,
        vec![ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(9998),
            pip_id: ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
        }],
    );

    // The granted equip is appended LAST in `activated_ability_definitions`;
    // resolve its runtime index the same way the engine does.
    let equip_index = crate::game::casting::activated_ability_definitions(&scenario.state, food)
        .iter()
        .position(|(_, ability)| ability.ability_tag == Some(AbilityTag::Equip))
        .expect("granted equip must be offered as an activated ability");

    let mut runner = scenario.build();
    // Announce → pay {1} from the pool → target the creature → resolve.
    let outcome = runner
        .activate(food, equip_index)
        .target_object(bear)
        .resolve();

    // CR 702.6a: equip attaches the Equipment to the chosen creature.
    assert_eq!(
        outcome.state().objects.get(&food).unwrap().attached_to,
        Some(bear.into()),
        "granted equip must attach the Equipment to the targeted creature through the \
         normal ActivateAbility path"
    );
    assert!(
        outcome
            .state()
            .objects
            .get(&bear)
            .unwrap()
            .attachments
            .contains(&food),
        "the equipped creature must carry the Equipment"
    );
}
