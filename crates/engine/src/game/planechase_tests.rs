//! Tests for the Planechase runtime (CR 901). Declared from `game/mod.rs` so
//! `planechase.rs` stays implementation-only (no inline tests).
//!
//! These drive the real pipeline: planeswalk/chaos/SBA functions emit events,
//! the trigger machinery (`process_triggers` / `collect_triggers_into_deferred`
//! / `drain_deferred_trigger_queue`) collects and dispatches them, and assertions
//! check the resulting stack / deferred-queue / event output. Several tests are
//! deliberately discriminating: they fail if the corresponding fix is reverted.

use super::planechase::{
    active_plane, chaos_ensues, check_phenomenon_planeswalk_sba, planar_ability_sentinel_id,
    planeswalk, roll_planar_die, set_planar_controller, PlanarDieFace,
};
use super::triggers::process_triggers;
use crate::database::synthesis::synthesize_planechase;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, StaticDefinition,
    TargetFilter, TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::format::FormatConfig;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use std::str::FromStr;

/// Build a `CardFace` for a plane/phenomenon carrying the given triggers and
/// statics, then run `synthesize_planechase` (the production stamping step) so
/// the trigger/static zones reflect the real card-build path.
fn synthesized_planar_face(
    core: CoreType,
    triggers: Vec<TriggerDefinition>,
    statics: Vec<StaticDefinition>,
) -> CardFace {
    let mut face = CardFace::default();
    face.card_type.core_types.push(core);
    face.triggers = triggers;
    face.static_abilities = statics;
    synthesize_planechase(&mut face);
    face
}

/// A `PlaneswalkedFrom` trigger that draws a card (its `valid_target` is
/// `Controller`, like the parser emits).
fn planeswalked_from_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::PlaneswalkedFrom)
        .valid_card(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Controller)
        .execute(draw_ability())
}

/// A `PlaneswalkedTo` trigger that draws a card.
fn planeswalked_to_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::PlaneswalkedTo)
        .valid_card(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Controller)
        .execute(draw_ability())
}

/// A `ChaosEnsues` trigger that draws a card.
fn chaos_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChaosEnsues)
        .valid_card(TargetFilter::SelfRef)
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

/// Create a planar object directly in `state.objects`, applying its synthesized
/// trigger/static definitions and setting its controller. Returns its id. The
/// object is NOT placed in any zone vector — the caller decides command zone vs
/// planar deck.
fn create_planar_object(
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

/// Place a Planechase game: `active` face up in the command zone, `deck` (front
/// = top) in the planar deck, controller set. Returns (active_id, deck_ids).
fn setup_planechase(
    state: &mut GameState,
    controller: PlayerId,
    active: (&str, &CardFace),
    deck: &[(&str, &CardFace)],
) -> (ObjectId, Vec<ObjectId>) {
    let active_id = create_planar_object(state, active.0, active.1, controller);
    state.command_zone.push_back(active_id);
    let mut deck_ids = Vec::new();
    for (name, face) in deck {
        let id = create_planar_object(state, name, face, controller);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.face_down = true;
        }
        state.planar_deck.push_back(id);
        deck_ids.push(id);
    }
    state.planar_controller = Some(controller);
    (active_id, deck_ids)
}

// ---------------------------------------------------------------------------
// 1. CoreType round-trip
// ---------------------------------------------------------------------------

#[test]
fn coretype_plane_phenomenon_roundtrip() {
    // CR 311 / CR 312: Plane and Phenomenon are nontraditional, non-permanent
    // card types that offer no protection quality.
    for ct in [CoreType::Plane, CoreType::Phenomenon] {
        let s = ct.to_string();
        assert_eq!(CoreType::from_str(&s), Ok(ct), "round-trip {s}");
        // CR 311.2 / CR 312.2: not permanent types.
        assert!(!ct.is_permanent_type(), "{s} must not be a permanent type");
        assert_eq!(ct.protection_quality_str(), None, "{s} has no protection");
    }
    assert_eq!(CoreType::Plane.to_string(), "Plane");
    assert_eq!(CoreType::Phenomenon.to_string(), "Phenomenon");
}

