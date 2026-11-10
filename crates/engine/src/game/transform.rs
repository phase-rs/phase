use crate::game::game_object::{GameObject, ObjectIncarnationRefExt};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::object_data::BackFaceData;
use crate::types::resolved_commands::{
    ResolvedObjectTransformCommand, ResolvedObjectTransformReplayInvariantError,
};
use crate::types::zones::Zone;

use super::engine::{EngineError, PriorityAnnouncementFacadeAccess, PriorityPrincipal};
use super::printed_cards::{apply_back_face_to_object, snapshot_object_base_face};

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
        || !other_face_is_transform_destination(object)
}

/// CR 701.27c + CR 701.27d: Whether the face stored in `back_face` is one this
/// permanent can transform into. An absent `back_face` is left to the callers'
/// own "no back face" handling and so is not judged here.
///
/// The stored-face layout is the typed discriminator: Split (CR 709), Flip
/// (CR 710), Adventurer (CR 715), Omen (CR 720) and preparation (CR 722) cards
/// keep alternative characteristics in the same `back_face` slot, but they are
/// single-faced cards, and a token copy of one carries those characteristics as
/// copiable values (CR 722.2b) without becoming double-faced. A transformed
/// double-faced permanent stores its front face with an erased layout, and
/// test-built double-faced fixtures often carry no layout at all, so only the
/// known single-faced layouts are excluded. Independently of layout, a face that
/// is an instant or sorcery is never a transform destination.
fn other_face_is_transform_destination(object: &crate::game::game_object::GameObject) -> bool {
    use crate::types::card_type::CoreType;
    let Some(face) = object.back_face.as_ref() else {
        return true;
    };
    // CR 701.27c: a permanent that isn't represented by a double-faced token or
    // a double-faced card can't transform.
    let single_faced_layout = layout_is_single_faced_alternative(face.layout_kind);
    // CR 701.27d: transforming into an instant or sorcery face does nothing.
    let instant_or_sorcery_face = face
        .card_types
        .core_types
        .iter()
        .any(|core| matches!(core, CoreType::Instant | CoreType::Sorcery));
    !single_faced_layout && !instant_or_sorcery_face
}

/// CR 709 / CR 710 / CR 715 / CR 720 / CR 722: whether a stored face's layout marks a
/// single-faced card's alternative characteristics (split half, flip half, Adventure, Omen,
/// prepare spell) kept in the `back_face` slot. Such a card is not double-faced, and a token
/// copy of it carries those characteristics as copiable values (CR 709.5b, CR 722.2b).
fn layout_is_single_faced_alternative(layout: Option<crate::types::card::LayoutKind>) -> bool {
    use crate::types::card::LayoutKind;
    match layout {
        Some(
            LayoutKind::Split
            | LayoutKind::Flip
            | LayoutKind::Adventure
            | LayoutKind::Omen
            | LayoutKind::Prepare,
        ) => true,
        Some(LayoutKind::Single | LayoutKind::Transform | LayoutKind::Modal | LayoutKind::Meld)
        | None => false,
    }
}

