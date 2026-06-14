//! Tests for the Archenemy runtime (CR 904 / CR 314). Declared from
//! `game/mod.rs` so `archenemy.rs` stays implementation-only (no inline tests).
//!
//! Most tests call the set-in-motion / abandon / SBA functions directly and
//! assert the resulting command-zone / scheme-deck / event output. Several are
//! deliberately discriminating: they fail if the corresponding fix is reverted.
//!
//! Exactly-once trigger collection is NOT asserted on the direct-call
//! set-in-motion tests — `set_in_motion` deliberately does not self-collect (the
//! post-action scan owns that). The pipeline-driven
//! `set_in_motion_collects_trigger_exactly_once` test drives
//! `apply_as_current(PassPriority)` through `run_post_action_pipeline` and reads
//! 2 if its fix is reverted. `abandon` is tested directly because CR 701.33a only
//! allows face-up ongoing schemes to be abandoned; the CR 904.10 non-ongoing SBA
//! is not an abandon action and must not fire `SchemeAbandoned`.

use super::archenemy::{
    abandon, active_schemes, check_scheme_abandon_sba, is_scheme_object, set_in_motion, top_scheme,
};
use super::engine::apply_as_current;
use super::triggers::{DeferredTrigger, PendingTrigger};
use crate::database::synthesis::synthesize_archenemy;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, StaticDefinition,
    TargetFilter, TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use std::str::FromStr;

/// Build a `CardFace` for a scheme carrying the given triggers, statics, and
/// supertypes, then run `synthesize_archenemy` (the production stamping step) so
/// the trigger/static zones reflect the real card-build path.
fn synthesized_scheme_face(
    triggers: Vec<TriggerDefinition>,
    statics: Vec<StaticDefinition>,
    supertypes: Vec<Supertype>,
) -> CardFace {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Scheme);
    face.card_type.supertypes = supertypes;
    face.triggers = triggers;
    face.static_abilities = statics;
    synthesize_archenemy(&mut face);
    face
}

/// A `SetInMotion` trigger that draws a card.
fn set_in_motion_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::SetInMotion)
        .valid_card(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Controller)
        .execute(draw_ability())
}

/// An `Abandoned` trigger that draws a card.
fn abandoned_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Abandoned)
        .valid_card(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Controller)
        .execute(draw_ability())
}

fn draw_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
}

/// Create a scheme object directly in `state.objects`, applying its synthesized
/// trigger/static definitions and setting its controller. Returns its id. The
/// object is NOT placed in any zone vector — the caller decides command zone vs
/// scheme deck.
fn create_scheme_object(
    state: &mut GameState,
    name: &str,
    face: &CardFace,
    controller: PlayerId,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let mut obj = crate::game::game_object::GameObject::new(
        id,
        CardId(id.0),
        controller,
        name.to_string(),
        Zone::Command,
    );
    obj.controller = controller;
    obj.card_types = face.card_type.clone();
    for trig in &face.triggers {
        obj.trigger_definitions.push(trig.clone());
    }
    for st in &face.static_abilities {
        obj.static_definitions.push(st.clone());
    }
    state.objects.insert(id, obj);
    id
}

/// Place a face-down scheme deck (front = top), designate `archenemy`. Returns
/// the deck ids in order. Schemes are NOT placed in the command zone — the
/// scheme deck holds them face down until set in motion.
fn setup_scheme_deck(
    state: &mut GameState,
    archenemy: PlayerId,
    deck: &[(&str, &CardFace)],
) -> Vec<ObjectId> {
    let mut deck_ids = Vec::new();
    for (name, face) in deck {
        let id = create_scheme_object(state, name, face, archenemy);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.face_down = true;
        }
        state.scheme_deck.push_back(id);
        deck_ids.push(id);
    }
    state.archenemy = Some(archenemy);
    deck_ids
}