#[test]
fn planar_die_legality_uses_semantic_priority_seat_under_turn_control() {
    // CR 901.9 / CR 116.2i: the active player may roll the planar die as a
    // special action while they have priority. CR 723 turn-control effects can
    // route the authorized submitter through another player, but the rules
    // legality remains attached to the controlled active seat.
    let mut state = GameState::new_two_player(7);
    let controller = PlayerId(0);
    let controlled = PlayerId(1);
    state.format_config = FormatConfig::planechase();
    state.active_player = controlled;
    state.priority_player = controller;
    state.turn_decision_controller = Some(controller);
    state.waiting_for = crate::types::game_state::WaitingFor::Priority { player: controlled };
    state.phase = Phase::PreCombatMain;

    let plane = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    setup_planechase(
        &mut state,
        controlled,
        ("Controlled Turn Plane", &plane),
        &[],
    );

    assert!(
        super::planechase::can_roll_planar_die(&state, controlled),
        "controlled active seat should satisfy planar-die rules legality"
    );
    assert!(
        !super::planechase::can_roll_planar_die(&state, controller),
        "authorized submitter is transport authority, not the planar-die rules seat"
    );
}

// ---------------------------------------------------------------------------
// 2. Planeswalk rotates the deck and command zone
// ---------------------------------------------------------------------------

#[test]
fn planeswalk_rotates_deck_and_command_zone() {
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let plane_a = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let plane_b = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (active_id, deck_ids) = setup_planechase(
        &mut state,
        p0,
        ("Plane A", &plane_a),
        &[("Plane B", &plane_b)],
    );
    let next_id = deck_ids[0];

    let mut events = Vec::new();
    planeswalk(&mut state, p0, &mut events);

    // CR 701.31b: the previously active plane is now the bottom of the planar
    // deck (face down), and the new top is face up in the command zone.
    assert_eq!(
        active_plane(&state),
        Some(next_id),
        "next plane is now active"
    );
    assert!(
        !state.command_zone.contains(&active_id),
        "old active left the command zone"
    );
    assert_eq!(
        state.planar_deck.back().copied(),
        Some(active_id),
        "old active is on the bottom of the planar deck"
    );
    assert!(
        state.objects.get(&active_id).unwrap().face_down,
        "departed plane is face down"
    );
    assert!(
        !state.objects.get(&next_id).unwrap().face_down,
        "arrived plane is face up"
    );
    // CR 701.31d: a Planeswalked event was emitted with both endpoints.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::Planeswalked { from: Some(f), to: Some(t), .. }
            if *f == active_id && *t == next_id
        )),
        "Planeswalked event with from/to endpoints emitted, got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. planeswalk-away trigger fires via the command-zone scan (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn planeswalk_away_trigger_fires_via_command_scan() {
    // DISCRIMINATING: fails if `synthesize_planechase` stops stamping
    // `trigger_zones = [Command]` (the command-zone scan would skip the plane's
    // departing trigger) or if the `PlaneswalkedFrom` matcher is reverted.
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let plane_a =
        synthesized_planar_face(CoreType::Plane, vec![planeswalked_from_trigger()], vec![]);
    let plane_b = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (active_id, _) = setup_planechase(
        &mut state,
        p0,
        ("Plane A", &plane_a),
        &[("Plane B", &plane_b)],
    );

    // Sanity: synthesis stamped the command zone onto the departing trigger.
    assert!(
        plane_a.triggers[0].trigger_zones.contains(&Zone::Command),
        "synthesize_planechase must stamp Zone::Command on the planeswalk-away trigger"
    );

    let mut events = Vec::new();
    planeswalk(&mut state, p0, &mut events);

    // The departing plane's trigger was collected (deferred), keyed to its source.
    assert!(
        state
            .deferred_triggers
            .iter()
            .any(|d| d.pending.source_id == active_id),
        "planeswalk-away trigger from {active_id:?} must be collected, got {:?}",
        state.deferred_triggers
    );
}

// ---------------------------------------------------------------------------
// 4. planeswalk-to trigger fires for the arriving plane
// ---------------------------------------------------------------------------

