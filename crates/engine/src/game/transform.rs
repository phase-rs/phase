use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::resolved_commands::{
    ResolvedObjectTransformCommand, ResolvedObjectTransformReplayInvariantError,
};
use crate::types::zones::Zone;

use super::engine::{EngineError, PriorityAnnouncementFacadeAccess, PriorityPrincipal};
use super::printed_cards::{apply_back_face_to_object, snapshot_object_face};

/// An engine-authored transform announcement for Priority preflight. The
/// permanent identity remains private to the transform authority until facade
/// conversion reconstructs the ordinary reducer primer.
pub(in crate::game) struct PriorityTransformAnnouncement {
    object_id: ObjectId,
}

impl PriorityTransformAnnouncement {
    fn new(object_id: ObjectId) -> Self {
        Self { object_id }
    }

    pub(in crate::game) fn object_id(
        &self,
        _access: &PriorityAnnouncementFacadeAccess,
    ) -> ObjectId {
        self.object_id
    }
}

/// CR 701.27 + CR 710.4 + CR 712.4c: Whether a battlefield permanent can
/// transform rather than silently ignoring the instruction.
pub(in crate::game) fn can_transform(state: &GameState, object_id: ObjectId) -> bool {
    let Some(object) = state.objects.get(&object_id) else {
        return false;
    };
    object.zone == Zone::Battlefield
        && object.back_face.is_some()
        && !transform_instruction_is_noop(state, object_id)
}

/// CR 701.27 + CR 710.4 + CR 712.4c: Whether a transform instruction is a
/// silent no-op after its subject is known to be a battlefield permanent.
fn transform_instruction_is_noop(state: &GameState, object_id: ObjectId) -> bool {
    let object = state
        .objects
        .get(&object_id)
        .expect("transform subject was validated before checking eligibility");
    crate::game::static_abilities::object_has_static_other(state, object_id, "CantTransform")
        // `merge_kind` distinguishes meld from a two-component mutate pile.
        || object.merge_kind == Some(crate::game::game_object::MergeKind::Meld)
        // Flip cards retain their alternate face in `back_face` but never
        // transform; `is_flip_permanent` is the canonical discriminator.
        || object.flipped
        || crate::game::flip::is_flip_permanent(object)
}

/// Enumerates controlled transformable battlefield permanents as Priority
/// primers. `can_transform` is shared with the reducer so accepted primers
/// always perform the advertised face change.
pub(in crate::game) fn priority_transform_announcements(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Vec<PriorityTransformAnnouncement> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|object_id| {
            state
                .objects
                .get(object_id)
                .is_some_and(|object| object.controller == principal.semantic_holder())
                && can_transform(state, *object_id)
        })
        .map(PriorityTransformAnnouncement::new)
        .collect()
}

/// CR 701.27a: Transform a double-faced permanent — turn it to its other face.
///
/// Toggles `obj.transformed`, swaps current characteristics with back_face data,
/// emits `GameEvent::Transformed`, and marks layers dirty.
///
/// Returns an error if the object has no back face (not a DFC).
pub fn transform_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Only permanents on the battlefield can transform".to_string(),
        ));
    }

    // A blocked, melded, or flip permanent ignores the instruction before a
    // meld result's intentionally absent back-face payload is inspected.
    if transform_instruction_is_noop(state, object_id) {
        return Ok(());
    }
    let back_face = obj
        .back_face
        .clone()
        .ok_or_else(|| EngineError::InvalidAction("Card has no back face".to_string()))?;

    // CR 613.7g: a double-faced permanent receives a new timestamp when it
    // transforms. All blocked/no-op early-returns above (off-battlefield,
    // CantTransform, meld, no back face) precede this, so they draw no timestamp.
    // Drawn before the `get_mut` borrow (`next_timestamp` takes `&mut self`).
    let ts = state.next_timestamp();

    let obj = state.objects.get_mut(&object_id).unwrap();

    if obj.transformed {
        let current_back = snapshot_object_face(obj);
        apply_back_face_to_object(obj, back_face);
        obj.back_face = Some(current_back);
        obj.transformed = false;
    } else {
        let current_front = snapshot_object_face(obj);
        apply_back_face_to_object(obj, back_face);
        obj.back_face = Some(current_front);
        obj.transformed = true;
    }
    // Written after `apply_back_face_to_object` (which does not touch
    // `timestamp`) so the single write covers both flip directions and cannot
    // be clobbered by the back-face application.
    obj.timestamp = ts;
    // CR 701.27f: Track successful transforms/conversions to ignore stale
    // self-transform instructions from abilities already on the stack.
    obj.transformation_count = obj.transformation_count.wrapping_add(1);

    let reference = ObjectIncarnationRef::from_object(obj);
    let resulting_transformed = obj.transformed;
    let resulting_transformation_count = obj.transformation_count;

    crate::game::layers::mark_layers_full(state);

    // CR 733: journal the settled transform through its owning family. Every
    // no-op guard above returned before the swap, so only a transform that
    // actually changed the displayed face is recorded.
    let cause = state.current_or_begin_rules_execution_node();
    state
        .resolved_rules_journal
        .record_object_transform(ResolvedObjectTransformCommand {
            object: reference,
            expected_old_transformed: !resulting_transformed,
            resulting_transformed,
            resulting_timestamp: ts,
            resulting_transformation_count,
            cause,
        })
        .expect("resolved transform must have a live journal cause");

    events.push(GameEvent::Transformed { object_id });

    Ok(())
}