/// CR 712.1 + CR 111.10i: the other face of a double-faced card or double-faced token, as the
/// engine stores it in `back_face` — `None` when the object is not double-faced, or when its
/// `back_face` holds something other than an other face.
///
/// Reads only the object's own state (its stored face, its own status, its printed type
/// line), never copy-modified values, so a Layer-1 copy effect does not change the answer.
///
/// Not [`is_double_faced_permanent`]: that answers CR 712.16's face-down question and keys on
/// printed layouts and `transformed`, so it is false for a front-face-up Incubator token,
/// whose stored Phyrexian face has no layout. This one keys on what `back_face` holds.
pub(crate) fn stored_double_face(obj: &GameObject) -> Option<&BackFaceData> {
    use crate::game::game_object::MergeKind;
    use crate::types::card::LayoutKind;
    let face = obj.back_face.as_ref()?;
    // CR 708.2 + CR 712.15: a face-down object's `back_face` is its face-down stash.
    if obj.face_down {
        return None;
    }
    // Specialize (digital-only, no CR rule): the stash is the face before specializing.
    if obj.specialized_color.is_some() {
        return None;
    }
    match obj.merge_kind {
        // CR 730.2i: a merged permanent is not a double-faced permanent even if it contains
        // double-faced components.
        Some(MergeKind::Mutate | MergeKind::Augment) => return None,
        // CR 712.1 + CR 712.16: meld cards are double-faced cards, and a melded permanent is
        // a double-faced permanent.
        Some(MergeKind::Meld) | None => {}
    }
    // CR 709.5b: a Room's other half is a copiable value of a single-faced card. A token copy
    // stores it with no layout, so the printed type line is the discriminator.
    if crate::game::room::has_shared_type_line(&obj.base_card_types) {
        return None;
    }
    // A `Single` layout names a single-faced card. No production writer stores it: the card
    // database stores `None` for it (`back_face_for_card_face_with_printed_ref`). That `None`
    // is classified double-faced below, which fails closed; no oracle id in the current card
    // export reaches that fallback (every two-face oracle id has a mapped layout).
    if face.layout_kind == Some(LayoutKind::Single) {
        return None;
    }
    if layout_is_single_faced_alternative(face.layout_kind) {
        return None;
    }
    Some(face)
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
    let Some(other_face) = face_swap_subject(state, object_id)? else {
        return Ok(());
    };

    // CR 613.7g: a double-faced permanent receives a new timestamp when it
    // transforms. All blocked/no-op early-returns above (off-battlefield,
    // CantTransform, meld, no back face) precede this, so they draw no timestamp.
    let ts = state.next_timestamp();
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.timestamp = ts;
    // CR 701.27f: Track successful transforms/conversions to ignore stale
    // self-transform instructions from abilities already on the stack.
    obj.transformation_count = obj.transformation_count.wrapping_add(1);

    swap_displayed_face(state, object_id, other_face);
    events.push(GameEvent::Transformed { object_id });

    Ok(())
}

/// CR 712.14a + CR 702.145b: Turn a permanent that is entering the battlefield
/// "transformed" back face up. It enters that way rather than transforming
/// (CR 701.27a), so it keeps its entry timestamp (CR 613.7d), counts no
/// CR 701.27f transform, and emits no `GameEvent::Transformed`.
pub(crate) fn enter_back_face_up(
    state: &mut GameState,
    object_id: ObjectId,
) -> Result<(), EngineError> {
    let Some(other_face) = face_swap_subject(state, object_id)? else {
        return Ok(());
    };
    swap_displayed_face(state, object_id, other_face);
    Ok(())
}

/// The face a battlefield permanent would turn to, or `None` when a face-swap
/// instruction is a silent no-op for it.
fn face_swap_subject(
    state: &GameState,
    object_id: ObjectId,
) -> Result<Option<BackFaceData>, EngineError> {
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
        return Ok(None);
    }
    obj.back_face
        .clone()
        .map(Some)
        .ok_or_else(|| EngineError::InvalidAction("Card has no back face".to_string()))
}

/// CR 701.27a + CR 707.2 + CR 613.1: turn `obj` to `other_face`, storing the
/// face it turns away as that face's copiable values — its base fields, never
/// its layer-modified live values. Shared by the transform and its CR 733
/// journal replay so both store the same face. Not `swap_object_faces`: the
/// transform stash writes `layout_kind: None` and the symmetric swap keeps the
/// stored slot's layout.
fn turn_to_other_face(obj: &mut GameObject, other_face: BackFaceData) {
    obj.sync_missing_base_characteristics();
    let displayed = snapshot_object_base_face(obj);
    apply_back_face_to_object(obj, other_face);
    obj.back_face = Some(displayed);
}

