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
    // external enter tap-state `Moved` effects writing in *opposite* directions
    // (one enters tapped, one enters untapped — the Frozen Aether + Spelunking
    // class) are last-applied-wins, a material CR 616.1e/f collision that
    // surfaces an ordering prompt (see
    // `paused_face_down_morph_entry_resumes_face_down`). (Two same-direction
    // writes are idempotent and commute without a prompt — see replacement.rs
    // `CommuteClass::EnterTapped`/`EnterUntapped`.) The bail is correct
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

/// CR 116.2b + CR 708.7: True when an active `CantBeTurnedFaceUp` static
/// prohibits turning `object_id` face up. Each such static's affected filter is
/// resolved from its source controller, so Karlov Watchdog's "permanents your
/// opponents control" (`controller: Opponent`) scopes to the watchdog
/// controller's opponents. The timing window ("during your turn") rides on the
/// static's `condition`, already applied by `battlefield_active_statics`.
pub(crate) fn is_blocked_by_cant_be_turned_face_up(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::statics::StaticMode;
    for (source, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if !matches!(def.mode, StaticMode::CantBeTurnedFaceUp) {
            continue;
        }
        let Some(filter) = def.affected.as_ref() else {
            continue;
        };
        let ctx = super::filter::FilterContext::from_source(state, source.id);
        if super::filter::matches_target_filter(state, object_id, filter, &ctx) {
            return true;
        }
    }
    false
}

