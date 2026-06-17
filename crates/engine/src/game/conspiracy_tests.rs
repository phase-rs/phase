//! Tests for the Conspiracy runtime (CR 905 / CR 702.106). Declared from
//! `game/mod.rs` so `conspiracy.rs` stays implementation-only (no inline tests).
//!
//! These exercise the building blocks directly: the `CoreType::Conspiracy`
//! round-trip, the command-zone source predicate, the game-start placement
//! (face up vs hidden-agenda face down), the controller=owner reseat (CR 905.5),
//! the hidden-agenda reveal (CR 905.4a + CR 702.106), the `synthesize_conspiracy`
//! zone stamping (CR 113.6b), and the static-source-index inclusion that makes a
//! face-up conspiracy's command-zone continuous statics gather like an emblem's.

use super::conspiracy::{
    conspiracies_in_command_zone, functions_from_command_zone, is_conspiracy,
    start_with_conspiracy, turn_hidden_agenda_face_up,
};
use super::functioning_abilities::{
    active_static_definitions, active_trigger_definitions, game_functioning_statics,
};
use crate::database::synthesis::synthesize_conspiracy;
use crate::types::ability::{StaticDefinition, TriggerDefinition};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, StaticSourceIndex};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use std::str::FromStr;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Build a `CardFace` for a conspiracy carrying the given statics/triggers, then
/// run `synthesize_conspiracy` (the production stamping step) so the
/// trigger/static zones reflect the real card-build path.
fn synthesized_conspiracy_face(
    statics: Vec<StaticDefinition>,
    triggers: Vec<TriggerDefinition>,
) -> CardFace {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Conspiracy);
    face.static_abilities = statics;
    face.triggers = triggers;
    synthesize_conspiracy(&mut face);
    face
}

/// Create a conspiracy object owned by `owner`, applying a synthesized face's
/// definitions. The object is created in `state.objects` only — NOT placed in
/// any zone vector; the caller (or `start_with_conspiracy`) decides placement.
/// `face_down` and `controller` are left at the `GameObject::new` defaults
/// (face up, controller == owner) so individual tests can override them.
fn create_conspiracy_object(
    state: &mut GameState,
    name: &str,
    face: &CardFace,
    owner: PlayerId,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let mut obj = crate::game::game_object::GameObject::new(
        id,
        CardId(id.0),
        owner,
        name.to_string(),
        Zone::Command,
    );
    obj.card_types = face.card_type.clone();
    for st in &face.static_abilities {
        obj.static_definitions.push(st.clone());
    }
    for trig in &face.triggers {
        obj.trigger_definitions.push(trig.clone());
    }
    state.objects.insert(id, obj);
    id
}

/// A vanilla face-up command-zone continuous static (no extra zone designation
/// beyond what `synthesize_conspiracy` stamps).
fn continuous_static() -> StaticDefinition {
    StaticDefinition::new(StaticMode::Continuous)
}

// ---------------------------------------------------------------------------
// CoreType round-trip
// ---------------------------------------------------------------------------

#[test]
fn coretype_conspiracy_roundtrip() {
    // CR 905: Conspiracy is a nontraditional, non-permanent card type that
    // offers no protection quality.
    assert_eq!(CoreType::Conspiracy.to_string(), "Conspiracy");
    assert_eq!(CoreType::from_str("Conspiracy"), Ok(CoreType::Conspiracy));
    assert_eq!(CoreType::Conspiracy.protection_quality_str(), None);
    assert!(!CoreType::Conspiracy.is_permanent_type());

    // serde round-trip.
    let json = serde_json::to_string(&CoreType::Conspiracy).unwrap();
    let back: CoreType = serde_json::from_str(&json).unwrap();
    assert_eq!(back, CoreType::Conspiracy);
}

// ---------------------------------------------------------------------------
// Identification + command-zone source predicate
// ---------------------------------------------------------------------------

#[test]
fn is_conspiracy_identifies_conspiracy_cards() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let consp = create_conspiracy_object(&mut state, "Worldknit", &face, P0);
    assert!(is_conspiracy(state.objects.get(&consp).unwrap()));

    let mut creature_face = CardFace::default();
    creature_face.card_type.core_types.push(CoreType::Creature);
    let creature = create_conspiracy_object(&mut state, "Grizzly Bears", &creature_face, P0);
    assert!(!is_conspiracy(state.objects.get(&creature).unwrap()));
}

