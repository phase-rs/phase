use crate::game::game_object::{EmblemSource, LinkedAbilitySource};
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityProvenance, Effect, EffectError, EffectKind, ResolvedAbility,
    StaticDefinition, TriggerDefinition,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::sync::Arc;

/// CR 114.1 + CR 114.4: Single authority for emblem-object construction. Creates
/// an emblem in `owner`'s command zone and installs the given static, triggered,
/// and activated abilities so they function from the command zone (CR 114.4).
///
/// Returns the new emblem's `ObjectId` so callers can set display-only
/// `emblem_source` provenance. `grant_emblem` does NOT set `emblem_source`
/// because it has no ability source of its own.
///
/// This helper does NOT read the card database (`GameState::card_db`); the
/// Momir emblem's random creature is drawn only when its ability resolves,
/// which is why `deck_loading` can grant the emblem before the database handle
/// is installed.
pub fn grant_emblem(
    state: &mut GameState,
    owner: PlayerId,
    mut statics: Vec<StaticDefinition>,
    triggers: Vec<TriggerDefinition>,
    abilities: Vec<AbilityDefinition>,
) -> ObjectId {
    // CR 114.4 + CR 113.6b: static abilities on emblems function from the
    // command zone; stamp the zone explicitly so permission readers that gate
    // on `active_zones` (graveyard play/cast permissions) see the static.
    for static_def in &mut statics {
        if !static_def.active_zones.contains(&Zone::Command) {
            static_def.active_zones.push(Zone::Command);
        }
    }

    // CR 114.1: Create emblem in command zone owned by `owner`.
    let emblem_id = create_object(state, CardId(0), owner, "Emblem".to_string(), Zone::Command);
    let obj = state.objects.get_mut(&emblem_id).unwrap();
    // CR 114.5: An emblem is neither a card nor a permanent. Setting `is_emblem`
    // BEFORE installing ability definitions is load-bearing:
    // `functioning_abilities::object_functions` uses this flag to admit
    // command-zone objects, so the first trigger/static scan after creation
    // sees the emblem's abilities.
    obj.is_emblem = true;
    // CR 114.4 + CR 611.1: static abilities function from the command zone.
    obj.static_definitions = statics.clone().into();
    obj.base_static_definitions = Arc::new(statics);
    // CR 113.1c + CR 114.4: install triggered abilities so
    // `active_trigger_definitions` yields them during command-zone scans.
    obj.install_trigger_base_definitions(Arc::new(triggers))
        .expect("trigger base-set generation must not overflow");
    // CR 113.1b + CR 114.4: install activated abilities so they can be activated
    // from the command zone (e.g. the Momir Basic emblem ability).
    obj.abilities = Arc::new(abilities.clone());
    obj.base_abilities = Arc::new(abilities);

    // CR 114.1 + CR 611.1: An emblem can source continuous effects; conservatively
    // request a full layer re-evaluation.
    crate::game::layers::mark_layers_full(state);
    emblem_id
}