/// Place a single face-up scheme in the command zone (already set in motion),
/// designate `archenemy`. Returns its id.
fn setup_active_scheme(
    state: &mut GameState,
    archenemy: PlayerId,
    name: &str,
    face: &CardFace,
) -> ObjectId {
    let id = create_scheme_object(state, name, face, archenemy);
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.face_down = false;
    }
    state.command_zone.push_back(id);
    state.archenemy = Some(archenemy);
    id
}

/// Count live trigger instances sourced from `scheme_id`: stack entries whose
/// source is the scheme + deferred-queue entries whose pending source is the
/// scheme. Exactly-once == 1. Mirrors the on-stack / waiting-to-be-put-on-stack
/// split of `scheme_trigger_on_stack_or_pending` (CR 904.10).
fn scheme_trigger_instances(state: &GameState, scheme_id: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|e| e.source_id == scheme_id)
        .count()
        + state
            .deferred_triggers
            .iter()
            .filter(|d| d.pending.source_id == scheme_id)
            .count()
}

// ---------------------------------------------------------------------------
// 1. CoreType round-trip
// ---------------------------------------------------------------------------

#[test]
fn coretype_scheme_roundtrip() {
    // CR 314: Scheme is a nontraditional, non-permanent card type that offers no
    // protection quality.
    let s = CoreType::Scheme.to_string();
    assert_eq!(s, "Scheme");
    assert_eq!(CoreType::from_str(&s), Ok(CoreType::Scheme));
    // CR 314.2: not a permanent type.
    assert!(!CoreType::Scheme.is_permanent_type());
    assert_eq!(CoreType::Scheme.protection_quality_str(), None);
}

// ---------------------------------------------------------------------------
// 2. set_in_motion promotes the top scheme, emits the event, and stamps the
//    controller (DISCRIMINATING). Exactly-once trigger collection is covered by
//    the pipeline-driven runtime test `set_in_motion_collects_trigger_exactly_once`.
// ---------------------------------------------------------------------------

#[test]
fn set_in_motion_promotes_event_and_stamps_controller() {
    // DISCRIMINATING: fails if `set_in_motion` stops turning the scheme face up /
    // stamping the controller, or stops emitting `SchemeSetInMotion`.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    let non_arch = PlayerId(1);
    let scheme = synthesized_scheme_face(vec![set_in_motion_trigger()], vec![], vec![]);
    let deck_ids = setup_scheme_deck(&mut state, arch, &[("Scheme A", &scheme)]);
    let scheme_id = deck_ids[0];
    assert_eq!(top_scheme(&state), Some(scheme_id));
    // The scheme enters set_in_motion carrying a stale/foreign controller (the
    // non-archenemy player), so the CR 314.5 controller stamp must actively
    // correct it — this makes the controller assertion below discriminating.
    state.objects.get_mut(&scheme_id).unwrap().controller = non_arch;
    assert_ne!(
        state.objects.get(&scheme_id).unwrap().controller,
        arch,
        "scheme starts under a non-archenemy controller before set_in_motion"
    );

    let mut events = Vec::new();
    set_in_motion(&mut state, &mut events);

    // CR 904.9 / CR 701.32b: the scheme is now face up in the command zone.
    assert!(
        state.command_zone.contains(&scheme_id),
        "scheme moved into the command zone"
    );
    assert!(
        !state.objects.get(&scheme_id).unwrap().face_down,
        "scheme is face up"
    );
    assert!(state.scheme_deck.is_empty(), "scheme left the scheme deck");
    assert_eq!(active_schemes(&state), vec![scheme_id]);
    // CR 314.5: the archenemy is the controller of the face-up scheme.
    assert_eq!(
        state.objects.get(&scheme_id).unwrap().controller,
        arch,
        "archenemy stamped as the scheme's controller"
    );
    // CR 904.9: SchemeSetInMotion emitted, keyed to the scheme + archenemy.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::SchemeSetInMotion { scheme_id: s, player_id: p }
            if *s == scheme_id && *p == arch
        )),
        "SchemeSetInMotion event emitted, got {events:?}"
    );
    // NOTE: exactly-once trigger collection is asserted by the pipeline-driven
    // `set_in_motion_collects_trigger_exactly_once` runtime test, not here.
    // `set_in_motion` deliberately does NOT self-collect (the post-action scan
    // owns that), so a direct call leaves `deferred_triggers` empty.
}

