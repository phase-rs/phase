use crate::types::ability::{
    CountValue, Effect, EffectError, EffectKind, ManaProduction, ManaSpendRestriction,
    ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::{ManaColor, ManaRestriction, ManaType, ManaUnit};

/// Mana effect: adds mana to the controller's mana pool.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (produced, restrictions) = match &ability.effect {
        Effect::Mana {
            produced,
            restrictions,
        } => (produced, restrictions),
        _ => return Err(EffectError::MissingParam("Produced".to_string())),
    };
    // ChosenColor needs to read from the source object's chosen_attributes
    let mana_types = match produced {
        ManaProduction::ChosenColor { count } => {
            let amount = resolve_count_value(count) as usize;
            state
                .objects
                .get(&ability.source_id)
                .and_then(|obj| obj.chosen_color())
                .map(|color| vec![mana_color_to_type(&color); amount])
                .unwrap_or_default()
        }
        other => resolve_mana_types(other),
    };

    // Resolve restriction templates into concrete restrictions
    let concrete_restrictions = resolve_restrictions(restrictions, state, ability.source_id);

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    for mana_type in mana_types {
        let unit = ManaUnit {
            color: mana_type,
            source_id: ability.source_id,
            snow: false,
            restrictions: concrete_restrictions.clone(),
        };
        player.mana_pool.add(unit);

        events.push(GameEvent::ManaAdded {
            player_id: ability.controller,
            mana_type,
            source_id: ability.source_id,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Resolve parse-time restriction templates into concrete `ManaRestriction` values.
fn resolve_restrictions(
    templates: &[ManaSpendRestriction],
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<ManaRestriction> {
    templates
        .iter()
        .filter_map(|template| match template {
            ManaSpendRestriction::SpellType(t) => {
                Some(ManaRestriction::OnlyForSpellType(t.clone()))
            }
            ManaSpendRestriction::ChosenCreatureType => state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_creature_type())
                .map(|ct| ManaRestriction::OnlyForCreatureType(ct.to_string())),
        })
        .collect()
}

/// Resolve a typed mana production descriptor into concrete mana units.
///
/// Current limitations:
/// - Variable counts resolve to 0 units.
/// - Chosen-color production resolves to 0 units (chosen-color runtime binding is not implemented).
pub(crate) fn resolve_mana_types(produced: &ManaProduction) -> Vec<ManaType> {
    match produced {
        ManaProduction::Fixed { colors } => colors.iter().map(mana_color_to_type).collect(),
        ManaProduction::Colorless { count } => {
            vec![ManaType::Colorless; resolve_count_value(count) as usize]
        }
        ManaProduction::AnyOneColor {
            count,
            color_options,
        } => {
            let amount = resolve_count_value(count) as usize;
            let Some(mana_type) = color_options.first().map(mana_color_to_type) else {
                return Vec::new();
            };
            vec![mana_type; amount]
        }
        ManaProduction::AnyCombination {
            count,
            color_options,
        } => {
            let amount = resolve_count_value(count) as usize;
            if color_options.is_empty() {
                return Vec::new();
            }
            (0..amount)
                .map(|index| mana_color_to_type(&color_options[index % color_options.len()]))
                .collect()
        }
        ManaProduction::ChosenColor { .. } => Vec::new(),
    }
}

fn resolve_count_value(value: &CountValue) -> u32 {
    match value {
        CountValue::Fixed(n) => *n,
        CountValue::Variable(_) => 0,
    }
}

/// Convert a ManaColor to the runtime ManaType.
fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_mana_ability(produced: ManaProduction) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana {
                produced,
                restrictions: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn produce_multiple_of_same_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green, ManaColor::Green, ManaColor::Green],
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_empty_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed { colors: vec![] }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::White, ManaColor::Blue],
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
            }),
            &mut events,
        )
        .unwrap();

        let mana_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_events.len(), 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
            }),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Mana,
                ..
            }
        )));
    }

    #[test]
    fn empty_produced_adds_no_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed { colors: vec![] }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Red],
            }),
            &mut events,
        )
        .unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.source_id, ObjectId(100));
    }

    #[test]
    fn produce_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::Colorless {
                count: CountValue::Fixed(2),
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn produce_any_one_color_uses_first_option() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyOneColor {
                count: CountValue::Fixed(2),
                color_options: vec![ManaColor::Blue, ManaColor::Red],
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 2);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn produce_any_combination_cycles_options() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::AnyCombination {
                count: CountValue::Fixed(3),
                color_options: vec![ManaColor::Black, ManaColor::Green],
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 2);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].mana_pool.total(), 3);
    }

    #[test]
    fn chosen_color_resolves_from_object_attribute() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(100);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Green));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: CountValue::Fixed(1),
        });
        // Override source_id to match our object
        let ability = ResolvedAbility {
            source_id: obj_id,
            ..ability
        };

        resolve(&mut state, &ability, &mut events).unwrap();

        let player = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(player.mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn chosen_color_unresolved_is_noop() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(ManaProduction::ChosenColor {
                count: CountValue::Fixed(1),
            }),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn restriction_spell_type_attaches_to_produced_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    color_options: vec![ManaColor::Green],
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForSpellType("Creature".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_resolves_from_source() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let obj_id = ObjectId(200);
        let mut obj = crate::game::game_object::GameObject::new(
            obj_id,
            CardId(2),
            PlayerId(0),
            "Cavern of Souls".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));
        state.objects.insert(obj_id, obj);

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    color_options: vec![ManaColor::Green],
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.restrictions.len(), 1);
        assert_eq!(
            unit.restrictions[0],
            ManaRestriction::OnlyForCreatureType("Elf".to_string())
        );
    }

    #[test]
    fn restriction_chosen_creature_type_drops_when_no_choice() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                },
                restrictions: vec![ManaSpendRestriction::ChosenCreatureType],
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // No source object → restriction can't resolve → mana is unrestricted
        let unit = &state.players[0].mana_pool.mana[0];
        assert!(unit.restrictions.is_empty());
    }
}