/// CR 607.1d + CR 607.5 + CR 113.7a: the pairing identity the new emblem
/// latches — the creating object's exact incarnation (push-time capture, else
/// live) and the copiable set of the ability that created the emblem: that
/// ability's own provenance when it has one, else (an "As ~ enters"
/// replacement, CR 614.1c, or a spell) the creator's set as the emblem is
/// created. An emblem created by a layer-6 granted ability (CR 607.1a
/// self-grants included), or by a trigger whose occurrence carries no
/// attributable set, is not attributed and pairs with nothing (fail closed).
fn emblem_link_binding(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<LinkedAbilitySource> {
    let source = state.objects.get(&ability.source_id)?;
    let creator = ObjectIncarnationRef::of(
        source.id,
        ability.source_incarnation.unwrap_or(source.incarnation),
    );
    let characteristic_set = match ability.source_ability_provenance() {
        Some(AbilityProvenance::Characteristic(set)) => set,
        Some(AbilityProvenance::Granted) => return None,
        None if ability.trigger_definition_ref.is_some() => return None,
        None => source.characteristic_set(),
    };
    Some(LinkedAbilitySource {
        creator,
        characteristic_set,
    })
}

/// CR 114.1 + CR 114.4: Create an emblem in the command zone with the given
/// abilities (statics and triggers). Emblems are not permanents — they cannot
/// be destroyed, exiled, bounced, or sacrificed. Per CR 114.4, both static
/// and triggered abilities function from the command zone.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (statics, triggers) = match &ability.effect {
        Effect::CreateEmblem { statics, triggers } => (statics, triggers),
        _ => return Err(EffectError::MissingParam("CreateEmblem".into())),
    };

    // CR 114: Capture display-only provenance from the ability's source (the
    // planeswalker/spell that created the emblem) BEFORE borrowing the emblem
    // mutably. The client renders the emblem as a chip bearing the source's art
    // crop + name; an emblem has no art of its own (CR 114.5). Read here while
    // the source still exists on the stack/battlefield — it may leave later.
    let emblem_source = state
        .objects
        .get(&ability.source_id)
        .map(|src| EmblemSource {
            name: src.name.clone(),
            printed_ref: src.printed_ref.clone(),
        });

    // CR 607.1d + CR 607.5 + CR 400.7: latch the creating object's exact
    // incarnation and the copiable set of the ability creating the emblem.
    let linked_ability_source = emblem_link_binding(state, ability);

    // CR 114.1: Create the emblem via the single-authority helper. No activated
    // abilities for the planeswalker/spell emblem path — only statics + triggers.
    let emblem_id = grant_emblem(
        state,
        ability.controller,
        statics.clone(),
        triggers.clone(),
        Vec::new(),
    );
    // CR 114: set display-only provenance captured above (grant_emblem leaves it
    // unset because it has no ability source of its own), and the CR 607.1d
    // pairing identity latched above.
    let emblem = state.objects.get_mut(&emblem_id).unwrap();
    emblem.emblem_source = emblem_source;
    emblem.linked_ability_source = linked_ability_source;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        BounceSelection, CharacteristicSetRef, ContinuousModification, ControllerRef,
        StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::statics::{CastFreeOrigin, CastFrequency, StaticMode};

    fn ninja_pump_static() -> StaticDefinition {
        StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Subtype(
                    "Ninja".to_string(),
                )],
                controller: Some(ControllerRef::You),
                properties: vec![],
            })),
            modifications: vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            condition: None,
            per_player_condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: vec![],
            characteristic_defining: false,
            description: None,
            attack_defended: None,
            source_controller: None,
            source_object: None,
            bypass_beneficiary: None,
            protection_does_not_remove: None,
            room_door: None,
        }
    }

    #[test]
    fn create_emblem_stamps_command_zone_on_graveyard_permission_statics() {
        let graveyard_play = StaticDefinition::new(StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: crate::types::ability::CardPlayMode::Play,
            graveyard_destination_replacement: None,
            extra_cost: None,
            enters_with_counter: None,
            required_cast_keyword: None,
        })
        .affected(TargetFilter::Typed(TypedFilter::new(
            crate::types::ability::TypeFilter::Land,
        )));
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![graveyard_play],
                triggers: Vec::new(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let emblem_id = state.command_zone[0];
        let static_def = &state.objects[&emblem_id].static_definitions[0];
        assert!(
            static_def.active_zones.contains(&Zone::Command),
            "emblem graveyard permissions must function from the command zone"
        );
    }

    #[test]
    fn create_emblem_creates_object_in_command_zone() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Emblem should be in command zone
        assert_eq!(state.command_zone.len(), 1);
        let emblem_id = state.command_zone[0];
        let emblem = state.objects.get(&emblem_id).unwrap();
        assert!(emblem.is_emblem);
        assert_eq!(emblem.zone, Zone::Command);
        assert_eq!(emblem.controller, PlayerId(0));
        assert_eq!(emblem.static_definitions.len(), 1);
        assert_eq!(emblem.base_static_definitions.len(), 1);
    }

    #[test]
    fn create_emblem_captures_source_provenance() {
        // CR 114: the emblem records its source's display name + printed_ref so
        // the client can render the source's art crop as a chip. The emblem has
        // no art of its own (CR 114.5), so this provenance is the only handle
        // the display layer has on "where it came from".
        use crate::types::card::PrintedCardRef;
        let mut state = GameState::new_two_player(42);

        // A planeswalker-style source on the battlefield with a printed ref.
        let source_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Jace, the Mind Sculptor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source_id).unwrap().printed_ref = Some(PrintedCardRef {
            oracle_id: "jace-oracle".to_string(),
            face_name: "Jace, the Mind Sculptor".to_string(),
        });

        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let emblem = state.objects.get(&state.command_zone[0]).unwrap();
        let provenance = emblem
            .emblem_source
            .as_ref()
            .expect("emblem records source provenance");
        assert_eq!(provenance.name, "Jace, the Mind Sculptor");
        assert_eq!(
            provenance.printed_ref.as_ref().unwrap().oracle_id,
            "jace-oracle"
        );
    }

    #[test]
    fn create_emblem_marks_layers_dirty() {
        let mut state = GameState::new_two_player(42);
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.layers_dirty.is_dirty());
    }

    /// Helper: create an emblem and return its ObjectId
    fn create_test_emblem(state: &mut GameState) -> ObjectId {
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(state, &ability, &mut events).unwrap();
        state.command_zone[0]
    }

    #[test]
    fn destroy_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::destroy::resolve(&mut state, &ability, &mut events).unwrap();

        // Emblem still exists in command zone
        assert!(state.command_zone.contains(&emblem_id));
        assert!(state.objects.contains_key(&emblem_id));
    }

    #[test]
    fn change_zone_exile_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Command),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::change_zone::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
        assert_eq!(state.objects[&emblem_id].zone, Zone::Command);
    }

    #[test]
    fn bounce_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::bounce::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
    }

    #[test]
    fn sacrifice_targeting_emblem_is_noop() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_test_emblem(&mut state);

        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![crate::types::ability::TargetRef::Object(emblem_id)],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        super::super::sacrifice::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.command_zone.contains(&emblem_id));
    }

    #[test]
    fn create_emblem_installs_triggered_abilities_on_command_zone_emblem() {
        // CR 113.1c + CR 114.4: An emblem-hosted triggered ability must be
        // installed as a `TriggerDefinition` on the emblem object, with both
        // the live and base stores populated so clones and layer resets
        // preserve the trigger.
        use crate::types::triggers::TriggerMode;
        let mut state = GameState::new_two_player(42);
        let trig = crate::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
            .trigger_zones(vec![Zone::Command]);
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: Vec::new(),
                triggers: vec![trig.clone()],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let emblem_id = state.command_zone[0];
        let emblem = state.objects.get(&emblem_id).unwrap();
        assert!(emblem.is_emblem);
        assert_eq!(emblem.trigger_definitions.len(), 1);
        assert_eq!(emblem.base_trigger_definitions.len(), 1);
        // CR 114.4 gate: `active_trigger_definitions` must yield the trigger
        // because `is_emblem` is set.
        let count =
            crate::game::functioning_abilities::active_trigger_definitions(&state, emblem).count();
        assert_eq!(count, 1, "command-zone emblem trigger must be active");
    }

    /// CR 114.4 + CR 601.2b (issue #1355): Tamiyo, Field Researcher's emblem
    /// installs a functioning `CastFromHandFree` static in the command zone.
    #[test]
    fn create_tamiyo_emblem_grants_hand_free_cast_permission() {
        use crate::game::casting::{can_cast_object_now, effective_spell_cost};
        use crate::parser::oracle_static::parse_static_line;
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        use crate::types::card_type::CoreType;
        use crate::types::mana::{ManaCost, ManaCostShard};
        use std::sync::Arc;

        let static_def = parse_static_line(
            "You may cast spells from your hand without paying their mana costs.",
        )
        .expect("Tamiyo emblem static should parse");
        assert!(
            matches!(
                static_def.mode,
                StaticMode::CastFromHandFree {
                    frequency: CastFrequency::Unlimited,
                    origin: CastFreeOrigin::Hand,
                }
            ),
            "expected CastFromHandFree static, got {:?}",
            static_def.mode
        );

        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![static_def],
                triggers: Vec::new(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let emblem_id = state.command_zone[0];
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Counterspell".to_string(),
                    description: None,
                },
            ));
        }

        let cost = effective_spell_cost(&state, PlayerId(0), spell_id)
            .expect("hand spell cost should compute");
        assert!(
            matches!(cost, ManaCost::NoCost),
            "Tamiyo emblem should zero the hand spell's mana cost, got {cost:?}"
        );
        assert!(can_cast_object_now(&state, PlayerId(0), spell_id));
        assert_eq!(
            crate::game::casting::hand_cast_free_permission_source(
                &state,
                PlayerId(0),
                state.objects.get(&spell_id).unwrap(),
            ),
            Some((emblem_id, CastFrequency::Unlimited)),
            "permission source should be the created emblem"
        );
    }

    /// CR 607.1d + CR 400.7 + CR 113.7a: the emblem latches the exact object that
    /// created it — the ability's push-time incarnation capture when present,
    /// else the live incarnation (an as-enters replacement drain has no stack
    /// capture).
    #[test]
    fn create_emblem_latches_creator_incarnation() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Tibalt, Cosmic Impostor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source_id).unwrap().incarnation = 3;
        let mut ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let live = state.command_zone[0];
        assert_eq!(
            state.objects[&live].linked_ability_source,
            Some(LinkedAbilitySource {
                creator: ObjectIncarnationRef::of(source_id, 3),
                characteristic_set: CharacteristicSetRef::Own,
            }),
            "no stack capture: the live incarnation is latched"
        );

        ability.source_incarnation = Some(2);
        resolve(&mut state, &ability, &mut events).unwrap();
        let captured = state.command_zone[1];
        assert_eq!(
            state.objects[&captured].linked_ability_source,
            Some(LinkedAbilitySource {
                creator: ObjectIncarnationRef::of(source_id, 2),
                characteristic_set: CharacteristicSetRef::Own,
            }),
            "the push-time capture wins over the live incarnation"
        );
    }

    /// CR 607.1d + CR 607.5 + CR 113.7a: the emblem latches the copiable set of
    /// the ability that created it — the ability's own provenance when it has
    /// one, else the creator's live layer-1 set — and an emblem created by a
    /// granted or unattributable ability pairs with nothing.
    #[test]
    fn create_emblem_latches_characteristic_set() {
        use crate::types::ability::{
            CopyEffectInstanceRef, Duration, TriggerBaseSetInstanceRef,
            TriggerDefinitionOccurrenceRef, TriggerDefinitionRef, TriggerGrantInstanceRef,
        };

        fn setup() -> (GameState, ObjectId, ResolvedAbility) {
            let mut state = GameState::new_two_player(42);
            let source_id = create_object(
                &mut state,
                CardId(7),
                PlayerId(0),
                "Creator".to_string(),
                Zone::Battlefield,
            );
            let ability = ResolvedAbility::new(
                Effect::CreateEmblem {
                    statics: vec![ninja_pump_static()],
                    triggers: Vec::new(),
                },
                vec![],
                source_id,
                PlayerId(0),
            );
            (state, source_id, ability)
        }
        fn latched(state: &GameState) -> Option<LinkedAbilitySource> {
            let emblem = *state.command_zone.last().expect("an emblem was created");
            state.objects[&emblem].linked_ability_source
        }
        let mut events = Vec::new();

        // (i) Reach guard: no provenance, no copy effect → the object's own set.
        let (mut state, source_id, ability) = setup();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            latched(&state),
            Some(LinkedAbilitySource {
                creator: ObjectIncarnationRef::from_object(&state.objects[&source_id]),
                characteristic_set: CharacteristicSetRef::Own,
            })
        );

        // (ii) The creator is under an applied layer-1 copy effect → that set.
        let (mut state, source_id, ability) = setup();
        let donor = create_object(
            &mut state,
            CardId(8),
            PlayerId(0),
            "Donor".to_string(),
            Zone::Battlefield,
        );
        let values = crate::game::printed_cards::intrinsic_copiable_values(&state.objects[&donor]);
        let copy_id = state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: source_id },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display_source: crate::game::game_object::DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert_eq!(
            state.objects[&source_id].name, "Donor",
            "reach: copy applied"
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let copy_set = CharacteristicSetRef::Copied(CopyEffectInstanceRef {
            continuous_effect_id: copy_id,
            modification_index: 0,
        });
        assert_eq!(
            latched(&state).map(|link| link.characteristic_set),
            Some(copy_set)
        );

        // (iii) The creating ability's own provenance wins over the live set.
        let (mut state, _, mut ability) = setup();
        let own_copy = CharacteristicSetRef::Copied(CopyEffectInstanceRef {
            continuous_effect_id: 91,
            modification_index: 0,
        });
        ability.set_source_ability_provenance_recursive(Some(AbilityProvenance::Characteristic(
            own_copy,
        )));
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            latched(&state).map(|link| link.characteristic_set),
            Some(own_copy)
        );

        // (iv) A granted creating ability is not attributed.
        let (mut state, _, mut ability) = setup();
        ability.set_source_ability_provenance_recursive(Some(AbilityProvenance::Granted));
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(latched(&state), None);

        // (v) A trigger whose occurrence carries no attributable set is not
        // attributed either.
        let (mut state, source_id, mut ability) = setup();
        ability.trigger_definition_ref = Some(TriggerDefinitionRef {
            source: ObjectIncarnationRef::from_object(&state.objects[&source_id]),
            occurrence: TriggerDefinitionOccurrenceRef::CopyRetained {
                grant_instance: TriggerGrantInstanceRef(1),
                source_base_set: TriggerBaseSetInstanceRef::INITIAL,
                source_printed_index: 0,
            },
        });
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(latched(&state), None);
    }

    /// Serialized surface: persisted emblems written before the creator field
    /// existed load with `None`, and a set field round-trips.
    #[test]
    fn emblem_creator_field_defaults_when_absent() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Tibalt, Cosmic Impostor".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let emblem = &state.objects[&state.command_zone[0]];
        let expected = Some(LinkedAbilitySource {
            creator: ObjectIncarnationRef::from_object(&state.objects[&source_id]),
            characteristic_set: CharacteristicSetRef::Own,
        });
        assert_eq!(emblem.linked_ability_source, expected);

        let mut json = serde_json::to_value(emblem).unwrap();
        let round_trip: crate::game::GameObject = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(round_trip.linked_ability_source, expected);

        let removed = json
            .as_object_mut()
            .unwrap()
            .remove("linked_ability_source");
        assert!(removed.is_some(), "the set field is serialized");
        let legacy: crate::game::GameObject = serde_json::from_value(json).unwrap();
        assert_eq!(legacy.linked_ability_source, None);
    }
}
