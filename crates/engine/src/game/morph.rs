use crate::types::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::sync::Arc;

use super::engine::EngineError;
use super::printed_cards::{apply_back_face_to_object, snapshot_object_face};

/// Stores the original characteristics of a face-down card so they can be
/// restored when the card is turned face up.
#[derive(Debug, Clone)]
pub struct FaceDownData {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub card_types: CardType,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub trigger_definitions: Vec<TriggerDefinition>,
    pub replacement_definitions: Vec<ReplacementDefinition>,
    pub static_definitions: Vec<StaticDefinition>,
    pub color: Vec<crate::types::mana::ManaColor>,
}

/// CR 708.2a: Face-down permanents have no characteristics except those
/// defined by the effect that put them face down. Manifest/morph-style face
/// down permanents default to 2/2 creatures with no name, subtypes, mana cost,
/// color, abilities, or rules text.
///
/// `profile` is the "otherwise specified by the effect" override from CR 708.2a:
/// power/toughness default to 2 when `None`, `Creature` is always present in the
/// core types (CR 708.2a), and any `extra_core_types`/`subtypes` the effect
/// listed are applied on top (CR 205.1a). `FaceDownProfile::vanilla_2_2()`
/// reproduces the manifest/morph default.
pub fn apply_face_down_creature_characteristics(
    obj: &mut crate::game::game_object::GameObject,
    profile: &crate::types::ability::FaceDownProfile,
) {
    obj.face_down = true;
    obj.name = String::new();
    obj.base_name = String::new();
    // CR 708.2a: power/toughness default to 2 unless the effect specifies otherwise.
    let power = profile.power.unwrap_or(2);
    let toughness = profile.toughness.unwrap_or(2);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    // CR 708.2a + CR 205.1a: Creature is always present; the effect may add
    // further core types (e.g. Artifact) without removing Creature.
    let mut core_types = vec![CoreType::Creature];
    for ct in &profile.extra_core_types {
        if !core_types.contains(ct) {
            core_types.push(*ct);
        }
    }
    obj.card_types = CardType {
        supertypes: vec![],
        core_types,
        subtypes: profile.subtypes.clone(),
    };
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::NoCost;
    obj.base_mana_cost = ManaCost::NoCost;
    obj.keywords = Vec::new();
    obj.base_keywords = Vec::new();
    obj.abilities = Arc::new(Vec::new());
    obj.base_abilities = Arc::new(Vec::new());
    obj.trigger_definitions = crate::types::definitions::Definitions::default();
    obj.base_trigger_definitions = Arc::new(Vec::new());
    obj.replacement_definitions = crate::types::definitions::Definitions::default();
    obj.base_replacement_definitions = Arc::new(Vec::new());
    obj.static_definitions = crate::types::definitions::Definitions::default();
    obj.base_static_definitions = Arc::new(Vec::new());
    obj.color = Vec::new();
    obj.base_color = Vec::new();
    // CR 708.2a: A face-down permanent has no name or printed identity. Clear
    // both the live and baseline display pointer so the layer reset cannot
    // restore the real card's art onto the face-down 2/2. The real ref is
    // preserved in `back_face` by `snapshot_object_face` and restored by
    // `turn_face_up` → `apply_back_face_to_object`.
    obj.printed_ref = None;
    obj.base_printed_ref = None;
}

/// CR 702.37a: A face-down permanent is a 2/2 creature with no name, mana cost, creature types, or abilities.
///
/// Moves the card from hand to battlefield with `face_down = true`, overriding
/// its characteristics to be a vanilla 2/2 creature. The original characteristics
/// are preserved in `back_face` so they can be restored by `turn_face_up`.
pub fn play_face_down(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this card".to_string(),
        ));
    }

    if obj.zone != Zone::Hand {
        return Err(EngineError::InvalidAction(
            "Card is not in hand".to_string(),
        ));
    }

    // Store original characteristics before overriding
    let original = snapshot_object_face(obj);

    // Move to battlefield
    super::zones::move_to_zone(state, object_id, Zone::Battlefield, events);

    // Apply face-down overrides
    let obj = state.objects.get_mut(&object_id).unwrap();
    apply_face_down_creature_characteristics(
        obj,
        &crate::types::ability::FaceDownProfile::vanilla_2_2(),
    );

    // Store original characteristics so turn_face_up can restore them
    obj.back_face = Some(original);

    Ok(())
}