/// CR 702.37e / CR 702.168d / CR 701.40b: Validate a turn-face-up special action
/// and derive the mana cost that must be paid before the permanent is flipped.
///
/// Shared front half of [`turn_face_up`]: checks controller, face-down state,
/// battlefield zone, and the `CantBeTurnedFaceUp` static (CR 116.2b + CR 708.7),
/// then extracts the cost to pay:
/// - a morph/megamorph/disguise keyword's stored cost (CR 702.37e / CR 702.168d), or
/// - a manifested creature card's mana cost (CR 701.40b).
///
/// CR 701.40b: a face-down permanent that is neither (no morph/disguise cost and
/// not a creature card) can't be turned face up this way — returns `Err`.
///
/// Kept separate from the commit half so the paid `GameAction::TurnFaceUp`
/// special-action handler can charge the returned cost through
/// `pay_special_action_mana_cost` before `turn_face_up` flips the permanent,
/// while the free direct callers (grant path, tests) reuse the same guards.
pub(crate) fn turn_face_up_prepare(
    state: &GameState,
    object_id: ObjectId,
    player: PlayerId,
) -> Result<ManaCost, EngineError> {
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

    // CR 116.2b + CR 708.7: a `CantBeTurnedFaceUp` static prohibits turning the
    // matched permanents face up (Karlov Watchdog: "Permanents your opponents
    // control can't be turned face up during your turn"). The static's timing
    // window rides on its `condition` (already gated by `battlefield_active_statics`)
    // and the affected permanents on its `affected` filter, resolved from the
    // static's source controller.
    if is_blocked_by_cant_be_turned_face_up(state, object_id) {
        return Err(EngineError::InvalidAction(
            "This permanent can't be turned face up right now".to_string(),
        ));
    }

    let back_face = obj
        .back_face
        .as_ref()
        .ok_or_else(|| EngineError::InvalidAction("No stored face data".to_string()))?;

    // CR 702.37e / CR 702.168d: the morph/megamorph/disguise cost is the cost
    // paid to turn the permanent face up. CR 701.40b: a manifested creature card
    // is turned up by paying its mana cost; a non-creature or no-mana-cost
    // manifest can't be turned up this way.
    back_face
        .keywords
        .iter()
        .find_map(|k| match k {
            Keyword::Morph(c) | Keyword::Megamorph(c) | Keyword::Disguise(c) => Some(c.clone()),
            _ => None,
        })
        .or_else(|| {
            if back_face
                .card_types
                .core_types
                .contains(&CoreType::Creature)
                && !matches!(back_face.mana_cost, ManaCost::NoCost)
            {
                Some(back_face.mana_cost.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            EngineError::InvalidAction("Card cannot be turned face up (no morph cost)".to_string())
        })
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
    // All validation + cost derivation lives in `turn_face_up_prepare` so the
    // paid `GameAction::TurnFaceUp` special-action route and the free direct
    // callers agree on legality. The derived cost is charged by the special-action
    // handler before it calls this commit half; the free callers discard it.
    turn_face_up_prepare(state, object_id, player)?;

    // `turn_face_up_prepare` guaranteed the stored face is present; re-clone it
    // for the commit. The immutable borrow ends before `next_timestamp` below
    // (which takes `&mut self`).
    let back_face = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.back_face.clone())
        .ok_or_else(|| EngineError::InvalidAction("No stored face data".to_string()))?;

    // CR 613.7f: a permanent receives a new timestamp when it turns face up.
    // (Turning face DOWN in place is unreachable in the engine today — only
    // archenemy scheme `turn_face_down` exists, which is not a permanent event —
    // so stamping the turn-face-up path covers the reachable case.) All error
    // early-returns above precede this, so a blocked turn-up draws no timestamp.
    // Drawn before the `get_mut` borrow (`next_timestamp` takes `&mut self`).
    let ts = state.next_timestamp();

    // Restore original characteristics
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.face_down = false;
    apply_back_face_to_object(obj, back_face);
    obj.back_face = None;
    // Written after `apply_back_face_to_object` so the back-face application
    // (which does not touch `timestamp`) cannot clobber the new stamp.
    obj.timestamp = ts;

    crate::game::layers::mark_layers_full(state);

    // CR 614.1e + CR 708.11: now that the permanent is face up
    // (carrying its real abilities), apply any "As ~ is turned face up, [effect]"
    // replacement effects as part of turning it up. The turn-up is not prevented
    // — the applier returns the event unchanged and performs its actions (e.g.
    // Hooded Hydra's +1/+1 counters) bound to this permanent.
    let proposed = crate::types::proposed_event::ProposedEvent::TurnFaceUp {
        object_id,
        applied: std::collections::HashSet::new(),
    };
    let _ = crate::game::replacement::replace_event(state, proposed, events);

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
///
/// `source_id` is the spell or ability source responsible for the manifest entry.
pub fn manifest_card(
    state: &mut GameState,
    _player: PlayerId,
    object_id: ObjectId,
    source_id: ObjectId,
    profile: crate::types::ability::FaceDownProfile,
    controller: Option<PlayerId>,
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
    // enter tap-state `Moved` effects writing in *opposite* directions (one
    // enters tapped, one enters untapped — the Frozen Aether + Spelunking class)
    // are last-applied-wins, a material CR 616.1e/f collision that surfaces an
    // ordering prompt (same-direction writes commute, no prompt — see
    // replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`). The bail is
    // correct and complete: the face-down profile rides the
    // parked event and the resume path applies it through the shared CR 708.3
    // helper (`zone_pipeline::apply_face_down_entry_profile`), so the manifest
    // resumes face down with nothing left for this helper to do.
    // CR 110.2a: An effect that puts an object onto the battlefield may specify
    // a controller other than the object's owner ("under your control"). When
    // `controller` is `Some`, the manifested card enters under that player's
    // control instead of the library owner's (Cybership routes the damaged
    // player's cards under the Cybership controller). The move is attributed to
    // `source_id` (the manifesting spell/ability), not the moved object.
    let mut request =
        super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, source_id)
            .face_down(profile);
    if let Some(controller) = controller {
        request = request.under_control_of(controller);
    }
    match super::zone_pipeline::move_object(state, request, events) {
        super::zone_pipeline::ZoneMoveResult::Done => Ok(()),
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => Ok(()),
    }
}

/// Find the object id of the top card of `player`'s library, if any.
pub(crate) fn top_library_object(
    state: &GameState,
    player: PlayerId,
) -> Result<ObjectId, EngineError> {
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
        object_id,
        crate::types::ability::FaceDownProfile::vanilla_2_2(),
        None,
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
        object_id,
        crate::types::ability::FaceDownProfile::cloaked_2_2(),
        None,
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
    /// Reachability: two co-played external enter tap-state `Moved` defs writing
    /// in *opposite* directions (one enters tapped, one enters untapped — the
    /// Frozen Aether + Spelunking class, both parse as ChangeZone Moved defs)
    /// are last-applied-wins, a material CR 616.1e/f collision that prompts —
    /// `move_object` parks the morph entry. (Same-direction writes are
    /// idempotent and commute without a prompt — see replacement.rs
    /// `CommuteClass::EnterTapped`/`EnterUntapped`.)
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

        // A genuinely *material* enter tap-state collision: one replacement makes
        // the entrant enter tapped (Frozen Aether class), the other makes it
        // enter untapped (Spelunking / Archelos class). Opposite directions are
        // last-applied-wins, so CR 616.1e/f requires the controller to order them
        // and the entry parks on a ReplacementChoice. (Two same-direction writes
        // commute — see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`.)
        for (offset, name, state_change) in [
            (
                0u64,
                "Frozen Aether",
                crate::types::ability::TapStateChange::Tap,
            ),
            (
                1,
                "Spelunking",
                crate::types::ability::TapStateChange::Untap,
            ),
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
                        state: state_change,
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

        // CR 616.1: the colliding tap/untap (opposite-direction) writes parked
        // the entry — the card has NOT moved yet and the prompt is live.
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice for the tap/untap collision, got {:?}",
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
        // CR 616.1e/f: opposite-direction tap-state writes are last-applied-wins.
        // The chosen order (`index: 0`) lands the untapped write last, so the
        // resumed entry is untapped — confirming both colliding replacements ran
        // through the resume path and the chosen ordering was honored.
        assert!(
            !obj.tapped,
            "the chosen ordering's last-applied untap write must win on the resumed entry"
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

    /// §10.1 NO-OVER-SUPPRESSION guard (NOT a revert-tripwire): the CR 708.3/708.2a
    /// entry guard suppresses only the ENTERING object's OWN self-replacement
    /// (`is_entering`, i.e. `rid.source == entering object`). An EXTERNAL source's
    /// enters-tapped replacement has `is_entering == false` and must STILL apply to
    /// a face-down 2/2 (a face-down permanent is still a creature entering the
    /// battlefield). Install ONE type-agnostic external "enters tapped" `Moved`
    /// replacement (Frozen Aether class, `valid_card == None`) on a DIFFERENT
    /// permanent — single direction, so no CR 616.1 collision/prompt — then play a
    /// morph creature face down and assert it enters TAPPED.
    ///
    /// This passes WITH and WITHOUT the guard: it guards against a naive
    /// "skip all replacements on face-down entry" broadening, not against guard
    /// absence. The discriminator for the fix is
    /// `warden_played_face_down_gains_zero_counters` (0 → 2 on revert).
    #[test]
    fn external_enters_tapped_still_applies_to_face_down_entry() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let oid = ObjectId(9000);
        let mut src = GameObject::new(
            oid,
            CardId(900),
            PlayerId(1),
            "Frozen Aether".to_string(),
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
            .description("Frozen Aether".to_string())]
        .into();
        state.objects.insert(oid, src);
        state.battlefield.push_back(oid);

        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();
        play_face_down(&mut state, player, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "reach-guard: the face-down entry was delivered (single-direction write, no prompt)"
        );
        assert!(obj.face_down, "reach-guard: entered FACE DOWN (CR 708.3)");
        assert!(
            obj.tapped,
            "external (is_entering == false) enters-tapped replacement still applies to the \
             face-down 2/2 — the guard suppresses only the entrant's OWN self-replacement"
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

    /// F1: turning a permanent face up issues a new timestamp (CR 613.7f). The
    /// error early-returns (wrong controller / not face down / off battlefield)
    /// all precede the write, so a rejected turn-up draws no timestamp.
    /// Reverting Step 3 leaves the timestamp unchanged across the successful
    /// turn-up, so the strict-increase assert fails.
    #[test]
    fn turn_face_up_bumps_timestamp_only_on_success() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);
        let mut events = Vec::new();

        play_face_down(&mut state, player, id, &mut events).unwrap();
        let ts_before = state.objects[&id].timestamp;

        // Wrong controller: error before the write -> no timestamp drawn.
        assert!(turn_face_up(&mut state, PlayerId(1), id, &mut events).is_err());
        assert_eq!(
            state.objects[&id].timestamp, ts_before,
            "a rejected turn-up (wrong controller) must not draw a timestamp"
        );

        // Successful turn-up: new timestamp (CR 613.7f).
        turn_face_up(&mut state, player, id, &mut events).unwrap();
        assert!(
            state.objects[&id].timestamp > ts_before,
            "turning face up must issue a new timestamp (CR 613.7f)"
        );

        // Already face up: not-face-down error before the write -> no further bump.
        let ts_after = state.objects[&id].timestamp;
        assert!(turn_face_up(&mut state, player, id, &mut events).is_err());
        assert_eq!(state.objects[&id].timestamp, ts_after);
    }

    /// CR 116.2b + CR 708.7: Karlov Watchdog — "Permanents your opponents
    /// control can't be turned face up during your turn." A `CantBeTurnedFaceUp`
    /// static controlled by P0 blocks P1 from turning their own face-down
    /// creature up while it is P0's turn (the prohibition's `DuringYourTurn`
    /// condition), but permits it on P1's own turn. Discriminating: the assert
    /// `is_err()` on P0's turn flips to a successful turn-up if the prohibition
    /// check in `turn_face_up` is removed.
    #[test]
    fn karlov_watchdog_blocks_opponent_turn_face_up_during_your_turn() {
        use crate::types::ability::{
            ControllerRef, FilterProp, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let watchdog_controller = PlayerId(0);
        let opponent = PlayerId(1);

        // P1 controls a face-down morph creature.
        let face_down = setup_morph_creature(&mut state, opponent);
        let mut events = Vec::new();
        play_face_down(&mut state, opponent, face_down, &mut events).unwrap();
        assert!(state.objects[&face_down].face_down);

        // P0 controls a Karlov-Watchdog-class permanent: "Permanents your
        // opponents control can't be turned face up during your turn."
        let watchdog = create_object(
            &mut state,
            CardId(0x4A12),
            watchdog_controller,
            "Karlov Watchdog".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&watchdog).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(0);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantBeTurnedFaceUp)
                    .affected(TargetFilter::Typed(
                        TypedFilter::permanent()
                            .controller(ControllerRef::Opponent)
                            .properties(vec![FilterProp::FaceDown]),
                    ))
                    .condition(crate::types::ability::StaticCondition::DuringYourTurn),
            );
        }

        // On P0's turn, the opponent's face-down permanent can't be turned up.
        state.active_player = watchdog_controller;
        let blocked = turn_face_up(&mut state, opponent, face_down, &mut events);
        assert!(
            blocked.is_err(),
            "during the watchdog controller's turn, the opponent must not be \
             able to turn their face-down creature up"
        );
        assert!(
            state.objects[&face_down].face_down,
            "the face-down creature must remain face down while prohibited"
        );

        // On the opponent's own turn, the prohibition's DuringYourTurn condition
        // (bound to the watchdog controller) no longer holds, so the turn-up is
        // permitted.
        state.active_player = opponent;
        turn_face_up(&mut state, opponent, face_down, &mut events)
            .expect("the opponent may turn their creature up on their own turn");
        assert!(!state.objects[&face_down].face_down);
    }

    #[test]
    fn turn_face_up_applies_as_turned_face_up_replacement() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);

        // CR 614.1e + CR 708.11: give the real face a Hooded-Hydra-style
        // "As ~ is turned face up, put five +1/+1 counters on it." replacement.
        {
            let def = crate::parser::oracle_replacement::parse_replacement_line(
                "As this creature is turned face up, put five +1/+1 counters on it.",
                "Secret Creature",
            )
            .expect("turn-face-up replacement should parse");
            assert_eq!(def.event, crate::types::ReplacementEvent::TurnFaceUp);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.replacement_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_replacement_definitions).push(def);
        }

        let mut events = Vec::new();
        play_face_down(&mut state, player, id, &mut events).unwrap();
        turn_face_up(&mut state, player, id, &mut events).unwrap();

        // The replacement applied AS the permanent was turned face up — it gains
        // five +1/+1 counters (and there is no stack trigger / response window).
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            5,
            "the As-turned-face-up replacement must put five +1/+1 counters on it"
        );
    }

    #[test]
    fn turn_face_up_self_resolving_chain_applies_each_step_once() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = setup_morph_creature(&mut state, player);

        // A two-step self-resolving "As ~ is turned face up" replacement: put two
        // +1/+1 counters on it, then put one more on it (both steps `SelfRef`).
        // `resolve_ability_chain` follows the typed `sub_ability` chain itself, so
        // each step must run EXACTLY once — total 3 counters, not 4 from a
        // double-resolved second step.
        {
            let inner = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            );
            let mut outer = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::SelfRef,
                },
            );
            outer.sub_ability = Some(Box::new(inner));
            let def = crate::types::ability::ReplacementDefinition::new(
                crate::types::ReplacementEvent::TurnFaceUp,
            )
            .valid_card(TargetFilter::SelfRef)
            .execute(outer);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.replacement_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_replacement_definitions).push(def);
        }

        let mut events = Vec::new();
        play_face_down(&mut state, player, id, &mut events).unwrap();
        turn_face_up(&mut state, player, id, &mut events).unwrap();

        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "each self-resolving step must apply exactly once (2 + 1), not double the sub-ability"
        );
    }

    #[test]
    fn turn_face_up_gaps_attach_to_external_target() {
        // CR 708.11: Gift of Doom's "As ~ is turned face up, you may attach it to
        // a creature" needs an external host *choice* made during the turn-up,
        // which the replacement-apply path does not model. It is gapped honestly
        // (no TurnFaceUp replacement is produced) rather than silently attaching
        // the Aura to the wrong object — only the self-resolving counter class is
        // modeled here.
        let attach = crate::parser::oracle_replacement::parse_replacement_line(
            "As this permanent is turned face up, you may attach it to a creature.",
            "Secret Creature",
        );
        assert!(
            !attach
                .as_ref()
                .is_some_and(|d| d.event == crate::types::ReplacementEvent::TurnFaceUp),
            "attach-to-an-external-creature turn-face-up must not be modeled as a \
             TurnFaceUp replacement (needs a turn-up-time choice)"
        );

        // The self-resolving counter class IS modeled.
        let counters = crate::parser::oracle_replacement::parse_replacement_line(
            "As this creature is turned face up, put five +1/+1 counters on it.",
            "Secret Creature",
        )
        .expect("self-counter turn-face-up replacement should parse");
        assert_eq!(counters.event, crate::types::ReplacementEvent::TurnFaceUp);
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
    fn manifested_creature_with_no_mana_cost_cannot_be_turned_face_up() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(10),
            player,
            "No Cost Creature".to_string(),
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
        obj.mana_cost = ManaCost::NoCost;
        obj.base_mana_cost = ManaCost::NoCost;

        let mut events = Vec::new();
        manifest(&mut state, player, &mut events).unwrap();

        let result = turn_face_up(&mut state, player, id, &mut events);
        assert!(
            result.is_err(),
            "a manifested creature with no mana cost cannot be turned face up"
        );
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