#[test]
fn planeswalk_to_trigger_fires() {
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let plane_a = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let plane_b = synthesized_planar_face(CoreType::Plane, vec![planeswalked_to_trigger()], vec![]);
    let (_active_id, deck_ids) = setup_planechase(
        &mut state,
        p0,
        ("Plane A", &plane_a),
        &[("Plane B", &plane_b)],
    );
    let arriving_id = deck_ids[0];

    let mut events = Vec::new();
    planeswalk(&mut state, p0, &mut events);

    assert!(
        state
            .deferred_triggers
            .iter()
            .any(|d| d.pending.source_id == arriving_id),
        "planeswalk-to trigger from {arriving_id:?} must be collected, got {:?}",
        state.deferred_triggers
    );
}

// ---------------------------------------------------------------------------
// 5. chaos ensues fires the plane's chaos trigger (DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn chaos_ensues_fires_plane_chaos_trigger() {
    // DISCRIMINATING: fails if `match_chaos_ensues` is reverted to the
    // unimplemented stub (the trigger would never reach the stack).
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    state.active_player = p0;
    let plane = synthesized_planar_face(CoreType::Plane, vec![chaos_trigger()], vec![]);
    let (plane_id, _) = setup_planechase(&mut state, p0, ("Chaos Plane", &plane), &[]);

    let stack_before = state.stack.len();
    let mut events = Vec::new();
    chaos_ensues(&mut state, &mut events);
    // The active plane stays in the command zone for chaos; the normal trigger
    // pass scans it there.
    process_triggers(&mut state, &events);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::ChaosEnsued { plane_id: id } if *id == plane_id)),
        "ChaosEnsued event keyed to the active plane emitted"
    );
    assert_eq!(
        state.stack.len(),
        stack_before + 1,
        "chaos trigger must reach the stack"
    );
}

// ---------------------------------------------------------------------------
// 6. planar die distribution 4/1/1 (B2 guard, DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn planar_die_distribution_4_1_1() {
    // DISCRIMINATING: fails if the 1->Planeswalk / 2->Chaos / 3..=6->Blank
    // mapping is reverted. We drive the RNG to each face value by seeding so the
    // first roll lands on each of 1..=6, and assert the resulting face.
    //
    // CR 901.3a: one Planeswalker face, one chaos face, four blank faces.
    // We exercise the mapping directly by counting outcomes over many rolls and
    // asserting the 4/1/1 ratio holds (Planeswalk and Chaos each ~1/6, Blank ~4/6).
    let p0 = PlayerId(0);
    let mut counts = [0usize; 3]; // [planeswalk, chaos, blank]
    let rolls = 6000;
    for seed in 0..rolls {
        let mut state = GameState::new_two_player(seed);
        // No planar deck / active plane: planeswalk and chaos become no-ops, so
        // only the rolled face matters here.
        state.planar_controller = Some(p0);
        let mut events = Vec::new();
        match roll_planar_die(&mut state, p0, &mut events) {
            PlanarDieFace::Planeswalk => counts[0] += 1,
            PlanarDieFace::Chaos => counts[1] += 1,
            PlanarDieFace::Blank => counts[2] += 1,
        }
        // CR 901.9d: a PlanarDieRolled event is always emitted.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::PlanarDieRolled { .. })),
            "PlanarDieRolled event must be emitted"
        );
        // CR 901.9d / CR 706.7: the planar roll ALSO emits a sides-less,
        // result-less generic DieRolled so "whenever a player rolls one or more
        // dice" abilities (TriggerMode::RolledDie) fire, while numeric-result
        // consumers ignore the `None` result.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::DieRolled {
                    sides: 6,
                    result: None,
                    ..
                }
            )),
            "planar die must emit DieRolled {{ sides: 6, result: None }} (CR 901.9d)"
        );
    }
    // Planeswalk and Chaos should each be near 1/6 of rolls; Blank near 4/6.
    // Use loose bounds to avoid flakiness while still catching a broken mapping.
    let n = rolls as f64;
    let pw = counts[0] as f64 / n;
    let chaos = counts[1] as f64 / n;
    let blank = counts[2] as f64 / n;
    assert!(
        (0.12..0.21).contains(&pw),
        "Planeswalk ~1/6, got {pw} ({})",
        counts[0]
    );
    assert!(
        (0.12..0.21).contains(&chaos),
        "Chaos ~1/6, got {chaos} ({})",
        counts[1]
    );
    assert!(
        (0.62..0.71).contains(&blank),
        "Blank ~4/6, got {blank} ({})",
        counts[2]
    );
}

