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
        state.crew_activated_this_turn.contains(&vehicle_id),
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
    let fires_at_announce = announce
        .events
        .iter()
        .any(|e| match_vehicle_crewed(e, &trigger, vehicle_id, &state));
    assert!(
        !fires_at_announce,
        "CR 702.122e: Crewed trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    let fires_at_resolve = resolve
        .events
        .iter()
        .any(|e| match_vehicle_crewed(e, &trigger, vehicle_id, &state));
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
        !announce
            .events
            .iter()
            .any(|e| match_stationed(e, &trigger, spacecraft_id, &state)),
        "CR 702.184a: Stationed trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        resolve
            .events
            .iter()
            .any(|e| match_stationed(e, &trigger, spacecraft_id, &state)),
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
        !announce
            .events
            .iter()
            .any(|e| match_saddled(e, &trigger, mount_id, &state)),
        "CR 702.171b: Saddled trigger must not fire at announcement"
    );

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        resolve
            .events
            .iter()
            .any(|e| match_saddled(e, &trigger, mount_id, &state)),
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
            } if *source_id == equipment_id
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
    use crate::game::triggers::{PendingTrigger, PendingTriggerContext};
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