#[test]
fn functions_from_command_zone_face_up_only() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let id = create_conspiracy_object(&mut state, "Worldknit", &face, P0);

    // Face up in the command zone → functions (CR 905.4).
    state.command_zone.push_back(id);
    assert!(functions_from_command_zone(state.objects.get(&id).unwrap()));

    // Face down (hidden agenda, CR 905.4a) → does not yet function.
    state.objects.get_mut(&id).unwrap().face_down = true;
    assert!(!functions_from_command_zone(
        state.objects.get(&id).unwrap()
    ));

    // Not in the command zone → does not function from it.
    state.objects.get_mut(&id).unwrap().face_down = false;
    state.objects.get_mut(&id).unwrap().zone = Zone::Battlefield;
    assert!(!functions_from_command_zone(
        state.objects.get(&id).unwrap()
    ));

    // Non-conspiracy command-zone object (e.g. a commander) → not a conspiracy
    // source.
    let mut cmdr_face = CardFace::default();
    cmdr_face.card_type.core_types.push(CoreType::Creature);
    let cmdr = create_conspiracy_object(&mut state, "Commander", &cmdr_face, P0);
    assert!(!functions_from_command_zone(
        state.objects.get(&cmdr).unwrap()
    ));
}

// ---------------------------------------------------------------------------
// Game-start placement (CR 905.4 / CR 905.4a / CR 905.5)
// ---------------------------------------------------------------------------

#[test]
fn start_with_conspiracy_face_up_reseats_controller_to_owner() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let id = create_conspiracy_object(&mut state, "Power Play", &face, P0);
    // Deliberately set a wrong controller to prove CR 905.5 reseats it.
    state.objects.get_mut(&id).unwrap().controller = P1;

    start_with_conspiracy(&mut state, id, false);

    let obj = state.objects.get(&id).unwrap();
    assert_eq!(obj.zone, Zone::Command);
    assert!(!obj.face_down, "CR 905.4: non-hidden-agenda starts face up");
    assert_eq!(obj.controller, P0, "CR 905.5: owner is the controller");
    assert!(state.command_zone.contains(&id));
}

#[test]
fn start_with_conspiracy_hidden_agenda_starts_face_down() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let id = create_conspiracy_object(&mut state, "Secret Summoning", &face, P0);

    start_with_conspiracy(&mut state, id, true);

    let obj = state.objects.get(&id).unwrap();
    assert_eq!(obj.zone, Zone::Command);
    assert!(obj.face_down, "CR 905.4a: hidden agenda starts face down");
    assert!(state.command_zone.contains(&id));
}

#[test]
fn start_with_conspiracy_does_not_duplicate_command_zone_entry() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let id = create_conspiracy_object(&mut state, "Worldknit", &face, P0);

    start_with_conspiracy(&mut state, id, false);
    start_with_conspiracy(&mut state, id, false);

    assert_eq!(state.command_zone.iter().filter(|&&x| x == id).count(), 1);
}

// ---------------------------------------------------------------------------
// Listing controlled, functioning conspiracies (CR 905.4 / CR 905.5)
// ---------------------------------------------------------------------------

#[test]
fn conspiracies_in_command_zone_filters_face_down_and_other_controllers() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);

    let up = create_conspiracy_object(&mut state, "Worldknit", &face, P0);
    start_with_conspiracy(&mut state, up, false);

    let hidden = create_conspiracy_object(&mut state, "Secret Summoning", &face, P0);
    start_with_conspiracy(&mut state, hidden, true);

    let opponent = create_conspiracy_object(&mut state, "Power Play", &face, P1);
    start_with_conspiracy(&mut state, opponent, false);

    let p0 = conspiracies_in_command_zone(&state, P0);
    assert_eq!(p0, vec![up], "only P0's face-up conspiracy");

    let p1 = conspiracies_in_command_zone(&state, P1);
    assert_eq!(p1, vec![opponent]);
}

// ---------------------------------------------------------------------------
// Hidden-agenda reveal (CR 905.4a + CR 702.106)
// ---------------------------------------------------------------------------