// ---------------------------------------------------------------------------
// 7. phenomenon encounter then SBA planeswalk (CR 704.6f)
// ---------------------------------------------------------------------------

#[test]
fn phenomenon_encounter_then_sba_planeswalk() {
    // CR 704.6f: a face-up phenomenon planeswalks (via SBA) once its triggered
    // ability leaves the stack. While the ability is on the stack, the SBA does
    // nothing.
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let phenom = synthesized_planar_face(CoreType::Phenomenon, vec![], vec![]);
    let next_plane = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (phenom_id, deck_ids) = setup_planechase(
        &mut state,
        p0,
        ("Phenom", &phenom),
        &[("Next Plane", &next_plane)],
    );
    let next_id = deck_ids[0];

    // Put a triggered ability sourced from the phenomenon onto the stack.
    state.stack.push_back(StackEntry {
        id: ObjectId(99_999),
        source_id: phenom_id,
        controller: p0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: phenom_id,
            ability: Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                phenom_id,
                p0,
            )),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });

    // SBA should NOT planeswalk while the phenomenon's ability is on the stack.
    let mut events = Vec::new();
    let mut any = false;
    check_phenomenon_planeswalk_sba(&mut state, &mut events, &mut any);
    assert!(
        !any,
        "no SBA planeswalk while the phenomenon's ability is on the stack"
    );
    assert_eq!(
        active_plane(&state),
        Some(phenom_id),
        "phenomenon still active"
    );

    // Clear the stack: now the SBA must planeswalk.
    state.stack.clear();
    let mut events2 = Vec::new();
    let mut any2 = false;
    check_phenomenon_planeswalk_sba(&mut state, &mut events2, &mut any2);
    assert!(any2, "SBA planeswalks once the ability leaves the stack");
    assert_eq!(
        active_plane(&state),
        Some(next_id),
        "planeswalked to next plane"
    );
}

// ---------------------------------------------------------------------------
// 8. active static applies only while active (S1 guard, DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn active_static_applies_only_while_active() {
    // DISCRIMINATING: fails if `synthesize_planechase` stops stamping
    // `active_zones = [Command]` onto the plane's static (the command-zone static
    // scan would never include it, so it would never apply even while active).
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let mut plane_static = StaticDefinition::new(StaticMode::Continuous);
    plane_static.description = Some("plane-static-marker".to_string());
    let plane_a = synthesized_planar_face(CoreType::Plane, vec![], vec![plane_static]);
    let plane_b = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (active_id, _) = setup_planechase(
        &mut state,
        p0,
        ("Plane A", &plane_a),
        &[("Plane B", &plane_b)],
    );

    // Sanity: synthesis stamped the command zone onto the static.
    assert!(
        plane_a.static_abilities[0]
            .active_zones
            .contains(&Zone::Command),
        "synthesize_planechase must stamp Zone::Command on the plane static"
    );

    // While active (in the command zone) the static is yielded by the real
    // game-scope static scan.
    let active_present = crate::game::functioning_abilities::game_active_statics(&state)
        .any(|(obj, _)| obj.id == active_id);
    assert!(
        active_present,
        "plane static must apply while the plane is the active command-zone card"
    );

    // After planeswalking away, the plane is no longer in the command zone, so
    // its static no longer applies.
    let mut events = Vec::new();
    planeswalk(&mut state, p0, &mut events);
    let still_present = crate::game::functioning_abilities::game_active_statics(&state)
        .any(|(obj, _)| obj.id == active_id);
    assert!(
        !still_present,
        "plane static must NOT apply after planeswalking away from it"
    );
}

// ---------------------------------------------------------------------------
// 9. planar_controller = None skips all Planechase SBA work
// ---------------------------------------------------------------------------

