use crate::game::printed_cards::apply_back_face_to_object;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 406.3: Turn the card(s) referenced by `target` face up via a resolving
/// effect — distinct from the morph/disguise *special action* in
/// `game/morph.rs::turn_face_up`. Used by the Imprint "flip" cards — Clone
/// Shell, Summoner's Egg, Compleated Clone Shell, The Creation of Avacyn —
/// which exile a card face down and later "turn the exiled card face up".
///
/// A card exiled face down keeps its real identity in exile (the face-down
/// profile is applied only on battlefield entry — see
/// `zone_pipeline::apply_face_down_entry_profile`), so for those cards clearing
/// the face-down flag makes the card publicly visible and records it as the
/// resolution's revealed object. The conditional follow-up ("if it's a creature
/// card, put it onto the battlefield …") then reads the card's real type and
/// moves it. If a genuinely face-down carrier with a stored `back_face` is
/// targeted, its real characteristics are restored (CR 708.2a).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::TurnFaceUp { target } => target.clone(),
        _ => return Ok(()),
    };

    let object_ids = target_object_ids(state, ability, &target);

    let mut restored_any = false;
    let mut turned_ids = Vec::new();
    for id in object_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            if obj.face_down {
                obj.face_down = false;
                if let Some(back) = obj.back_face.take() {
                    apply_back_face_to_object(obj, back);
                }
                restored_any = true;
                turned_ids.push(id);
                events.push(GameEvent::TurnedFaceUp { object_id: id });
            }
        }
    }

    if !turned_ids.is_empty() {
        state.last_revealed_ids = turned_ids;
    }

    // CR 613: a turned-up card's restored characteristics require a layer
    // re-derive (mirrors the morph special-action path).
    if restored_any {
        crate::game::layers::mark_layers_full(state);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::TurnFaceUp,
        source_id: ability.source_id,
    });
    Ok(())
}

fn target_object_ids(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let resolved = crate::game::targeting::resolved_targets(ability, target, state);
    let explicit = crate::game::effects::effect_object_targets(target, &resolved);
    if !explicit.is_empty() {
        return explicit;
    }

    let zone = target
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    crate::game::targeting::zone_object_ids(state, zone)
        .into_iter()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, target, &ctx))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCondition, ControllerRef, TargetRef};
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::{EtbTapState, Zone};

    fn linked_face_down_creature(state: &mut GameState) -> (ObjectId, ObjectId) {
        let source = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Clone Shell".to_string(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            state,
            CardId(101),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&exiled).unwrap();
            obj.face_down = true;
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }
        state.exile_links.push(ExileLink {
            source_id: source,
            exiled_id: exiled,
            kind: ExileLinkKind::TrackedBySource,
        });
        (source, exiled)
    }

    #[test]
    fn turn_face_up_resolves_implicit_exiled_by_source_and_reveals_it() {
        let mut state = GameState::new_two_player(42);
        let (source, exiled) = linked_face_down_creature(&mut state);
        let ability = ResolvedAbility::new(
            Effect::TurnFaceUp {
                target: TargetFilter::ExiledBySource,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.objects[&exiled].face_down);
        assert_eq!(state.last_revealed_ids, vec![exiled]);
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::TurnedFaceUp { object_id } if *object_id == exiled)
        ));
    }

    #[test]
    fn turn_face_up_chain_feeds_creature_card_condition_and_target() {
        let mut state = GameState::new_two_player(42);
        let (source, exiled) = linked_face_down_creature(&mut state);
        let put_creature = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .condition(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Creature],
            additional_filter: None,
            subtype_filter: None,
        });
        let ability = ResolvedAbility::new(
            Effect::TurnFaceUp {
                target: TargetFilter::ExiledBySource,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(put_creature);

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let obj = &state.objects[&exiled];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.controller, PlayerId(0));
        assert!(!obj.face_down);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to, .. } if *object_id == exiled && *to == Zone::Battlefield)));
    }

    #[test]
    fn turn_face_up_does_not_emit_event_for_already_face_up_card() {
        let mut state = GameState::new_two_player(42);
        let (source, exiled) = linked_face_down_creature(&mut state);
        state.objects.get_mut(&exiled).unwrap().face_down = false;
        let ability = ResolvedAbility::new(
            Effect::TurnFaceUp {
                target: TargetFilter::ExiledBySource,
            },
            vec![TargetRef::Object(exiled)],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.last_revealed_ids.is_empty());
        assert!(!events.iter().any(
            |event| matches!(event, GameEvent::TurnedFaceUp { object_id } if *object_id == exiled)
        ));
    }
}