#[test]
fn turn_hidden_agenda_face_up_reveals_and_is_guarded() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![], vec![]);
    let id = create_conspiracy_object(&mut state, "Secret Summoning", &face, P0);
    start_with_conspiracy(&mut state, id, true);

    // An opponent cannot turn it face up.
    assert!(!turn_hidden_agenda_face_up(&mut state, id, P1));
    assert!(state.objects.get(&id).unwrap().face_down);

    // The controller can.
    assert!(turn_hidden_agenda_face_up(&mut state, id, P0));
    assert!(!state.objects.get(&id).unwrap().face_down);

    // Already face up → no-op.
    assert!(!turn_hidden_agenda_face_up(&mut state, id, P0));
}

// ---------------------------------------------------------------------------
// Synthesis stamping (CR 113.6b)
// ---------------------------------------------------------------------------

#[test]
fn synthesize_conspiracy_stamps_command_zone_on_abilities() {
    let static_def = continuous_static();
    let trigger = TriggerDefinition::new(TriggerMode::SpellCast);
    let face = synthesized_conspiracy_face(vec![static_def], vec![trigger]);

    assert!(
        face.static_abilities[0]
            .active_zones
            .contains(&Zone::Command),
        "CR 113.6b: conspiracy statics function from the command zone"
    );
    assert!(
        face.triggers[0].trigger_zones.contains(&Zone::Command),
        "CR 113.6b: conspiracy triggers function from the command zone"
    );
}

#[test]
fn synthesize_conspiracy_ignores_non_conspiracy_faces() {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Creature);
    face.static_abilities = vec![continuous_static()];
    synthesize_conspiracy(&mut face);
    assert!(!face.static_abilities[0]
        .active_zones
        .contains(&Zone::Command));
}

#[test]
fn face_down_conspiracy_opt_in_abilities_do_not_function() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(
        vec![continuous_static()],
        vec![TriggerDefinition::new(TriggerMode::SpellCast)],
    );
    let id = create_conspiracy_object(&mut state, "Secret Summoning", &face, P0);
    start_with_conspiracy(&mut state, id, true);

    let obj = state.objects.get(&id).unwrap();
    assert_eq!(
        active_static_definitions(&state, obj).count(),
        0,
        "CR 905.4a: a face-down hidden-agenda conspiracy's statics do not function"
    );
    assert_eq!(
        active_trigger_definitions(&state, obj).count(),
        0,
        "CR 905.4a: a face-down hidden-agenda conspiracy's triggers do not function"
    );
    assert!(
        !game_functioning_statics(&state).any(|(source, _)| source.id == id),
        "the game-scope static iterator must also exclude face-down conspiracies"
    );
}

#[test]
fn face_up_conspiracy_opt_in_abilities_function() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(
        vec![continuous_static()],
        vec![TriggerDefinition::new(TriggerMode::SpellCast)],
    );
    let id = create_conspiracy_object(&mut state, "Worldknit", &face, P0);
    start_with_conspiracy(&mut state, id, false);

    let obj = state.objects.get(&id).unwrap();
    assert_eq!(active_static_definitions(&state, obj).count(), 1);
    assert_eq!(active_trigger_definitions(&state, obj).count(), 1);
    assert!(game_functioning_statics(&state).any(|(source, _)| source.id == id));
}

// ---------------------------------------------------------------------------
// Static-source-index inclusion — the layer-gather seam (CR 611.2 / CR 114.3)
// ---------------------------------------------------------------------------

#[test]
fn face_up_conspiracy_with_continuous_static_is_indexed_as_command_source() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![continuous_static()], vec![]);
    let id = create_conspiracy_object(&mut state, "Worldknit", &face, P0);
    start_with_conspiracy(&mut state, id, false);

    StaticSourceIndex::rebuild_from_state(&mut state);
    assert!(
        state.static_source_index.command_sources.contains(&id),
        "a face-up conspiracy that sources a continuous effect is a command-zone generator"
    );
}

#[test]
fn face_down_conspiracy_is_not_indexed_as_command_source() {
    let mut state = GameState::new_two_player(7);
    let face = synthesized_conspiracy_face(vec![continuous_static()], vec![]);
    let id = create_conspiracy_object(&mut state, "Secret Summoning", &face, P0);
    start_with_conspiracy(&mut state, id, true);

    StaticSourceIndex::rebuild_from_state(&mut state);
    assert!(
        !state.static_source_index.command_sources.contains(&id),
        "CR 905.4a: a face-down hidden-agenda conspiracy does not yet function"
    );
}