#[test]
fn planar_controller_none_skips_sba() {
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let phenom = synthesized_planar_face(CoreType::Phenomenon, vec![], vec![]);
    let next_plane = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (phenom_id, _) = setup_planechase(
        &mut state,
        p0,
        ("Phenom", &phenom),
        &[("Next Plane", &next_plane)],
    );
    // Clear the controller — not a Planechase game.
    state.planar_controller = None;

    let mut events = Vec::new();
    let mut any = false;
    check_phenomenon_planeswalk_sba(&mut state, &mut events, &mut any);
    assert!(
        !any,
        "no Planechase SBA work when planar_controller is None"
    );
    assert_eq!(
        active_plane(&state),
        Some(phenom_id),
        "phenomenon untouched when there is no planar controller"
    );
    assert!(
        events.is_empty(),
        "no events when planar_controller is None"
    );
}

// ---------------------------------------------------------------------------
// 10. empty-planar-deck planeswalk keeps the active plane (CR 701.31b, DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn empty_deck_planeswalk_keeps_active_plane() {
    // DISCRIMINATING: fails if `planeswalk` reverts to popping the (empty) deck,
    // leaving `to = None` and rotating the sole plane into the deck — which would
    // empty the command zone of any active plane. CR 701.31b: with only the
    // active card present, putting it on the bottom makes it the new top again,
    // so the same plane stays active (a self-planeswalk).
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let plane_a = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    // Empty planar deck — only the active plane exists.
    let (active_id, _) = setup_planechase(&mut state, p0, ("Lone Plane", &plane_a), &[]);

    let mut events = Vec::new();
    planeswalk(&mut state, p0, &mut events);

    // The active plane must still be active (and face up) — not lost.
    assert_eq!(
        active_plane(&state),
        Some(active_id),
        "the sole plane must remain active after a planeswalk with an empty deck"
    );
    assert!(
        state.command_zone.contains(&active_id),
        "the sole plane must stay in the command zone"
    );
    assert!(
        !state.objects.get(&active_id).unwrap().face_down,
        "the sole plane must remain face up after the self-planeswalk"
    );
    // The deck must not have gained a stray copy of the plane.
    assert!(
        !state.planar_deck.contains(&active_id),
        "self-planeswalk must not push the active plane into the planar deck"
    );
    // CR 701.31d: the Planeswalked event reports the same card as from and to.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::Planeswalked { from: Some(f), to: Some(t), .. }
            if *f == active_id && *t == active_id
        )),
        "self-planeswalk must announce from == to, got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// 11. synthesize_planechase appends Command, preserving pre-existing zones
// ---------------------------------------------------------------------------