// ---------------------------------------------------------------------------
// 3. set_in_motion is a no-op outside an Archenemy game
// ---------------------------------------------------------------------------

#[test]
fn set_in_motion_noop_outside_archenemy() {
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    let scheme = synthesized_scheme_face(vec![], vec![], vec![]);
    let deck_ids = setup_scheme_deck(&mut state, arch, &[("Scheme A", &scheme)]);
    let scheme_id = deck_ids[0];
    // Not an Archenemy game.
    state.archenemy = None;

    let mut events = Vec::new();
    set_in_motion(&mut state, &mut events);

    assert_eq!(
        top_scheme(&state),
        Some(scheme_id),
        "scheme deck untouched when archenemy is None"
    );
    assert!(
        !state.command_zone.contains(&scheme_id),
        "scheme not promoted when archenemy is None"
    );
    assert!(events.is_empty(), "no events when archenemy is None");
}

// ---------------------------------------------------------------------------
// 4. precombat main sets the top scheme in motion (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn begin_precombat_main_sets_scheme_in_motion() {
    // DISCRIMINATING: fails if the `set_in_motion` hook is removed from
    // `finish_enter_phase`. Driving the phase machinery into PreCombatMain with
    // the active player = archenemy must set the top scheme in motion.
    use crate::types::phase::Phase;

    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    let scheme = synthesized_scheme_face(vec![], vec![], vec![]);
    let deck_ids = setup_scheme_deck(&mut state, arch, &[("Scheme A", &scheme)]);
    let scheme_id = deck_ids[0];

    // Drive the real phase pipeline into PreCombatMain (Draw -> PreCombatMain).
    state.phase = Phase::Draw;
    let mut events = Vec::new();
    crate::game::turns::advance_phase(&mut state, &mut events);
    assert_eq!(state.phase, Phase::PreCombatMain);

    assert!(
        state.command_zone.contains(&scheme_id),
        "archenemy's precombat main set the top scheme in motion"
    );
    assert!(
        !state.objects.get(&scheme_id).unwrap().face_down,
        "scheme is face up after precombat main"
    );

    // A non-archenemy active player's precombat main does NOT set in motion.
    let mut state2 = GameState::new_two_player(7);
    let arch2 = PlayerId(0);
    let non_arch = PlayerId(1);
    state2.active_player = non_arch;
    let scheme2 = synthesized_scheme_face(vec![], vec![], vec![]);
    let deck_ids2 = setup_scheme_deck(&mut state2, arch2, &[("Scheme B", &scheme2)]);
    let scheme_id2 = deck_ids2[0];

    state2.phase = Phase::Draw;
    let mut events2 = Vec::new();
    crate::game::turns::advance_phase(&mut state2, &mut events2);
    assert_eq!(state2.phase, Phase::PreCombatMain);
    assert_eq!(
        top_scheme(&state2),
        Some(scheme_id2),
        "non-archenemy's precombat main must NOT set a scheme in motion"
    );
    assert!(
        !state2.command_zone.contains(&scheme_id2),
        "scheme stays in the deck on a non-archenemy turn"
    );
}