/// CR 702.37c: Turning a face-down permanent face up restores its original characteristics.
///
/// Validates that the player controls the permanent and that it has morph/disguise
/// cost data stored. Sets `face_down = false`, restores characteristics from
/// stored `back_face`, and emits `GameEvent::TurnedFaceUp`.
pub fn turn_face_up(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this permanent".to_string(),
        ));
    }

    if !obj.face_down {
        return Err(EngineError::InvalidAction(
            "Permanent is not face down".to_string(),
        ));
    }

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }

    let back_face = obj
        .back_face
        .clone()
        .ok_or_else(|| EngineError::InvalidAction("No stored face data".to_string()))?;

    // Check that the card actually has a morph or disguise cost
    let has_morph_cost = back_face.keywords.iter().any(|k| {
        matches!(
            k,
            Keyword::Morph(_) | Keyword::Megamorph(_) | Keyword::Disguise(_)
        )
    });

    // For manifest: creature cards can be turned face up by paying mana cost
    // (handled separately -- here we just need morph/disguise keywords OR
    // we allow turning up if the card has a mana cost and is a creature)
    let is_manifested_creature = !has_morph_cost
        && back_face
            .card_types
            .core_types
            .contains(&CoreType::Creature);

    if !has_morph_cost && !is_manifested_creature {
        return Err(EngineError::InvalidAction(
            "Card cannot be turned face up (no morph cost)".to_string(),
        ));
    }

    // Restore original characteristics
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.face_down = false;
    apply_back_face_to_object(obj, back_face);
    obj.back_face = None;

    crate::game::layers::mark_layers_full(state);

    events.push(GameEvent::TurnedFaceUp { object_id });

    Ok(())
}

/// CR 701.40a: Shared helper that manifests a specific card face-down as a 2/2 creature.
/// Used by both `manifest()` (top of library) and Manifest Dread (player-selected card).
///
/// The card must already exist in `state.objects`. This function:
/// 1. Snapshots the card's original characteristics
/// 2. Moves it to the battlefield
/// 3. Applies face-down 2/2 creature overrides
/// 4. Stores originals in `back_face` for later turn-face-up
pub fn manifest_card(
    state: &mut GameState,
    _player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found for manifest".to_string()))?;

    // Store original characteristics before overriding
    let original = snapshot_object_face(obj);

    // Move to battlefield
    super::zones::move_to_zone(state, object_id, Zone::Battlefield, events);

    // Apply face-down overrides — CR 701.40a: 2/2 creature with no text/name/subtypes/mana cost
    let obj = state.objects.get_mut(&object_id).unwrap();
    apply_face_down_creature_characteristics(
        obj,
        &crate::types::ability::FaceDownProfile::vanilla_2_2(),
    );
    obj.back_face = Some(original);

    Ok(())
}

