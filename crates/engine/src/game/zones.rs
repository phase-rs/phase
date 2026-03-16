use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::game_object::GameObject;
use super::printed_cards::{apply_back_face_to_object, snapshot_object_face};

/// Allocate a new ObjectId, create a GameObject with defaults, insert into state.objects, and add to the specified zone.
pub fn create_object(
    state: &mut GameState,
    card_id: CardId,
    owner: PlayerId,
    name: String,
    zone: Zone,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let obj = GameObject::new(id, card_id, owner, name, zone);
    state.objects.insert(id, obj);
    add_to_zone(state, id, zone, owner);

    id
}

/// Move an object from its current zone to a destination zone, updating all bookkeeping and emitting a ZoneChanged event.
pub fn move_to_zone(
    state: &mut GameState,
    object_id: ObjectId,
    to: Zone,
    events: &mut Vec<GameEvent>,
) {
    // Commander zone redirection: when a commander would go to graveyard or exile,
    // redirect to the command zone instead
    let to = if state.format_config.command_zone
        && super::commander::should_redirect_to_command_zone(state, object_id, to)
    {
        Zone::Command
    } else {
        to
    };

    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;
    let owner = obj.owner;

    remove_from_zone(state, object_id, from, owner);
    add_to_zone(state, object_id, to, owner);

    let obj_mut = state.objects.get_mut(&object_id).unwrap();
    obj_mut.zone = to;

    // DFC Rule 711.8: transformed permanents revert to front face on zone change
    if obj_mut.transformed {
        if let Some(back_face) = obj_mut.back_face.clone() {
            let current_back = snapshot_object_face(obj_mut);
            apply_back_face_to_object(obj_mut, back_face);
            obj_mut.back_face = Some(current_back);
            obj_mut.transformed = false;
        }
    }

    // Track when objects enter the battlefield (for summoning sickness)
    if to == Zone::Battlefield {
        obj_mut.entered_battlefield_turn = Some(state.turn_number);
    }

    // Track descended: a permanent card was put into its owner's graveyard
    if to == Zone::Graveyard {
        use crate::types::card_type::CoreType;
        let is_permanent_card = obj_mut.card_types.core_types.iter().any(|ct| {
            matches!(
                ct,
                CoreType::Creature
                    | CoreType::Artifact
                    | CoreType::Enchantment
                    | CoreType::Planeswalker
                    | CoreType::Land
                    | CoreType::Battle
            )
        });
        if is_permanent_card {
            if let Some(player) = state.players.iter_mut().find(|p| p.id == owner) {
                player.descended_this_turn = true;
            }
        }
    }

    // Mark layers dirty when objects enter or leave the battlefield
    if from == Zone::Battlefield || to == Zone::Battlefield {
        state.layers_dirty = true;
    }

    // Prune host-bound transient effects when a permanent leaves the battlefield
    if from == Zone::Battlefield {
        super::layers::prune_host_left_effects(state, object_id);
    }

    super::restrictions::record_zone_change(state, object_id, from, to);

    events.push(GameEvent::ZoneChanged {
        object_id,
        from,
        to,
    });
}

/// Move an object to a specific position in its owner's library (top or bottom), emitting a ZoneChanged event.
/// Convention: library[0] = top of library.
pub fn move_to_library_position(
    state: &mut GameState,
    object_id: ObjectId,
    top: bool,
    events: &mut Vec<GameEvent>,
) {
    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;
    let owner = obj.owner;

    remove_from_zone(state, object_id, from, owner);

    // Place at top (index 0) or bottom (push to end)
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == owner)
        .expect("owner exists");
    if top {
        player.library.insert(0, object_id);
    } else {
        player.library.push(object_id);
    }

    state.objects.get_mut(&object_id).unwrap().zone = Zone::Library;

    events.push(GameEvent::ZoneChanged {
        object_id,
        from,
        to: Zone::Library,
    });
}

/// Remove an ObjectId from the appropriate zone collection.
pub fn remove_from_zone(state: &mut GameState, object_id: ObjectId, zone: Zone, owner: PlayerId) {
    match zone {
        Zone::Library | Zone::Hand | Zone::Graveyard => {
            let player = state
                .players
                .iter_mut()
                .find(|p| p.id == owner)
                .expect("owner exists");
            match zone {
                Zone::Library => player.library.retain(|id| *id != object_id),
                Zone::Hand => player.hand.retain(|id| *id != object_id),
                Zone::Graveyard => player.graveyard.retain(|id| *id != object_id),
                _ => unreachable!(),
            }
        }
        Zone::Battlefield => state.battlefield.retain(|id| *id != object_id),
        Zone::Stack => state.stack.retain(|e| e.id != object_id),
        Zone::Exile => state.exile.retain(|id| *id != object_id),
        Zone::Command => {} // Command zone not tracked as a collection in Phase 3
    }
}

