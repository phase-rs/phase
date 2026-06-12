use crate::types::ability::{
    AbilityDefinition, FaceDownBody, ReplacementDefinition, StaticDefinition, TriggerDefinition,
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
use super::printed_cards::apply_back_face_to_object;

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
/// `profile` is the "otherwise specified by the effect" override from CR 708.2a.
/// For a `FaceDownBody::Creature` profile, power/toughness default to 2 when
/// `None`, `Creature` is always present in the core types, and any
/// `extra_core_types`/`subtypes` the effect listed are applied on top
/// (CR 205.1a); `FaceDownProfile::vanilla_2_2()` reproduces the manifest/morph
/// default. For a `FaceDownBody::Noncreature` profile (CR 708.2a sentence 2 —
/// e.g. Yedora's "It's a Forest land."), the core types come entirely from
/// `extra_core_types`, there is no implicit Creature type, and the permanent has
/// no power/toughness (CR 208.1).
pub fn apply_face_down_creature_characteristics(
    obj: &mut crate::game::game_object::GameObject,
    profile: &crate::types::ability::FaceDownProfile,
) {
    obj.face_down = true;
    obj.name = String::new();
    obj.base_name = String::new();
    // CR 708.2a + CR 205.1a: assemble the face-down core-type set. A creature
    // body (morph/manifest default, CR 708.2a sentence 1) always carries the
    // Creature core type with the effect's extra types layered on top. A
    // non-creature body (CR 708.2a sentence 2 — "It's a Forest land.") takes its
    // core types entirely from the effect, with no implicit Creature.
    let mut core_types = match profile.body {
        FaceDownBody::Creature => vec![CoreType::Creature],
        FaceDownBody::Noncreature => Vec::new(),
    };
    for ct in &profile.extra_core_types {
        if !core_types.contains(ct) {
            core_types.push(*ct);
        }
    }
    // CR 208.1 + CR 708.2a: only a creature body has power/toughness — it
    // defaults to 2/2 unless the effect specifies otherwise. A non-creature
    // body (a Forest land) has no power/toughness.
    let (power, toughness) = match profile.body {
        FaceDownBody::Creature => (
            Some(profile.power.unwrap_or(2)),
            Some(profile.toughness.unwrap_or(2)),
        ),
        FaceDownBody::Noncreature => (profile.power, profile.toughness),
    };
    obj.power = power;
    obj.toughness = toughness;
    obj.base_power = power;
    obj.base_toughness = toughness;
    obj.card_types = CardType {
        supertypes: vec![],
        core_types,
        subtypes: profile.subtypes.clone(),
    };
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::NoCost;
    obj.base_mana_cost = ManaCost::NoCost;
    // CR 701.58a: A cloaked permanent enters with ward {2}; plain manifest/morph
    // grants no keywords. The ward rides the face-down state and is replaced by
    // the real card's keywords when the card is turned face up.
    let face_down_keywords: Vec<Keyword> = match &profile.ward {
        Some(cost) => vec![Keyword::Ward(cost.clone())],
        None => Vec::new(),
    };
    obj.keywords = face_down_keywords.clone();
    obj.base_keywords = face_down_keywords;
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

    // CR 708.3 + CR 614.1c: route the face-down battlefield entry through the
    // zone-change pipeline. The delivery tail applies the face-down 2/2 profile
    // (snapshot the real face into `back_face`, overwrite with the vanilla 2/2 —
    // CR 708.2a) AND seeds enters-with-counters statics ("creatures you control
    // enter with an additional +1/+1 counter" — Hardened Scales class), which
    // the raw `move_to_zone` + manual override skipped entirely. CR 708.3: the
    // permanent is turned face down BEFORE it enters, so the tail does this
    // before the ETB-counter/trigger blocks — the manual post-move override is
    // dropped (the tail is the single authority, mirroring `manifest_card` and
    // change_zone's face-down path).
    //
    // CR 616.1: a battlefield-entry pause IS reachable here — two co-played
    // external enter-tapped `Moved` effects (Authority of the Consuls +
    // Imposing Sovereign class) both write the entry event's tap field, a
    // material same-field collision that surfaces an ordering prompt (see
    // `paused_face_down_morph_entry_resumes_face_down`). The bail is correct
    // and complete: the face-down profile rides the parked event, and the
    // resume path (`engine_replacement::handle_replacement_choice`'s ZoneChange
    // arm) applies it through the shared CR 708.3 helper
    // (`zone_pipeline::apply_face_down_entry_profile`), so the entry resumes
    // face down with nothing left for this helper to do.
    match super::zone_pipeline::move_object(
        state,
        super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, object_id)
            .face_down(crate::types::ability::FaceDownProfile::vanilla_2_2()),
        events,
    ) {
        super::zone_pipeline::ZoneMoveResult::Done => Ok(()),
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => Ok(()),
    }
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
    profile: crate::types::ability::FaceDownProfile,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if !state.objects.contains_key(&object_id) {
        return Err(EngineError::InvalidAction(
            "Object not found for manifest".to_string(),
        ));
    }

    // CR 701.40a + CR 708.3 + CR 614.1c: route the face-down manifest entry
    // through the zone-change pipeline. The delivery tail applies the vanilla
    // 2/2 face-down profile (snapshot real face into `back_face`, overwrite —
    // CR 708.2a) AND seeds enters-with-counters statics (Hardened Scales class),
    // which the raw `move_to_zone` + manual override skipped. The manual
    // post-move override is dropped (the tail is the single authority).
    //
    // CR 616.1: a battlefield-entry pause IS reachable — two co-played external
    // enter-tapped `Moved` effects (Authority of the Consuls + Imposing
    // Sovereign class) collide on the entry's tap field and surface an ordering
    // prompt. The bail is correct and complete: the face-down profile rides the
    // parked event and the resume path applies it through the shared CR 708.3
    // helper (`zone_pipeline::apply_face_down_entry_profile`), so the manifest
    // resumes face down with nothing left for this helper to do.
    match super::zone_pipeline::move_object(
        state,
        super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, object_id)
            .face_down(profile),
        events,
    ) {
        super::zone_pipeline::ZoneMoveResult::Done => Ok(()),
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => Ok(()),
    }
}