#[test]
fn synthesize_planechase_appends_command_zone() {
    // Guard for Finding 3: `synthesize_planechase` must PUSH Zone::Command onto
    // any pre-existing zone list, not overwrite it. A trigger/static that already
    // designated another zone must keep it and gain Command.
    let mut trigger = TriggerDefinition::new(TriggerMode::PlaneswalkedFrom);
    trigger.trigger_zones = vec![Zone::Exile];
    let mut static_def = StaticDefinition::new(StaticMode::Continuous);
    static_def.active_zones = vec![Zone::Exile];

    let face = synthesized_planar_face(CoreType::Plane, vec![trigger], vec![static_def]);

    assert!(
        face.triggers[0].trigger_zones.contains(&Zone::Exile)
            && face.triggers[0].trigger_zones.contains(&Zone::Command),
        "pre-existing trigger zone must be preserved and Command appended, got {:?}",
        face.triggers[0].trigger_zones
    );
    assert!(
        face.static_abilities[0].active_zones.contains(&Zone::Exile)
            && face.static_abilities[0]
                .active_zones
                .contains(&Zone::Command),
        "pre-existing static zone must be preserved and Command appended, got {:?}",
        face.static_abilities[0].active_zones
    );

    // Idempotent: running synthesis again does not duplicate Command.
    let mut face2 = face;
    synthesize_planechase(&mut face2);
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

/// Roll the planar die under fresh seeds until the Planeswalker face comes up,
/// returning the seed whose FIRST `roll_planar_die` lands on Planeswalk. Keeps
/// the discriminating planeswalk tests deterministic without hard-coding an RNG
/// internal.
fn seed_yielding_planeswalk() -> u64 {
    for seed in 0..10_000 {
        let mut probe = GameState::new_two_player(seed);
        probe.planar_controller = Some(PlayerId(0));
        let mut events = Vec::new();
        if roll_planar_die(&mut probe, PlayerId(0), &mut events) == PlanarDieFace::Planeswalk {
            return seed;
        }
    }
    panic!("no seed produced a Planeswalk face within the search bound");
}

// ---------------------------------------------------------------------------
// 13. Planeswalk die face puts the planeswalking ability ON THE STACK
//     (CR 901.8 / CR 901.9c, DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn planeswalk_die_face_queues_ability_on_stack() {
    // DISCRIMINATING: fails if `roll_planar_die`'s Planeswalk arm is reverted to
    // calling `planeswalk(...)` inline. CR 901.9c: the planeswalking ability is
    // put on the stack and resolves at the next priority — so immediately after
    // the roll the active plane is UNCHANGED and a stack entry sourced from the
    // synthetic planar-ability sentinel exists. An inline revert would have
    // already planeswalked (active plane == next plane) with nothing on the
    // stack.
    let p0 = PlayerId(0);
    let seed = seed_yielding_planeswalk();
    let mut state = GameState::new_two_player(seed);
    state.active_player = p0;
    let plane_a = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let plane_b = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (active_id, deck_ids) = setup_planechase(
        &mut state,
        p0,
        ("Plane A", &plane_a),
        &[("Plane B", &plane_b)],
    );
    let next_id = deck_ids[0];

    let mut events = Vec::new();
    let face = roll_planar_die(&mut state, p0, &mut events);
    assert_eq!(face, PlanarDieFace::Planeswalk, "seed must roll Planeswalk");

    // CR 901.9c: ability on the stack, sourced from the planar-ability sentinel
    // controlled by the roller — and NOT yet resolved (active plane unchanged).
    let sentinel = planar_ability_sentinel_id(p0);
    assert!(
        state.stack.iter().any(|e| e.source_id == sentinel),
        "planeswalking ability must be on the stack sourced from {sentinel:?}, got {:?}",
        state.stack
    );
    assert_eq!(
        active_plane(&state),
        Some(active_id),
        "active plane must be UNCHANGED before the ability resolves (not inline)"
    );

    // Resolve the planeswalking ability: now the planeswalk actually happens.
    let ability = ResolvedAbility::new(Effect::Planeswalk, vec![], sentinel, p0);
    let mut resolve_events = Vec::new();
    crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut resolve_events, 0)
        .expect("planeswalking ability resolves");
    assert_eq!(
        active_plane(&state),
        Some(next_id),
        "after the ability resolves, the planeswalk promotes the next plane"
    );
}

// ---------------------------------------------------------------------------
// 14. Generic RolledDie trigger fires on the planar die (CR 901.9d / CR 706.7)
// ---------------------------------------------------------------------------

#[test]
fn planar_die_fires_generic_rolled_die_trigger() {
    // CR 901.9d / CR 706.7: rolling the planar die fires "whenever a player rolls
    // one or more dice" (TriggerMode::RolledDie). A roller-controlled object with
    // such a trigger (no die_sides filter) must match the emitted DieRolled.
    let p0 = PlayerId(0);
    let mut state = GameState::new_two_player(7);
    state.active_player = p0;
    let plane = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (_active_id, _) = setup_planechase(&mut state, p0, ("Plane", &plane), &[]);

    // A roller-controlled source carrying a generic RolledDie trigger.
    let source_id = create_planar_object(&mut state, "Roller Source", &plane, p0);
    let trigger =
        TriggerDefinition::new(TriggerMode::RolledDie).valid_target(TargetFilter::Controller);

    let mut events = Vec::new();
    roll_planar_die(&mut state, p0, &mut events);

    let die_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::DieRolled { .. }))
        .expect("planar roll must emit a generic DieRolled event");
    assert!(
        super::trigger_matchers::match_rolled_die(die_event, &trigger, source_id, &state),
        "RolledDie trigger (no die_sides) must fire on the planar die roll"
    );
}

// ---------------------------------------------------------------------------
// 15. die_sides filter boundary on the planar die (CR 901.3a / CR 706.7)
// ---------------------------------------------------------------------------