/// Swaps the displayed face and journals the swap with the object's settled
/// timestamp and transformation count.
fn swap_displayed_face(state: &mut GameState, object_id: ObjectId, other_face: BackFaceData) {
    let obj = state.objects.get_mut(&object_id).unwrap();
    turn_to_other_face(obj, other_face);
    obj.transformed = !obj.transformed;

    let reference = ObjectIncarnationRef::from_object(obj);
    let resulting_transformed = obj.transformed;
    let resulting_timestamp = obj.timestamp;
    let resulting_transformation_count = obj.transformation_count;

    crate::game::layers::mark_layers_full(state);

    // CR 733: journal the settled face swap through its owning family. Every
    // no-op guard returned before the swap, so only a swap that actually changed
    // the displayed face is recorded.
    let cause = state.current_or_begin_rules_execution_node();
    state
        .resolved_rules_journal
        .record_object_transform(ResolvedObjectTransformCommand {
            object: reference,
            expected_old_transformed: !resulting_transformed,
            resulting_transformed,
            resulting_timestamp,
            resulting_transformation_count,
            cause,
        })
        .expect("resolved transform must have a live journal cause");
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
    turn_to_other_face(obj, back_face);
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
/// `transformed` flag, because the transform stash hard-codes `layout_kind: None`
/// when the front face is stored in `back_face` during a transform.
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
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
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

    /// CR 707.2 + CR 613.1 + CR 701.27a: the face a transform turns away is stored
    /// as its copiable values — its base fields — never its layer-modified live
    /// ones, so continuous effects apply over it again from its printed values
    /// when it returns. The nearest printed board is Grub, Storied Matriarch
    /// under Goblin Chieftain ("Other Goblin creatures you control get +1/+1 and
    /// have haste."); the real board is the `grub_storied_matriarch_769`
    /// integration row. This mirrors `flip::tests::
    /// flip_stashes_printed_characteristics_not_layer_modified_ones`.
    ///
    /// Case (ii): no printed card reaches a transform with an uninitialized
    /// base: every card object is installed by `apply_card_face_to_object`, and
    /// a token's first layer pass seeds its base before it can be transformed.
    /// The nearest printed shape is the Incubator token from Blighted
    /// Burgeoning ("When this Aura enters, incubate 2. (Create an Incubator
    /// token with two +1/+1 counters on it and "{2}: Transform this token." It
    /// transforms into a 0/0 Phyrexian artifact creature.)"). This mirrors
    /// `printed_cards::tests::swap_object_faces_stores_copiable_values_in_every_zone`
    /// case (ii) for the transform writer.
    ///
    /// Revert-failing: let the transform stash the live snapshot again
    /// (`snapshot_object_face`) and the stored front face is 3/4 with haste, so
    /// the restored front face is 3/4 with haste in both its live and base
    /// fields — every P/T and keyword assertion in (i) fails. Drop
    /// `turn_to_other_face`'s `sync_missing_base_characteristics` call and (ii)
    /// stores no P/T and no card types.
    #[test]
    fn transform_stashes_copiable_values_not_layer_modified_ones() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        {
            // Stand in for an active continuous effect (+1/+1 and a granted
            // keyword): live fields inflated, `base_*` untouched.
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(4);
            obj.keywords.push(Keyword::Haste);
        }
        let mut events = Vec::new();

        transform_permanent(&mut state, id, &mut events).unwrap();
        let obj = &state.objects[&id];
        assert_eq!(obj.name, "Werewolf Back", "reach-guard: it transformed");
        let stored = obj.back_face.as_ref().expect("the front face is stored");
        assert_eq!(stored.name, "Werewolf Front");
        assert_eq!(
            (stored.power, stored.toughness),
            (Some(2), Some(3)),
            "CR 707.2: the stored face holds its printed 2/3, not the inflated 3/4"
        );
        assert_eq!(
            stored.keywords,
            vec![Keyword::Vigilance],
            "CR 707.2: a granted keyword is not a copiable value"
        );

        transform_permanent(&mut state, id, &mut events).unwrap();
        let obj = &state.objects[&id];
        assert_eq!(
            obj.name, "Werewolf Front",
            "reach-guard: it transformed back"
        );
        assert_eq!((obj.power, obj.toughness), (Some(2), Some(3)));
        assert_eq!((obj.base_power, obj.base_toughness), (Some(2), Some(3)));
        assert_eq!(obj.keywords, vec![Keyword::Vigilance]);
        assert_eq!(obj.base_keywords, vec![Keyword::Vigilance]);

        // (ii) A fixture whose base no layer pass has seeded: live-only writes.
        let fixture = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Unseeded Front".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&fixture).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Incubator".to_string());
            obj.power = Some(2);
            obj.toughness = Some(2);
            let mut back = crate::game::printed_cards::snapshot_object_face(obj);
            back.name = "Unseeded Back".to_string();
            obj.back_face = Some(back);
            assert!(
                !obj.base_characteristics_initialized
                    && obj.base_power.is_none()
                    && obj.base_card_types == CardType::default(),
                "reach-guard: the fixture's base is unseeded"
            );
        }

        transform_permanent(&mut state, fixture, &mut events).unwrap();
        let obj = &state.objects[&fixture];
        assert_eq!(obj.name, "Unseeded Back", "reach-guard: it transformed");
        let stored = obj.back_face.as_ref().expect("the front face is stored");
        assert_eq!(stored.name, "Unseeded Front");
        assert_eq!(
            (stored.power, stored.toughness),
            (Some(2), Some(2)),
            "CR 613.1: the stored face holds the unseeded face's P/T"
        );
        assert_eq!(stored.card_types.core_types, vec![CoreType::Artifact]);
        assert_eq!(stored.card_types.subtypes, vec!["Incubator".to_string()]);
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

    /// CR 712.1 + CR 111.10i: `stored_double_face` answers "does `back_face` hold the other face
    /// of a double-faced card or token?", one row per gate. Each gated row starts from a plain
    /// object with a stored face and moves exactly one field, so only its gate can explain its
    /// verdict. Every row is checked and the mismatches are reported together.
    ///
    /// Nearest printed cards: (a)/(b) Kessig Prowler // Sinuous Predator; (c) Valentin, Dean of
    /// the Vein // Lisette, Dean of the Root; (d) Bruna, the Fading Light; (e) the Incubator token
    /// from Blighted Burgeoning's "incubate 2"; (f) Fire // Ice, Akki Lavarunner // Tok-Tok,
    /// Volcano Born, Bonecrusher Giant // Stomp, Dirgur Island Dragon // Skimming Strike and
    /// Adventurous Eater // Have a Bite; (h) a card manifested by Soul Summons; (i) Lae'zel,
    /// Githyanki Warrior after it specializes; (j) a token copy of Bottomless Pool // Locker Room
    /// under a Layer-1 copy of a creature; (k) Gemrazer mutated onto Kessig Prowler. (g) is
    /// synthetic: no production writer stores a `Single` layout.
    ///
    /// Revert-failing, one row per gate: drop the face-down gate ⇒ (h) is `Some`; the specialize
    /// gate ⇒ (i); the Room gate ⇒ (j); the merged gate ⇒ (k)'s Mutate and Augment rows; the
    /// `Single` gate ⇒ (g); replace `layout_is_single_faced_alternative` with `false` ⇒ every
    /// (f) row. (b) and (e) are the positive reach for the no-layout arm.
    #[test]
    fn stored_double_face_classifies_each_stored_face() {
        use crate::game::game_object::MergeKind;
        use crate::types::card::LayoutKind;

        let mut state = GameState::new_two_player(42);
        let with_face = |state: &mut GameState, name: &str, layout: Option<LayoutKind>| {
            let id = create_object(
                state,
                CardId(3),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.back_face = Some(BackFaceData {
                name: format!("{name} other face"),
                layout_kind: layout,
                ..Default::default()
            });
            id
        };
        let mut rows: Vec<(String, ObjectId, bool)> = Vec::new();

        // (a) a transforming double-faced card, front face up.
        let prowler = setup_dfc(&mut state);
        state
            .objects
            .get_mut(&prowler)
            .unwrap()
            .back_face
            .as_mut()
            .unwrap()
            .layout_kind = Some(LayoutKind::Transform);
        rows.push(("(a) Transform, front face up".into(), prowler, true));

        // (b) the same card after it transforms: its front face is stashed with no layout.
        let transformed = setup_dfc(&mut state);
        state
            .objects
            .get_mut(&transformed)
            .unwrap()
            .back_face
            .as_mut()
            .unwrap()
            .layout_kind = Some(LayoutKind::Transform);
        transform_permanent(&mut state, transformed, &mut Vec::new()).unwrap();
        assert_eq!(
            state.objects[&transformed]
                .back_face
                .as_ref()
                .map(|face| face.layout_kind),
            Some(None),
            "(b) reach-guard: the transform stash has no layout"
        );
        rows.push((
            "(b) transformed, stash with no layout".into(),
            transformed,
            true,
        ));

        // (c) a modal double-faced card; (d) a meld card that has not melded.
        let modal = with_face(&mut state, "Valentin", Some(LayoutKind::Modal));
        rows.push(("(c) Modal".into(), modal, true));
        let meld_card = with_face(&mut state, "Bruna", Some(LayoutKind::Meld));
        rows.push(("(d) Meld face, not merged".into(), meld_card, true));

        // (e) an Incubator token from the production incubate resolver.
        let earlier: Vec<ObjectId> = state.battlefield.iter().copied().collect();
        crate::game::effects::incubate::resolve(
            &mut state,
            &crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::Incubate {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                },
                vec![],
                ObjectId(99),
                PlayerId(0),
            ),
            &mut Vec::new(),
        )
        .unwrap();
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        let incubator = *state
            .battlefield
            .iter()
            .find(|id| !earlier.contains(id))
            .expect("incubate created an Incubator token");
        assert_eq!(
            state.objects[&incubator]
                .back_face
                .as_ref()
                .map(|face| (face.name.as_str(), face.layout_kind)),
            Some(("Phyrexian Token", None)),
            "(e) reach-guard: the payload stores the Phyrexian face with no layout"
        );
        rows.push(("(e) Incubator token".into(), incubator, true));

        // (f) single-faced cards that keep alternative characteristics in `back_face`.
        for layout in [
            LayoutKind::Split,
            LayoutKind::Flip,
            LayoutKind::Adventure,
            LayoutKind::Omen,
            LayoutKind::Prepare,
        ] {
            let id = with_face(&mut state, "Alternative", Some(layout));
            rows.push((format!("(f) {layout:?}"), id, false));
        }

        // (g) a stored `Single` layout.
        let single = with_face(&mut state, "Single", Some(LayoutKind::Single));
        rows.push(("(g) Single".into(), single, false));

        // (h) face down: `back_face` is the face-down stash.
        let face_down = with_face(&mut state, "Manifested", None);
        state.objects.get_mut(&face_down).unwrap().face_down = true;
        rows.push(("(h) face down".into(), face_down, false));

        // (i) specialized: `back_face` is the face before specializing.
        let specialized = with_face(&mut state, "Lae'zel", None);
        state
            .objects
            .get_mut(&specialized)
            .unwrap()
            .specialized_color = Some(ManaColor::Red);
        rows.push(("(i) specialized".into(), specialized, false));

        // (j) a Room-copy token: printed Room, stored half with no layout, and a live type
        // line without Room, as under a Layer-1 copy of a creature.
        let room_copy = with_face(&mut state, "Bottomless Pool", None);
        {
            let obj = state.objects.get_mut(&room_copy).unwrap();
            obj.is_token = true;
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Room".to_string()],
            };
            assert!(
                !obj.card_types.subtypes.iter().any(|s| s == "Room"),
                "(j) reach-guard: the live type line has no Room"
            );
        }
        rows.push((
            "(j) Room copy, live type line not a Room".into(),
            room_copy,
            false,
        ));

        // (k) merged permanents over a transforming face.
        for (kind, expected) in [
            (MergeKind::Mutate, false),
            (MergeKind::Augment, false),
            (MergeKind::Meld, true),
        ] {
            let id = with_face(&mut state, "Merged", Some(LayoutKind::Transform));
            state.objects.get_mut(&id).unwrap().merge_kind = Some(kind);
            rows.push((format!("(k) merged by {kind:?}"), id, expected));
        }

        // (l) no stored face.
        let plain = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Plain".to_string(),
            Zone::Battlefield,
        );
        rows.push(("(l) no back_face".into(), plain, false));

        let mismatches: Vec<String> = rows
            .iter()
            .filter_map(|(row, id, expected)| {
                let found = stored_double_face(&state.objects[id]);
                (found.is_some() != *expected).then(|| {
                    format!(
                        "{row}: expected {}, got {:?}",
                        if *expected { "Some" } else { "None" },
                        found.map(|face| &face.name)
                    )
                })
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "stored_double_face misclassified: {mismatches:#?}"
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