// ---------------------------------------------------------------------------
// 5. ongoing scheme static applies only while face up (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn ongoing_scheme_static_applies_only_while_face_up() {
    // DISCRIMINATING: fails if `synthesize_archenemy` stops stamping
    // `active_zones = [Command]` onto the scheme's static (the command-zone static
    // scan would never include it). After abandon the scheme leaves the active
    // command-zone view, so its static no longer applies.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    let mut scheme_static = StaticDefinition::new(StaticMode::Continuous);
    scheme_static.description = Some("scheme-static-marker".to_string());
    let scheme = synthesized_scheme_face(vec![], vec![scheme_static], vec![Supertype::Ongoing]);

    // Sanity: synthesis stamped the command zone onto the static.
    assert!(
        scheme.static_abilities[0]
            .active_zones
            .contains(&Zone::Command),
        "synthesize_archenemy must stamp Zone::Command on the scheme static"
    );

    let scheme_id = setup_active_scheme(&mut state, arch, "Ongoing Scheme", &scheme);

    // While face up in the command zone, the static is yielded by the real scan.
    let active_present = crate::game::functioning_abilities::game_active_statics(&state)
        .any(|(obj, _)| obj.id == scheme_id);
    assert!(
        active_present,
        "ongoing scheme static must apply while the scheme is face up"
    );

    // After abandon, the scheme is no longer in the command zone, so its static
    // no longer applies.
    let mut events = Vec::new();
    abandon(&mut state, scheme_id, &mut events);
    let still_present = crate::game::functioning_abilities::game_active_statics(&state)
        .any(|(obj, _)| obj.id == scheme_id);
    assert!(
        !still_present,
        "scheme static must NOT apply after the scheme is abandoned"
    );
}

// ---------------------------------------------------------------------------
// 6. non-ongoing scheme is turned down on resolution (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn nonongoing_scheme_turns_down_on_resolution() {
    // DISCRIMINATING: fails if `check_scheme_abandon_sba` no longer turns down a
    // face-up non-ongoing scheme, or if it stops respecting an on-stack scheme
    // trigger.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    let scheme = synthesized_scheme_face(vec![], vec![], vec![]);
    let scheme_id = setup_active_scheme(&mut state, arch, "One-Shot Scheme", &scheme);

    // With a scheme trigger ON THE STACK, the SBA does nothing (CR 904.10).
    state.stack.push_back(StackEntry {
        id: ObjectId(99_999),
        source_id: scheme_id,
        controller: arch,
        kind: StackEntryKind::TriggeredAbility {
            source_id: scheme_id,
            ability: Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                scheme_id,
                arch,
            )),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });
    let mut events = Vec::new();
    let mut any = false;
    check_scheme_abandon_sba(&mut state, &mut events, &mut any);
    assert!(
        !any,
        "no abandon while the scheme's ability is on the stack"
    );
    assert!(
        state.command_zone.contains(&scheme_id),
        "scheme still face up while its ability is on the stack"
    );

    // Clear the stack: now the SBA turns the scheme face down and puts it on
    // the bottom of the scheme deck.
    state.stack.clear();
    let mut events2 = Vec::new();
    let mut any2 = false;
    check_scheme_abandon_sba(&mut state, &mut events2, &mut any2);
    assert!(any2, "turn down once the ability leaves the stack");
    assert!(
        state.objects.get(&scheme_id).unwrap().face_down,
        "abandoned scheme is face down"
    );
    assert!(
        !state.command_zone.contains(&scheme_id),
        "abandoned scheme left the command zone"
    );
    assert_eq!(
        state.scheme_deck.back().copied(),
        Some(scheme_id),
        "abandoned scheme is on the bottom of the scheme deck"
    );
}