/// CR 701.40a: Manifest puts the top card of library onto battlefield face down as a 2/2 creature.
///
/// If the manifested card is a creature, it can later be turned face up by paying its mana cost.
pub fn manifest(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let player_state = state
        .players
        .iter()
        .find(|p| p.id == player)
        .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;

    let top_card_id = player_state
        .library
        .front()
        .copied()
        .ok_or_else(|| EngineError::InvalidAction("Library is empty".to_string()))?;

    // Find the object that corresponds to this library entry
    let object_id = state
        .objects
        .iter()
        .find(|(_, obj)| {
            obj.owner == player
                && obj.zone == Zone::Library
                && state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.library.front() == Some(&obj.id))
                    .unwrap_or(false)
        })
        .map(|(id, _)| *id)
        .ok_or_else(|| EngineError::InvalidAction("Top card object not found".to_string()))?;

    let _ = top_card_id; // used for finding the object above

    manifest_card(state, player, object_id, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::QuantityExpr;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaColor;

    fn setup_morph_creature(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            player,
            "Secret Creature".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(4);
        obj.toughness = Some(5);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Beast".to_string()],
        };
        obj.keywords = vec![
            Keyword::Morph(crate::types::mana::ManaCost::Cost {
                generic: 3,
                shards: vec![],
            }),
            Keyword::Trample,
        ];
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            crate::types::ability::AbilityKind::Activated,
            crate::types::ability::Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
        )]);
        obj.color = vec![ManaColor::Green];
        id
    }

    #[test]
    fn play_face_down_creates_2_2_with_no_characteristics() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();

        play_face_down(&mut state, player, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.card_types.core_types, vec![CoreType::Creature]);
        assert!(obj.card_types.subtypes.is_empty());
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());
        assert!(obj.color.is_empty());
    }

    #[test]
    fn turn_face_up_restores_original_characteristics() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();

        play_face_down(&mut state, player, id, &mut events).unwrap();
        turn_face_up(&mut state, player, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.face_down);
        assert_eq!(obj.name, "Secret Creature");
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(5));
        assert!(obj.card_types.subtypes.contains(&"Beast".to_string()));
        assert!(obj.keywords.contains(&Keyword::Trample));
        assert!(obj
            .keywords
            .contains(&Keyword::Morph(crate::types::mana::ManaCost::Cost {
                generic: 3,
                shards: vec![]
            })));
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.color, vec![ManaColor::Green]);
    }

    #[test]
    fn face_down_clears_printed_ref_and_turn_face_up_restores_it() {
        // CR 708.2a: a face-down 2/2 exposes no card identity, so its display
        // pointer (`printed_ref`) is cleared — including the baseline, so the
        // layer reset cannot resurrect the real card's art. Turning it face up
        // restores the original art from the snapshot in `back_face`.
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let secret_ref = crate::types::card::PrintedCardRef {
            oracle_id: "secret-oracle-id".to_string(),
            face_name: "Secret Creature".to_string(),
        };
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.printed_ref = Some(secret_ref.clone());
            obj.base_printed_ref = Some(secret_ref.clone());
        }

        let mut events = Vec::new();
        play_face_down(&mut state, player, id, &mut events).unwrap();
        assert_eq!(state.objects[&id].printed_ref, None);
        assert_eq!(state.objects[&id].base_printed_ref, None);
        // A layer pass must not restore the hidden card's art from a stale base.
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(state.objects[&id].printed_ref, None);

        turn_face_up(&mut state, player, id, &mut events).unwrap();
        assert_eq!(state.objects[&id].printed_ref, Some(secret_ref.clone()));
        assert_eq!(state.objects[&id].base_printed_ref, Some(secret_ref));
    }

    #[test]
    fn turn_face_up_emits_event() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();

        play_face_down(&mut state, player, id, &mut events).unwrap();
        events.clear();
        turn_face_up(&mut state, player, id, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceUp { object_id } if *object_id == id)));
    }

    #[test]
    fn face_down_hides_identity_from_opponents() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();

        play_face_down(&mut state, player, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        // Server-side: face_down = true means opponents cannot see the identity
        assert!(obj.face_down);
        // The actual identity is stored in back_face (hidden from opponents in serialization)
        assert!(obj.back_face.is_some());
        let original = obj.back_face.as_ref().unwrap();
        assert_eq!(original.name, "Secret Creature");
        assert_eq!(original.power, Some(4));
    }

    #[test]
    fn manifest_puts_top_card_face_down_as_2_2() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Add a card to the top of library
        let id = create_object(
            &mut state,
            CardId(10),
            player,
            "Library Creature".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Elemental".to_string()],
        };
        obj.keywords = vec![Keyword::Flying];
        obj.color = vec![ManaColor::Blue];

        let mut events = Vec::new();
        manifest(&mut state, player, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert!(obj.keywords.is_empty());

        // Original data preserved
        let original = obj.back_face.as_ref().unwrap();
        assert_eq!(original.name, "Library Creature");
        assert_eq!(original.power, Some(3));
    }

    #[test]
    fn manifested_creature_can_be_turned_face_up() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(10),
            player,
            "Manifest Target".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(5);
        obj.toughness = Some(5);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };

        let mut events = Vec::new();
        manifest(&mut state, player, &mut events).unwrap();

        // Turn face up (creature card can be turned up by paying mana cost)
        turn_face_up(&mut state, player, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.face_down);
        assert_eq!(obj.name, "Manifest Target");
        assert_eq!(obj.power, Some(5));
    }

    #[test]
    fn face_down_profile_applies_specified_characteristics() {
        // CR 708.2a + CR 205.1a: A Cyber-Controller-style profile overrides the
        // vanilla 2/2 default: 2/2, [Creature, Artifact], subtype "Cyberman".
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let secret_ref = crate::types::card::PrintedCardRef {
            oracle_id: "secret-oracle-id".to_string(),
            face_name: "Secret Creature".to_string(),
        };
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.printed_ref = Some(secret_ref.clone());
        }

        let original = snapshot_object_face(&state.objects[&id]);
        let profile = crate::types::ability::FaceDownProfile {
            power: Some(2),
            toughness: Some(2),
            extra_core_types: vec![CoreType::Artifact],
            subtypes: vec!["Cyberman".to_string()],
        };
        {
            let obj = state.objects.get_mut(&id).unwrap();
            apply_face_down_creature_characteristics(obj, &profile);
            obj.back_face = Some(original);
        }

        let obj = &state.objects[&id];
        assert!(obj.face_down);
        assert_eq!(obj.name, "");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        // CR 708.2a: Creature always present; Artifact added (CR 205.1a).
        assert_eq!(
            obj.card_types.core_types,
            vec![CoreType::Creature, CoreType::Artifact]
        );
        assert_eq!(obj.card_types.subtypes, vec!["Cyberman".to_string()]);
        // printed_ref cleared (no exposed identity); the real face is in back_face.
        assert_eq!(obj.printed_ref, None);
        assert!(obj.back_face.is_some());
        assert_eq!(obj.back_face.as_ref().unwrap().name, "Secret Creature");
    }

    #[test]
    fn manifested_noncreature_cannot_be_turned_face_up() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(10),
            player,
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Instant],
            subtypes: vec![],
        };

        let mut events = Vec::new();
        manifest(&mut state, player, &mut events).unwrap();

        // Try to turn face up -- should fail (no morph cost, not a creature)
        let result = turn_face_up(&mut state, player, id, &mut events);
        assert!(result.is_err());
    }
}
