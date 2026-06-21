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
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCondition, ControllerRef, TargetRef};
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType};
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
    fn turn_face_up_targets_battlefield_face_down_creature_via_filter() {
        // CR 708.7 + CR 708.8 (Bustle / Expose the Culprit class): a resolving "turn
        // <target> face up" effect aimed at a battlefield face-down creature
        // restores that permanent's real characteristics. No explicit target is
        // pre-resolved — the resolver must locate it by the face-down filter on
        // the battlefield. Reverting the parser/resolver wiring (so the target
        // never reaches the battlefield arm) leaves the permanent face down and
        // flips the `!face_down`/name assertions below.
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Build the real creature, snapshot its face, then push it face down on
        // the battlefield — mirroring the manifest/morph battlefield entry so
        // back_face holds the genuine BackFaceData the resolver restores.
        let id = create_object(
            &mut state,
            CardId(7),
            player,
            "Real Beast".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(5);
            obj.toughness = Some(4);
            obj.card_types = crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Beast".to_string()],
            };
            let snapshot = crate::game::printed_cards::snapshot_object_face(obj);
            crate::game::morph::apply_face_down_creature_characteristics(
                obj,
                &crate::types::ability::FaceDownProfile::vanilla_2_2(),
            );
            obj.back_face = Some(snapshot);
        }

        let face_down_creature = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature()
                .properties(vec![crate::types::ability::FilterProp::FaceDown]),
        );
        let ability = ResolvedAbility::new(
            Effect::TurnFaceUp {
                target: face_down_creature,
            },
            vec![],
            id,
            player,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(
            !obj.face_down,
            "the face-down creature must be turned face up"
        );
        assert_eq!(obj.name, "Real Beast", "real face restored");
        assert_eq!(obj.power, Some(5));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnedFaceUp { object_id } if *object_id == id)));
    }

    #[test]
    fn reveal_then_turn_face_up_chain_turns_up_creature_card() {
        // CR 701.20 + CR 700.1 (Hauntwoods Shrieker reveal ability): the parsed
        // chain is `Reveal { target: face-down permanent }` → (sub, optional,
        // condition RevealedHasCardType{Creature}) `TurnFaceUp { ParentTarget }`.
        // The reveal records `last_revealed_ids`; the engine injects that into
        // the sub-ability's targets and the RevealedHasCardType rider reads it.
        // Reverting the reveal's `last_revealed_ids` write (or removing Reveal
        // from `effect_writes_last_revealed_ids`) leaves the permanent face down
        // and flips the `!face_down` / name assertions.
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(50),
            player,
            "Hauntwoods Shrieker".to_string(),
            Zone::Battlefield,
        );

        // A face-down creature on the battlefield (back_face is a real creature).
        let target_obj = create_object(
            &mut state,
            CardId(51),
            player,
            "Hidden Wurm".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_obj).unwrap();
            obj.power = Some(6);
            obj.toughness = Some(6);
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Wurm".to_string()],
            };
            let snapshot = crate::game::printed_cards::snapshot_object_face(obj);
            crate::game::morph::apply_face_down_creature_characteristics(
                obj,
                &crate::types::ability::FaceDownProfile::vanilla_2_2(),
            );
            obj.back_face = Some(snapshot);
        }

        // Build the parsed chain shape: Reveal(target) → optional TurnFaceUp.
        let turn_up = ResolvedAbility::new(
            Effect::TurnFaceUp {
                target: TargetFilter::ParentTarget,
            },
            vec![],
            source,
            player,
        )
        .condition(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Creature],
            additional_filter: None,
            subtype_filter: None,
        });
        let mut turn_up = turn_up;
        turn_up.optional = true;
        let ability = ResolvedAbility::new(
            Effect::Reveal {
                target: TargetFilter::ParentTarget,
            },
            // The reveal's own target is the face-down permanent (resolved by the
            // targeting pipeline for "reveal target face-down permanent").
            vec![TargetRef::Object(target_obj)],
            source,
            player,
        )
        .sub_ability(turn_up);

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The "you may" turn-up pauses for the controller's choice; accept it.
        if matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::OptionalEffectChoice { .. }
        ) {
            apply_as_current(
                &mut state,
                GameAction::DecideOptionalEffect { accept: true },
            )
            .expect("accept the optional turn-face-up");
        }

        let obj = &state.objects[&target_obj];
        assert!(
            !obj.face_down,
            "the revealed creature card must be turned face up after accepting"
        );
        assert_eq!(obj.name, "Hidden Wurm", "real face restored on turn-up");
        assert_eq!(obj.power, Some(6));
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