// ---------------------------------------------------------------------------
// 7. a deferred scheme trigger also blocks abandon (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn deferred_scheme_trigger_blocks_abandon() {
    // DISCRIMINATING (reviewer-requested negative): a scheme trigger sitting in
    // `deferred_triggers` (NOT yet on the stack) also blocks abandon — covers the
    // "waiting to be put on the stack" half of CR 904.10.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    // A face-up non-ongoing scheme that the SBA would otherwise abandon.
    let scheme = synthesized_scheme_face(vec![], vec![], vec![]);
    let scheme_id = setup_active_scheme(&mut state, arch, "Trigger Scheme", &scheme);

    // Seed the deferred queue directly with a scheme-sourced pending trigger
    // (NOT yet on the stack). `set_in_motion` no longer self-collects, so this
    // test seeds the "waiting to be put on the stack" state explicitly — the
    // analogue of `nonongoing_scheme_abandons_on_resolution` seeding an on-stack
    // `StackEntry`.
    let pending = PendingTrigger {
        source_id: scheme_id,
        controller: arch,
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            scheme_id,
            arch,
        ),
        timestamp: 0,
        target_constraints: vec![],
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    state.deferred_triggers.push(DeferredTrigger {
        pending,
        trigger_events: vec![],
    });
    assert!(
        state
            .deferred_triggers
            .iter()
            .any(|d| d.pending.source_id == scheme_id),
        "scheme trigger is deferred (waiting to be put on the stack)"
    );

    // The abandon SBA must do nothing while the trigger is waiting (CR 904.10).
    let mut events2 = Vec::new();
    let mut any = false;
    check_scheme_abandon_sba(&mut state, &mut events2, &mut any);
    assert!(
        !any,
        "no abandon while a scheme trigger is waiting to be put on the stack"
    );
    assert!(
        state.command_zone.contains(&scheme_id),
        "scheme stays face up while its trigger is deferred"
    );
    assert!(
        !state.objects.get(&scheme_id).unwrap().face_down,
        "scheme stays face up while its trigger is deferred"
    );
}

// ---------------------------------------------------------------------------
// 8. ongoing scheme is not abandoned by the SBA (CR 904.11)
// ---------------------------------------------------------------------------

#[test]
fn ongoing_scheme_not_abandoned_by_sba() {
    // CR 904.11: an ongoing scheme is never abandoned by the SBA, even with the
    // stack clear.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    let scheme = synthesized_scheme_face(vec![], vec![], vec![Supertype::Ongoing]);
    let scheme_id = setup_active_scheme(&mut state, arch, "Ongoing Scheme", &scheme);

    let mut events = Vec::new();
    let mut any = false;
    check_scheme_abandon_sba(&mut state, &mut events, &mut any);
    assert!(!any, "ongoing scheme must not be abandoned by the SBA");
    assert!(
        state.command_zone.contains(&scheme_id),
        "ongoing scheme stays face up in the command zone"
    );
    assert!(
        !state.objects.get(&scheme_id).unwrap().face_down,
        "ongoing scheme stays face up"
    );
}

// ---------------------------------------------------------------------------
// 9. abandon fires the Abandoned trigger for ongoing schemes
// ---------------------------------------------------------------------------

#[test]
fn abandon_fires_abandoned_trigger() {
    // `abandon` intentionally retains its inline self-collect (single authority):
    // CR 701.33a allows only face-up ongoing schemes to be abandoned, and it
    // collects the trigger while the scheme is still face up in the command zone,
    // then removes it — so no later scan can re-collect it (no pipeline filter
    // needed). This direct call defers the trigger exactly once.
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    let scheme =
        synthesized_scheme_face(vec![abandoned_trigger()], vec![], vec![Supertype::Ongoing]);
    let scheme_id = setup_active_scheme(&mut state, arch, "Abandon Scheme", &scheme);

    let mut events = Vec::new();
    abandon(&mut state, scheme_id, &mut events);

    // CR 701.33b: SchemeAbandoned emitted, keyed to the scheme.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::SchemeAbandoned { scheme_id: s, .. } if *s == scheme_id
        )),
        "SchemeAbandoned event emitted, got {events:?}"
    );
    // CR 603.3: the Abandoned trigger is collected into the deferred queue.
    assert!(
        state
            .deferred_triggers
            .iter()
            .any(|d| d.pending.source_id == scheme_id),
        "Abandoned trigger from {scheme_id:?} must be collected, got {:?}",
        state.deferred_triggers
    );
}

// ---------------------------------------------------------------------------
// 10. synthesize_archenemy appends Command, preserving pre-existing zones
// ---------------------------------------------------------------------------