#[test]
fn planar_die_sides_filter_boundary() {
    // CR 901.3a: the planar die has six faces, so a `die_sides: Some(6)` trigger
    // matches it; CR 706.7: a `Some(20)` trigger does not (intended boundary —
    // the planar roll is reported as a 6-sided die).
    let p0 = PlayerId(0);
    let mut state = GameState::new_two_player(7);
    state.active_player = p0;
    let plane = synthesized_planar_face(CoreType::Plane, vec![], vec![]);
    let (_active_id, _) = setup_planechase(&mut state, p0, ("Plane", &plane), &[]);
    let source_id = create_planar_object(&mut state, "Roller Source", &plane, p0);

    let mut events = Vec::new();
    roll_planar_die(&mut state, p0, &mut events);
    let die_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::DieRolled { .. }))
        .expect("planar roll must emit a generic DieRolled event");

    let mut d6_trigger =
        TriggerDefinition::new(TriggerMode::RolledDie).valid_target(TargetFilter::Controller);
    d6_trigger.die_sides = Some(6);
    assert!(
        super::trigger_matchers::match_rolled_die(die_event, &d6_trigger, source_id, &state),
        "die_sides: Some(6) must match the six-faced planar die (CR 901.3a)"
    );

    let mut d20_trigger =
        TriggerDefinition::new(TriggerMode::RolledDie).valid_target(TargetFilter::Controller);
    d20_trigger.die_sides = Some(20);
    assert!(
        !super::trigger_matchers::match_rolled_die(die_event, &d20_trigger, source_id, &state),
        "die_sides: Some(20) must NOT match the planar die (CR 706.7 boundary)"
    );
}

// ---------------------------------------------------------------------------
// 16. start_next_turn syncs the planar controller to the new active player
//     (CR 311.5 / CR 312.4 / CR 901.6, DISCRIMINATING)
// ---------------------------------------------------------------------------

#[test]
fn start_next_turn_syncs_planar_controller() {
    // DISCRIMINATING: fails if the `set_planar_controller` call is removed from
    // `start_next_turn`. CR 311.5: the planar controller is normally the active
    // player; advancing the turn must move both `planar_controller` and the
    // active plane's `.controller` to the new active player, so the plane's
    // "you"-scoped trigger now matches the NEW controller.
    let mut state = GameState::new_two_player(7);
    let p0 = PlayerId(0);
    let p1 = PlayerId(1);
    state.active_player = p0;
    let plane = synthesized_planar_face(CoreType::Plane, vec![planeswalked_to_trigger()], vec![]);
    let (active_id, _) = setup_planechase(&mut state, p0, ("Plane", &plane), &[]);
    // Active plane initially controlled by p0.
    assert_eq!(
        state.objects.get(&active_id).map(|o| o.controller),
        Some(p0)
    );

    // Advance the turn — p1 becomes active.
    let mut events = Vec::new();
    crate::game::turns::start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, p1, "p1 is now the active player");

    assert_eq!(
        state.planar_controller,
        Some(p1),
        "planar controller follows the active player (CR 311.5)"
    );
    assert_eq!(
        state.objects.get(&active_id).map(|o| o.controller),
        Some(p1),
        "active plane's controller is synced to the new planar controller"
    );

    // The active plane's "you"-scoped (Controller) trigger now matches p1, not p0.
    let trig = &state.objects.get(&active_id).unwrap().trigger_definitions[0];
    assert!(
        super::trigger_matchers::valid_player_matches(trig, &state, p1, active_id),
        "Controller-scoped trigger must now match the NEW controller p1"
    );
    assert!(
        !super::trigger_matchers::valid_player_matches(trig, &state, p0, active_id),
        "Controller-scoped trigger must no longer match the OLD controller p0"
    );
}

// ---------------------------------------------------------------------------
// 17. set_planar_controller is a no-op outside a Planechase game
// ---------------------------------------------------------------------------

#[test]
fn set_planar_controller_noop_outside_planechase() {
    let mut state = GameState::new_two_player(7);
    // Not a Planechase game: no planar controller, empty deck, no active plane.
    assert!(state.planar_controller.is_none());
    assert!(state.planar_deck.is_empty());
    assert!(active_plane(&state).is_none());

    let mut events = Vec::new();
    set_planar_controller(&mut state, PlayerId(1), &mut events);
    assert!(
        state.planar_controller.is_none(),
        "set_planar_controller must not designate a controller outside Planechase"
    );
    assert!(events.is_empty(), "no events outside a Planechase game");
}