/// Add an ObjectId to the appropriate zone collection.
pub fn add_to_zone(state: &mut GameState, object_id: ObjectId, zone: Zone, owner: PlayerId) {
    match zone {
        Zone::Library | Zone::Hand | Zone::Graveyard => {
            let player = state
                .players
                .iter_mut()
                .find(|p| p.id == owner)
                .expect("owner exists");
            match zone {
                Zone::Library => player.library.push(object_id),
                Zone::Hand => player.hand.push(object_id),
                Zone::Graveyard => player.graveyard.push(object_id),
                _ => unreachable!(),
            }
        }
        Zone::Battlefield => state.battlefield.push(object_id),
        Zone::Stack => {} // Stack entries are managed separately via StackEntry
        Zone::Exile => state.exile.push(object_id),
        Zone::Command => {} // Command zone not tracked as a collection in Phase 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::game_state::GameState;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn create_object_assigns_id_and_inserts() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        assert_eq!(id, ObjectId(1));
        assert!(state.objects.contains_key(&id));
        assert_eq!(state.objects[&id].name, "Forest");
        assert_eq!(state.objects[&id].zone, Zone::Hand);
        assert_eq!(state.next_object_id, 2);
    }

    #[test]
    fn create_object_adds_to_player_hand() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        assert!(state.players[0].hand.contains(&id));
    }

    #[test]
    fn create_object_adds_to_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn create_object_increments_id() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Hand,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Hand,
        );
        assert_eq!(id1, ObjectId(1));
        assert_eq!(id2, ObjectId(2));
    }

    #[test]
    fn move_hand_to_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        assert!(!state.players[0].hand.contains(&id));
        assert!(state.battlefield.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(events.len(), 1);
        match &events[0] {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
            } => {
                assert_eq!(*object_id, id);
                assert_eq!(*from, Zone::Hand);
                assert_eq!(*to, Zone::Battlefield);
            }
            _ => panic!("expected ZoneChanged event"),
        }
    }

    #[test]
    fn move_library_to_hand() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Hand, &mut events);

        assert!(!state.players[0].library.contains(&id));
        assert!(state.players[0].hand.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Hand);
    }

    #[test]
    fn move_battlefield_to_graveyard() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);
    }

    #[test]
    fn move_to_exile() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Exile, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.exile.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Exile);
    }

    #[test]
    fn move_generates_zone_changed_event() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            GameEvent::ZoneChanged {
                object_id: id,
                from: Zone::Hand,
                to: Zone::Graveyard,
            }
        );
    }

    #[test]
    fn move_to_library_top() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Top".to_string(),
            Zone::Hand,
        );

        let mut events = Vec::new();
        move_to_library_position(&mut state, id2, true, &mut events);

        assert_eq!(state.players[0].library[0], id2); // top
        assert_eq!(state.players[0].library[1], id1); // bottom
        assert_eq!(state.objects[&id2].zone, Zone::Library);
    }

    #[test]
    fn move_to_library_bottom() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );

        let mut events = Vec::new();
        move_to_library_position(&mut state, id2, false, &mut events);

        assert_eq!(state.players[0].library[0], id1); // stays at top
        assert_eq!(state.players[0].library[1], id2); // goes to bottom
    }

    #[test]
    fn player_zones_are_per_player() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Card".to_string(),
            Zone::Hand,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Hand,
        );

        assert!(state.players[0].hand.contains(&id1));
        assert!(!state.players[0].hand.contains(&id2));
        assert!(state.players[1].hand.contains(&id2));
        assert!(!state.players[1].hand.contains(&id1));
    }

    #[test]
    fn shared_zones_work_for_any_player() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Creature".to_string(),
            Zone::Battlefield,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Creature".to_string(),
            Zone::Battlefield,
        );

        assert!(state.battlefield.contains(&id1));
        assert!(state.battlefield.contains(&id2));
    }

    #[test]
    fn multiple_zone_transfers() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        // Library -> Hand (draw)
        move_to_zone(&mut state, id, Zone::Hand, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Hand);

        // Hand -> Battlefield (play)
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);

        // Battlefield -> Graveyard (destroy)
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);

        assert_eq!(events.len(), 3);
    }
}