#[test]
fn synthesize_archenemy_appends_command_zone() {
    // `synthesize_archenemy` must PUSH Zone::Command onto any pre-existing zone
    // list, not overwrite it, and be idempotent.
    let mut trigger = TriggerDefinition::new(TriggerMode::SetInMotion);
    trigger.trigger_zones = vec![Zone::Exile];
    let mut static_def = StaticDefinition::new(StaticMode::Continuous);
    static_def.active_zones = vec![Zone::Exile];

    let face = synthesized_scheme_face(vec![trigger], vec![static_def], vec![]);

    assert!(
        face.triggers[0].trigger_zones.contains(&Zone::Exile)
            && face.triggers[0].trigger_zones.contains(&Zone::Command),
        "pre-existing trigger zone preserved and Command appended, got {:?}",
        face.triggers[0].trigger_zones
    );
    assert!(
        face.static_abilities[0].active_zones.contains(&Zone::Exile)
            && face.static_abilities[0]
                .active_zones
                .contains(&Zone::Command),
        "pre-existing static zone preserved and Command appended, got {:?}",
        face.static_abilities[0].active_zones
    );

    // Idempotent: re-synthesis does not duplicate Command.
    let mut face2 = face;
    synthesize_archenemy(&mut face2);
    let command_count = face2.triggers[0]
        .trigger_zones
        .iter()
        .filter(|z| **z == Zone::Command)
        .count();
    assert_eq!(
        command_count, 1,
        "Command must not be duplicated on re-synthesis"
    );
}

// ---------------------------------------------------------------------------
// 11. archenemy = None skips the abandon SBA
// ---------------------------------------------------------------------------

#[test]
fn archenemy_none_skips_abandon_sba() {
    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.active_player = arch;
    let scheme = synthesized_scheme_face(vec![], vec![], vec![]);
    let scheme_id = setup_active_scheme(&mut state, arch, "Scheme", &scheme);
    // Not an Archenemy game.
    state.archenemy = None;
    // Sanity: the object is recognized as a scheme regardless.
    assert!(is_scheme_object(&state, scheme_id));

    let mut events = Vec::new();
    let mut any = false;
    check_scheme_abandon_sba(&mut state, &mut events, &mut any);
    assert!(!any, "no abandon SBA work when archenemy is None");
    assert!(
        state.command_zone.contains(&scheme_id),
        "scheme untouched when there is no archenemy"
    );
    assert!(events.is_empty(), "no events when archenemy is None");
}

// ---------------------------------------------------------------------------
// 12. set_in_motion collects the trigger EXACTLY ONCE through the real pipeline
//     (DISCRIMINATING — fails with count 2 if the inline self-collect is
//     restored, because the post-action scan would then collect it a second time)
// ---------------------------------------------------------------------------

/// Give each player a library card so the Draw step's draw turn-based action
/// doesn't pause the pipeline on an empty-library / loss condition.
fn seed_libraries(state: &mut GameState) {
    for (seat, pid) in [PlayerId(0), PlayerId(1)].into_iter().enumerate() {
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let obj = crate::game::game_object::GameObject::new(
            id,
            CardId(id.0),
            pid,
            format!("Filler {seat}"),
            Zone::Library,
        );
        state.objects.insert(id, obj);
        state.players[seat].library.push_back(id);
    }
}