/// CR 701.27a + CR 613.7g: Apply one exact already-resolved transform.
///
/// This installs the recorded face swap, displayed-face flag, timestamp, and
/// transformation count. It re-runs none of `transform_permanent`'s CR 701.27
/// eligibility guards — those decided at resolve time, and a command only
/// exists because the transform actually happened.
pub fn apply_resolved_transform(
    state: &mut GameState,
    command: &ResolvedObjectTransformCommand,
) -> Result<(), ResolvedObjectTransformReplayInvariantError> {
    let object = state.objects.get(&command.object.object_id).ok_or(
        ResolvedObjectTransformReplayInvariantError::UnknownObject(command.object.object_id),
    )?;
    let found = ObjectIncarnationRef::from_object(object);
    if found != command.object {
        return Err(ResolvedObjectTransformReplayInvariantError::StaleObject {
            expected: command.object,
            found,
        });
    }
    if object.transformed != command.expected_old_transformed {
        return Err(
            ResolvedObjectTransformReplayInvariantError::TransformedPreconditionMismatch {
                expected: command.expected_old_transformed,
                found: object.transformed,
            },
        );
    }
    let back_face = object.back_face.clone().ok_or(
        ResolvedObjectTransformReplayInvariantError::MissingBackFace(command.object.object_id),
    )?;

    let obj = state
        .objects
        .get_mut(&command.object.object_id)
        .expect("validated transform command object remains live");
    let displayed = snapshot_object_face(obj);
    apply_back_face_to_object(obj, back_face);
    obj.back_face = Some(displayed);
    obj.transformed = command.resulting_transformed;
    obj.timestamp = command.resulting_timestamp;
    obj.transformation_count = command.resulting_transformation_count;

    // CR 613.7g: the transform drew this timestamp during the original
    // execution, so replay installs it rather than drawing a fresh one — and
    // must carry the allocator past it or a later draw reissues it.
    state.adopt_replayed_timestamp(command.resulting_timestamp);

    crate::game::layers::mark_layers_full(state);

    Ok(())
}

