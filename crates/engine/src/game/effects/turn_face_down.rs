use crate::types::ability::{Effect, EffectError, EffectKind, FaceDownProfile, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 708.2a + CR 708.2b + CR 712.16: Turn the face-up permanent(s) selected by
/// the resolving ability's `target` slot face down via a spell or ability
/// (Cyber Conversion — "Turn target creature face down. It's a 2/2 Cyberman
/// artifact creature.").
///
/// Each matched permanent that is eligible (CR 708.2b: not already face down;
/// CR 712.16 / CR 730.2j: not a double-faced or melded permanent) becomes the
/// body described by the effect's `profile` (CR 205.1a) — a vanilla 2/2 creature
/// (CR 708.2a sentence 1) when none was specified. Its real face is preserved in
/// `back_face` (mirroring morph/manifest) so it can later be turned face up,
/// restoring its printed characteristics (CR 708.8) — including a real morph
/// ability per the Cyber Conversion ruling. Inverse of
/// [`super::turn_face_up::resolve`]; emits `GameEvent::TurnedFaceDown`
/// (a distinct game action per CR 701.27b).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, profile) = match &ability.effect {
        Effect::TurnFaceDown { target, profile } => (
            target.clone(),
            profile.clone().unwrap_or_else(FaceDownProfile::vanilla_2_2),
        ),
        _ => return Ok(()),
    };

    let mut changed = false;
    for id in crate::game::effects::resolved_battlefield_object_ids(state, ability, &target) {
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        // CR 708.2b: A face-down permanent can't be turned face down — nothing
        // happens and its characteristics are unchanged.
        if obj.face_down {
            continue;
        }
        // CR 712.16 + CR 730.2j: Double-faced and melded permanents already on
        // the battlefield can't be turned face down — nothing happens.
        if crate::game::transform::is_double_faced_permanent(obj) {
            continue;
        }
        // CR 708.2a + CR 708.8 + CR 613: Preserve the real face from the
        // object's printed/base characteristics. Snapshotting from base fields
        // (not the live fields) avoids baking in any continuous-effect
        // modifications (e.g. a +1/+1 anthem that has inflated power/toughness)
        // that are currently active. `apply_back_face_to_object` on turn-up
        // writes these values into both live and base fields, so the layer
        // system then reapplies all continuous effects from the correct printed
        // baseline — not from an already-inflated one.
        let snapshot = crate::game::printed_cards::snapshot_object_base_face(obj);
        // CR 708.2a + CR 205.1a: Apply the effect-specified (or default vanilla
        // 2/2) face-down body.
        crate::game::morph::apply_face_down_creature_characteristics(obj, &profile);
        obj.back_face = Some(snapshot);
        changed = true;
        events.push(GameEvent::TurnedFaceDown { object_id: id });
    }

    // CR 613: the new face-down copiable characteristics (Layer 1) require a
    // full layer re-derive (mirrors the turn-face-up path).
    if changed {
        crate::game::layers::mark_layers_full(state);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::TurnFaceDown,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        FaceDownBody, FilterProp, MultiTargetSpec, TargetFilter, TypedFilter,
    };
    use crate::types::card::LayoutKind;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Face-up vanilla creature on the battlefield (Grizzly Bears, 2/2 Bear).
    fn setup_face_up_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Bear".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        id
    }

    fn turn_face_down_ability(
        target_id: ObjectId,
        profile: Option<FaceDownProfile>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::TurnFaceDown {
                target: TargetFilter::SpecificObject { id: target_id },
                profile,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        )
    }

    /// Cyber Conversion: the Cyberman body overrides the vanilla 2/2 default.
    fn cyberman_profile() -> FaceDownProfile {
        FaceDownProfile {
            power: Some(2),
            toughness: Some(2),
            body: FaceDownBody::Creature,
            extra_core_types: vec![CoreType::Artifact],
            subtypes: vec!["Cyberman".to_string()],
            ward: None,
        }
    }

    #[test]
    fn turns_face_up_creature_into_specified_cyberman_body() {
        // CR 708.2a + CR 205.1a: turn target creature face down as a 2/2 Cyberman
        // artifact creature; the real face is preserved for a later turn-up.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state);
        let ability = turn_face_down_ability(id, Some(cyberman_profile()));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.face_down);
        assert_eq!(obj.name, "");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(
            obj.card_types.core_types,
            vec![CoreType::Creature, CoreType::Artifact]
        );
        assert_eq!(obj.card_types.subtypes, vec!["Cyberman".to_string()]);
        // CR 708.8: the real face is preserved so it can be restored on turn-up.
        assert_eq!(obj.back_face.as_ref().unwrap().name, "Grizzly Bears");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceDown { object_id } if *object_id == id)));
    }

    #[test]
    fn default_profile_is_vanilla_2_2_creature() {
        // CR 708.2a sentence 1: with no specified body, a turned-down permanent is
        // a vanilla 2/2 creature.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state);
        let ability = turn_face_down_ability(id, None);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.face_down);
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.card_types.core_types, vec![CoreType::Creature]);
        assert!(obj.card_types.subtypes.is_empty());
    }

    #[test]
    fn already_face_down_permanent_is_unchanged() {
        // CR 708.2b: a face-down permanent can't be turned face down — nothing
        // happens, and no TurnedFaceDown event is emitted.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state);
        state.objects.get_mut(&id).unwrap().face_down = true;
        let ability = turn_face_down_ability(id, Some(cyberman_profile()));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 708.2b: no characteristics changed and no event fired.
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceDown { .. })));
    }

    #[test]
    fn double_faced_permanent_cannot_be_turned_face_down() {
        // CR 712.16: a DFC already on the battlefield (its back face records the
        // Transform layout) can't be turned face down — nothing happens.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state);
        let snapshot = crate::game::printed_cards::snapshot_object_face(&state.objects[&id]);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            let mut back = snapshot;
            back.layout_kind = Some(LayoutKind::Transform);
            obj.back_face = Some(back);
        }
        let ability = turn_face_down_ability(id, Some(cyberman_profile()));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(!obj.face_down, "a DFC must not become face down");
        assert_eq!(obj.name, "Grizzly Bears");
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceDown { .. })));
    }

    #[test]
    fn meld_permanent_cannot_be_turned_face_down() {
        // CR 730.2j: a face-up melded permanent contains a double-faced component,
        // so it can't be turned face down — nothing happens and no event fires.
        // Exercises the `merge_kind == Some(MergeKind::Meld)` branch of
        // `is_double_faced_permanent`, which the other DFC test (Transform
        // `layout_kind`) does not reach.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state);
        state.objects.get_mut(&id).unwrap().merge_kind =
            Some(crate::game::game_object::MergeKind::Meld);
        let ability = turn_face_down_ability(id, Some(cyberman_profile()));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(
            !obj.face_down,
            "a melded permanent must not become face down"
        );
        assert_eq!(obj.name, "Grizzly Bears");
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceDown { .. })));
    }

    #[test]
    fn snapshot_uses_base_not_live_under_continuous_modifier() {
        // CR 613.1 + CR 708.8: When a permanent is turned face down by a spell or
        // ability, the stored back_face must capture the object's printed/base
        // characteristics — not the live values that may be inflated by an active
        // continuous effect (such as a +1/+1 anthem).
        //
        // Failure shape if live fields were snapshotted: a 2/2 creature pumped to
        // 3/3 by an anthem gets turned face down and stores 3/3 in back_face. On a
        // later turn-up, `apply_back_face_to_object` writes 3/3 into both live and
        // base fields. The layer system then reapplies the anthem from base 3/3,
        // producing 4/4 — a rules violation. With base fields snapshotted, the
        // restored base is 2/2, the anthem reapplies to give 3/3, which is correct.
        //
        // Discriminating: reverting `snapshot_object_base_face` back to
        // `snapshot_object_face` (live fields) makes `snapshot.power` equal to 3
        // and `restored_base_power` equal to 3, flipping the assertions below.
        let mut state = GameState::new_two_player(42);
        let id = setup_face_up_creature(&mut state); // base_power=2, base_toughness=2

        // Simulate a continuous +1/+1 effect: live fields inflated but base unchanged.
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(3);
            // base_power / base_toughness remain Some(2) as set by setup_face_up_creature.
        }

        let ability = turn_face_down_ability(id, Some(cyberman_profile()));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The stored back_face must reflect the printed 2/2, not the live 3/3.
        let snapshot = state.objects[&id]
            .back_face
            .as_ref()
            .expect("back_face must be set after turning face down");
        assert_eq!(
            snapshot.power,
            Some(2),
            "back_face must capture base power (2), not live power inflated by continuous effect (3)"
        );
        assert_eq!(
            snapshot.toughness,
            Some(2),
            "back_face must capture base toughness (2), not live toughness (3)"
        );
        assert_eq!(
            snapshot.name, "Grizzly Bears",
            "back_face must capture the base name"
        );

        // Simulate what turn_face_up::resolve does: restore from back_face.
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.face_down = false;
            let back = obj.back_face.take().unwrap();
            crate::game::printed_cards::apply_back_face_to_object(obj, back);
        }

        // After restoration, live and base fields must reflect the printed 2/2.
        // The layer system (not run in this unit test) would then reapply the
        // anthem to produce 3/3 — the correct CR 708.8 + CR 613 outcome.
        let obj = &state.objects[&id];
        assert!(!obj.face_down, "must be face up after restoration");
        assert_eq!(
            obj.base_power,
            Some(2),
            "restored base_power must be the printed 2/2, not 3/3"
        );
        assert_eq!(
            obj.base_toughness,
            Some(2),
            "restored base_toughness must be the printed 2/2, not 3/3"
        );
        assert_eq!(
            obj.name, "Grizzly Bears",
            "real name must be restored on turn-up"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::TurnedFaceDown { object_id } if *object_id == id)),
            "TurnedFaceDown event must be emitted"
        );
    }

    #[test]
    fn multi_target_zero_chosen_turns_nothing_face_down() {
        // CR 601.2c + CR 608.2b (Illithid Harvester: "turn any number of target
        // tapped nontoken creatures face down"): when the controller chooses ZERO
        // targets, the resolved target set is empty and the effect must affect
        // NOTHING. It must not fall through to the mass/population zone scan and
        // turn every tapped nontoken creature on the battlefield face down.
        //
        // Discriminating: the filter below matches both creatures, so reverting the
        // `multi_target.is_some()` gate in `resolved_battlefield_object_ids` would
        // enumerate and flip both — the `!face_down` assertions and the empty-event
        // assertion all fail.
        let mut state = GameState::new_two_player(42);

        // Two tapped nontoken creatures (one per player) that the filter matches.
        let mine = setup_face_up_creature(&mut state);
        let theirs = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hill Giant".to_string(),
            Zone::Battlefield,
        );
        for id in [mine, theirs] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.tapped = true;
            obj.is_token = false;
            if obj.card_types.core_types.is_empty() {
                obj.card_types.core_types.push(CoreType::Creature);
            }
        }

        // "any number of target tapped nontoken creatures" → multi_target with a
        // fixed-zero minimum and no max, with ZERO targets actually chosen.
        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Tapped, FilterProp::NonToken]),
        );
        let mut ability = ResolvedAbility::new(
            Effect::TurnFaceDown {
                target: filter,
                profile: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::unlimited(0));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for id in [mine, theirs] {
            assert!(
                !state.objects[&id].face_down,
                "a creature not chosen as a target must stay face up"
            );
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::TurnedFaceDown { .. })),
            "no TurnedFaceDown event when zero targets were chosen"
        );
    }
}