#[test]
fn set_in_motion_collects_trigger_exactly_once() {
    // DISCRIMINATING: drives the REAL priority pipeline from the archenemy's Draw
    // step into PreCombatMain. `finish_enter_phase` calls `set_in_motion`, then
    // `run_post_action_pipeline`'s post-action scan collects the SchemeSetInMotion
    // trigger. If `set_in_motion` were also self-collecting (the reverted bug),
    // the trigger would be collected TWICE and the count below would be 2.
    use crate::types::actions::GameAction;
    use crate::types::phase::Phase;

    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.turn_number = 2; // not turn 1, so the Draw step is not skipped
    state.active_player = arch;
    seed_libraries(&mut state);

    let scheme = synthesized_scheme_face(vec![set_in_motion_trigger()], vec![], vec![]);
    let deck_ids = setup_scheme_deck(&mut state, arch, &[("Pipeline Scheme", &scheme)]);
    let scheme_id = deck_ids[0];

    // Park at the archenemy's Draw-step priority window so an all-pass advances
    // into PreCombatMain through `pass_priority_once_with_pipeline`.
    state.phase = Phase::Draw;
    state.priority_player = arch;
    state.waiting_for = WaitingFor::Priority { player: arch };
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    // Drive PassPriority for each living player until the step advances.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(
        state.phase,
        Phase::PreCombatMain,
        "all-pass on the Draw step advanced into PreCombatMain"
    );
    assert!(
        state.command_zone.contains(&scheme_id),
        "the top scheme was set in motion entering PreCombatMain"
    );
    // EXACTLY ONCE: stack + deferred combined hold a single instance.
    assert_eq!(
        scheme_trigger_instances(&state, scheme_id),
        1,
        "SchemeSetInMotion trigger collected exactly once, got {} (stack={:?}, deferred={:?})",
        scheme_trigger_instances(&state, scheme_id),
        state.stack,
        state.deferred_triggers,
    );
}

// ---------------------------------------------------------------------------
// 13. non-ongoing SBA is NOT an abandon action
// ---------------------------------------------------------------------------

#[test]
fn nonongoing_sba_does_not_fire_abandoned_trigger() {
    // DISCRIMINATING: CR 904.10 / CR 314.6 turn a non-ongoing scheme face down
    // and put it on the bottom of the scheme deck as an SBA, but CR 701.33a says
    // only face-up ongoing schemes can be abandoned. A non-ongoing scheme with a
    // synthetic Abandoned trigger must therefore NOT get `SchemeAbandoned` or a
    // deferred Abandoned trigger from the SBA path.
    use crate::types::actions::GameAction;
    use crate::types::phase::Phase;

    let mut state = GameState::new_two_player(7);
    let arch = PlayerId(0);
    state.turn_number = 2;
    state.active_player = arch;
    seed_libraries(&mut state);

    // Face-up non-ongoing scheme with an Abandoned trigger; stack + deferred empty.
    let scheme = synthesized_scheme_face(vec![abandoned_trigger()], vec![], vec![]);
    let scheme_id = setup_active_scheme(&mut state, arch, "Pipeline Abandon Scheme", &scheme);
    assert!(state.stack.is_empty() && state.deferred_triggers.is_empty());

    // Park at a Priority window so the pipeline's SBA loop runs the abandon SBA.
    state.phase = Phase::PreCombatMain;
    state.priority_player = arch;
    state.waiting_for = WaitingFor::Priority { player: arch };
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    // A single PassPriority drives `run_post_action_pipeline`, whose SBA loop
    // turns the face-up non-ongoing scheme down.
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // The scheme was turned face down, out of the command zone, on the bottom of
    // the scheme deck.
    assert!(
        state.objects.get(&scheme_id).unwrap().face_down,
        "abandoned scheme is face down"
    );
    assert!(
        !state.command_zone.contains(&scheme_id),
        "abandoned scheme left the command zone"
    );
    assert_eq!(
        state.scheme_deck.back().copied(),
        Some(scheme_id),
        "abandoned scheme is on the bottom of the scheme deck"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::SchemeAbandoned { .. })),
        "non-ongoing SBA is not an abandon action"
    );
    // NOT abandoned: no SchemeAbandoned trigger/event was collected.
    assert_eq!(
        scheme_trigger_instances(&state, scheme_id),
        0,
        "non-ongoing SBA must not collect an Abandoned trigger, got {} (stack={:?}, deferred={:?})",
        scheme_trigger_instances(&state, scheme_id),
        state.stack,
        state.deferred_triggers,
    );
}
