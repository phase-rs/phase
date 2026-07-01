use crate::game::game_object::EmblemSource;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition,
    TriggerDefinition,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
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
/// This helper does NOT read any format pool fields (`momir_pool`,
/// `momir_pool_faces`); those are resolution-time-only reads, which is why
/// `deck_loading` can grant the Momir emblem before the pool is built.
pub fn grant_emblem(
    state: &mut GameState,
    owner: PlayerId,
    statics: Vec<StaticDefinition>,
    triggers: Vec<TriggerDefinition>,
    abilities: Vec<AbilityDefinition>,
) -> ObjectId {
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
    obj.trigger_definitions = triggers.clone().into();
    obj.base_trigger_definitions = Arc::new(triggers);
    // CR 113.1b + CR 114.4: install activated abilities so they can be activated
    // from the command zone (e.g. the Momir Basic emblem ability).
    obj.abilities = Arc::new(abilities.clone());
    obj.base_abilities = Arc::new(abilities);

    // CR 114.1 + CR 611.1: An emblem can source continuous effects; conservatively
    // request a full layer re-evaluation.
    crate::game::layers::mark_layers_full(state);
    emblem_id
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
    // unset because it has no ability source of its own).
    state.objects.get_mut(&emblem_id).unwrap().emblem_source = emblem_source;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        BounceSelection, ContinuousModification, ControllerRef, StaticDefinition, TargetFilter,
        TypedFilter,
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
        }
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
}