/// CR 712.16 + CR 730.2j: True when `obj` is a double-faced permanent
/// (transform/modal/meld DFC) or a melded permanent — none of which can be
/// turned face down. Used by `effects::turn_face_down` to enforce the no-op,
/// and by presentation adapters that need the engine's authoritative
/// "is this permanent double-faced?" answer instead of re-deriving one.
///
/// Keys on the typed layout/merge discriminants rather than `back_face.is_some()`
/// so that single-faced layouts that may legally be turned face down — Adventure,
/// Omen, Split, Flip — are NOT blocked (they carry no Transform/Modal/Meld
/// `layout_kind`). CR 710 flip cards in particular put their alternative half in
/// the same `back_face` slot, so `back_face.is_some()` would report all 21 of
/// them as double-faced. A DFC currently showing its back face is caught by the
/// `transformed` flag, because `snapshot_object_face` zeroes `layout_kind` when
/// the front face is stashed in `back_face` during a transform.
pub fn is_double_faced_permanent(obj: &crate::game::game_object::GameObject) -> bool {
    use crate::types::card::LayoutKind;
    // CR 730.2j: a face-up melded permanent contains a double-faced component.
    if obj.merge_kind == Some(crate::game::game_object::MergeKind::Meld) {
        return true;
    }
    // CR 712.16: nonmodal/modal DFC and meld cards — the back face records the
    // DFC layout.
    if matches!(
        obj.back_face.as_ref().and_then(|b| b.layout_kind),
        Some(LayoutKind::Transform | LayoutKind::Modal | LayoutKind::Meld)
    ) {
        return true;
    }
    // CR 712.16: a DFC already showing its back face (its front face is snapshot
    // into `back_face` with a zeroed `layout_kind`) is still a DFC.
    obj.transformed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup_dfc(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Werewolf Front".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(3);
        obj.base_power = Some(2);
        obj.base_toughness = Some(3);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Werewolf".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.keywords = vec![Keyword::Vigilance];
        obj.base_keywords = vec![Keyword::Vigilance];
        obj.abilities = Arc::new(vec![crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            crate::types::ability::Effect::Unimplemented {
                name: "FrontAbility".to_string(),
                description: None,
            },
        )]);
        obj.base_abilities = Arc::clone(&obj.abilities);
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.parse_warnings = vec![OracleDiagnostic::IgnoredRemainder {
            text: "front diagnostic".to_string(),
            parser: "transform_test".to_string(),
            line_index: 0,
        }];

        obj.back_face = Some(BackFaceData {
            name: "Werewolf Back".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            mana_cost: crate::types::mana::ManaCost::default(),
            keywords: vec![Keyword::Trample],
            abilities: vec![crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                crate::types::ability::Effect::Unimplemented {
                    name: "BackAbility".to_string(),
                    description: None,
                },
            )],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::Green, ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![OracleDiagnostic::IgnoredRemainder {
                text: "back diagnostic".to_string(),
                parser: "transform_test".to_string(),
                line_index: 0,
            }],
        });

        id
    }

    #[test]
    fn transform_flips_to_back_face() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.transformed);
        assert_eq!(obj.name, "Werewolf Back");
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
        assert_eq!(obj.keywords, vec![Keyword::Trample]);
        assert_eq!(
            crate::types::ability::effect_variant_name(&obj.abilities[0].effect),
            "BackAbility"
        );
        assert_eq!(obj.color, vec![ManaColor::Green, ManaColor::Red]);
        assert!(matches!(
            obj.parse_warnings.as_slice(),
            [OracleDiagnostic::IgnoredRemainder { text, .. }] if text == "back diagnostic"
        ));
        assert!(state.layers_dirty.is_dirty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], GameEvent::Transformed { object_id: id });
    }

    #[test]
    fn transform_back_restores_front_face() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        transform_permanent(&mut state, id, &mut events).unwrap();
        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Werewolf Front");
        assert!(matches!(
            obj.parse_warnings.as_slice(),
            [OracleDiagnostic::IgnoredRemainder { text, .. }] if text == "front diagnostic"
        ));
        assert_eq!(events.len(), 2);
    }

    /// G1: transforming a double-faced permanent issues a new timestamp on each
    /// flip in both directions (CR 613.7g). A non-DFC permanent (no back face)
    /// returns before the write and draws no timestamp. Reverting Step 4 leaves
    /// the timestamp unchanged across a transform, so the strict-increase
    /// asserts fail.
    #[test]
    fn transform_bumps_timestamp_each_flip_but_not_for_non_dfc() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        let ts_initial = state.objects[&id].timestamp;

        transform_permanent(&mut state, id, &mut events).unwrap();
        let ts_front_to_back = state.objects[&id].timestamp;
        assert!(
            ts_front_to_back > ts_initial,
            "transforming to the back face must issue a new timestamp (CR 613.7g)"
        );

        transform_permanent(&mut state, id, &mut events).unwrap();
        let ts_back_to_front = state.objects[&id].timestamp;
        assert!(
            ts_back_to_front > ts_front_to_back,
            "transforming back to the front face must issue a new timestamp (CR 613.7g)"
        );

        // A non-DFC permanent (no back face) errors before the write -> no draw.
        let plain = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let plain_ts = state.objects[&plain].timestamp;
        assert!(transform_permanent(&mut state, plain, &mut events).is_err());
        assert_eq!(
            state.objects[&plain].timestamp, plain_ts,
            "a non-DFC permanent must not draw a timestamp on a failed transform"
        );
    }

    #[test]
    fn zone_change_resets_transformed() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        // Transform to back face
        transform_permanent(&mut state, id, &mut events).unwrap();
        assert!(state.objects[&id].transformed);
        assert_eq!(state.objects[&id].name, "Werewolf Back");

        // Move to graveyard (zone change should reset to front face)
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Werewolf Front");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(3));
    }

    #[test]
    fn non_dfc_cannot_transform() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Regular Card".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let result = transform_permanent(&mut state, id, &mut events);
        assert!(result.is_err());
        assert!(events.is_empty());
    }

    #[test]
    fn cant_transform_suppresses_transform() {
        // CR 701.27: A permanent with "Can't transform" silently no-ops.
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::Other("CantTransform".to_string()))
                .affected(TargetFilter::SelfRef),
        );
        let mut events = Vec::new();

        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.transformed, "transform should have been blocked");
        assert_eq!(obj.name, "Werewolf Front");
        assert!(events.is_empty(), "no Transformed event should be emitted");
    }

    #[test]
    fn priority_omits_a_permanent_that_cannot_transform() {
        use crate::types::ability::{StaticDefinition, TargetFilter};
        use crate::types::game_state::WaitingFor;
        use crate::types::phase::Phase;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let id = setup_dfc(&mut state);
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::Other("CantTransform".to_string()))
                .affected(TargetFilter::SelfRef),
        );
        let principal = crate::game::engine::priority_principal_for_preflight(&state)
            .expect("the synchronized priority window has a principal");

        assert!(
            priority_transform_announcements(&state, &principal).is_empty(),
            "Priority must not announce a transform that the reducer silently ignores"
        );
    }

    #[test]
    fn off_battlefield_object_cannot_transform() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
        let mut events = Vec::new();

        let result = transform_permanent(&mut state, id, &mut events);

        assert!(result.is_err());
        assert!(events.is_empty());
        assert!(!state.objects[&id].transformed);
    }
}