/// Find the object id of the top card of `player`'s library, if any.
fn top_library_object(state: &GameState, player: PlayerId) -> Result<ObjectId, EngineError> {
    let player_state = state
        .players
        .iter()
        .find(|p| p.id == player)
        .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;

    let _top_card_id = player_state
        .library
        .front()
        .copied()
        .ok_or_else(|| EngineError::InvalidAction("Library is empty".to_string()))?;

    // Find the object that corresponds to this library entry
    state
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
        .ok_or_else(|| EngineError::InvalidAction("Top card object not found".to_string()))
}

/// CR 701.40a: Manifest puts the top card of library onto battlefield face down as a 2/2 creature.
///
/// If the manifested card is a creature, it can later be turned face up by paying its mana cost.
pub fn manifest(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let object_id = top_library_object(state, player)?;
    manifest_card(
        state,
        player,
        object_id,
        crate::types::ability::FaceDownProfile::vanilla_2_2(),
        events,
    )
}

/// CR 701.58a: Cloak puts the top card of library onto the battlefield face
/// down as a 2/2 creature **with ward {2}**. Like manifest, a cloaked creature
/// card can later be turned face up for its mana cost.
pub fn cloak(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let object_id = top_library_object(state, player)?;
    manifest_card(
        state,
        player,
        object_id,
        crate::types::ability::FaceDownProfile::cloaked_2_2(),
        events,
    )
}

#[cfg(test)]
mod tests {
    use super::super::printed_cards::snapshot_object_face;
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

    /// CR 616.1 + CR 708.3 discriminating test (fail-first): a face-down morph
    /// entry parked on a replacement-ordering prompt must resume FACE DOWN.
    ///
    /// Reachability: two co-played external enter-tapped `Moved` defs (Authority
    /// of the Consuls + Imposing Sovereign class — both parse as ChangeZone
    /// Moved defs) both write the entry event's tap field; same-field writes are
    /// non-commuting and the engine has no same-value dedupe, so the set is
    /// material and CR 616.1 prompts — `move_object` parks the morph entry.
    ///
    /// The resume path (`handle_replacement_choice`'s ZoneChange arm) previously
    /// destructured the approved event with `..`, DISCARDING
    /// `face_down_profile`, and delivered via the raw mover — the morph resumed
    /// FACE UP, violating CR 708.3 and leaking the hidden card to the opponent.
    #[test]
    fn paused_face_down_morph_entry_resumes_face_down() {
        use crate::game::engine::apply_as_current;
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Two external enter-tapped Moved replacements on the opponent's board.
        for (offset, name) in [
            (0u64, "Authority of the Consuls"),
            (1, "Imposing Sovereign"),
        ] {
            let oid = ObjectId(9000 + offset);
            let mut src = GameObject::new(
                oid,
                CardId(900 + offset),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            src.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    crate::types::ability::Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: crate::types::ability::EffectScope::Single,
                        state: crate::types::ability::TapStateChange::Tap,
                    },
                ))
                .destination_zone(Zone::Battlefield)
                .description(name.to_string())]
            .into();
            state.objects.insert(oid, src);
            state.battlefield.push_back(oid);
        }

        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();
        play_face_down(&mut state, player, id, &mut events).unwrap();

        // CR 616.1: the colliding enter-tapped writes parked the entry — the
        // card has NOT moved yet and the prompt is live.
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice for the enter-tapped collision, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            state.objects[&id].zone,
            Zone::Hand,
            "entry must be parked, not delivered, while the prompt is live"
        );
        state.priority_player = chooser;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume replacement choice");

        let obj = &state.objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "entry delivered after resume");
        assert!(
            obj.tapped,
            "both enter-tapped replacements applied to the resumed entry"
        );
        assert!(
            obj.face_down,
            "resumed morph entry must be FACE DOWN (CR 708.3) — face-up resume leaks the hidden card"
        );
        assert_eq!(obj.power, Some(2), "vanilla 2/2 face-down profile");
        assert_eq!(obj.toughness, Some(2), "vanilla 2/2 face-down profile");
        assert_eq!(obj.name, "", "face-down profile hides the printed name");
        assert!(obj.card_types.subtypes.is_empty());
        assert!(
            obj.back_face.is_some(),
            "real face snapshot stored so turn-face-up can restore it"
        );
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

    /// Regression test for GitHub issue #2024: Controller can look at their
    /// own face-down manifested card on the battlefield. This test verifies
    /// the visibility system correctly exposes face-down cards to their controller.
    #[test]
    fn controller_can_see_own_face_down_manifested_card() {
        use crate::game::visibility::filter_state_for_viewer;

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        let id = create_object(
            &mut state,
            CardId(10),
            controller,
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
        manifest(&mut state, controller, &mut events).unwrap();

        // Controller should see the full card
        let controller_view = filter_state_for_viewer(&state, controller);
        let controller_obj = controller_view.objects.get(&id).unwrap();
        assert_eq!(controller_obj.name, "Manifest Target");
        assert!(controller_obj.face_down);

        // Opponent should see it as hidden
        let opponent_view = filter_state_for_viewer(&state, opponent);
        let opponent_obj = opponent_view.objects.get(&id).unwrap();
        assert_eq!(opponent_obj.name, "Hidden Card");
        assert!(opponent_obj.face_down);
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
            body: crate::types::ability::FaceDownBody::Creature,
            extra_core_types: vec![CoreType::Artifact],
            subtypes: vec!["Cyberman".to_string()],
            ward: None,
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
