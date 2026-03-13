//! Typed object filter matching using TargetFilter enum.
//!
//! Replaces the Forge-style string filter parsing with typed enum matching.
//! All filter logic works against the TargetFilter enum hierarchy from types/ability.rs.

use crate::game::game_object::GameObject;
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Check if an object matches a typed TargetFilter.
///
/// `source_id` is the object that owns the ability (used for SelfRef/Other resolution).
/// The source's controller is looked up from `state` (for You/Opponent).
pub fn matches_target_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    let source_controller = state.objects.get(&source_id).map(|o| o.controller);
    filter_inner(state, object_id, filter, source_id, source_controller)
}

/// Like [`matches_target_filter`], but with an explicit controller.
///
/// Use this when resolving effects from the stack where the source object
/// may no longer exist (e.g. sacrificed as a cost).
pub fn matches_target_filter_controlled(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    controller: PlayerId,
) -> bool {
    filter_inner(state, object_id, filter, source_id, Some(controller))
}

fn filter_inner(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false,     // Players are not objects
        TargetFilter::Controller => false, // Controller is a player, not an object
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::Typed(TypedFilter {
            card_type,
            subtype,
            controller,
            properties,
        }) => {
            let obj = match state.objects.get(&object_id) {
                Some(o) => o,
                None => return false,
            };
            // Type check
            if let Some(tf) = card_type {
                if !matches_type_filter(tf, obj) {
                    return false;
                }
            }
            // Controller check
            if let Some(ctrl) = controller {
                match ctrl {
                    ControllerRef::You => {
                        if source_controller != Some(obj.controller) {
                            return false;
                        }
                    }
                    ControllerRef::Opponent => {
                        if source_controller == Some(obj.controller) {
                            return false;
                        }
                    }
                }
            }
            // Subtype check
            if let Some(st) = subtype {
                if !obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(st))
                {
                    return false;
                }
            }
            // All properties must match
            let source_obj = state.objects.get(&source_id);
            let source_attached_to = source_obj.and_then(|s| s.attached_to);
            let source_chosen_creature_type =
                source_obj.and_then(|s| s.chosen_creature_type().map(|t| t.to_string()));
            properties.iter().all(|p| {
                matches_filter_prop(
                    p,
                    obj,
                    object_id,
                    source_id,
                    source_controller,
                    source_attached_to,
                    source_chosen_creature_type.as_deref(),
                )
            })
        }
        TargetFilter::Not { filter: inner } => {
            !filter_inner(state, object_id, inner, source_id, source_controller)
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| filter_inner(state, object_id, f, source_id, source_controller)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|f| filter_inner(state, object_id, f, source_id, source_controller)),
        // StackAbility targeting is handled directly in find_legal_targets, not via filter
        TargetFilter::StackAbility => false,
    }
}

/// Check if an object matches a TypeFilter variant.
fn matches_type_filter(tf: &TypeFilter, obj: &GameObject) -> bool {
    match tf {
        TypeFilter::Creature => obj.card_types.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => obj.card_types.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => obj.card_types.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => obj.card_types.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Instant => obj.card_types.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => obj.card_types.core_types.contains(&CoreType::Sorcery),
        TypeFilter::Planeswalker => obj.card_types.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Permanent => {
            obj.card_types.core_types.contains(&CoreType::Creature)
                || obj.card_types.core_types.contains(&CoreType::Artifact)
                || obj.card_types.core_types.contains(&CoreType::Enchantment)
                || obj.card_types.core_types.contains(&CoreType::Land)
                || obj.card_types.core_types.contains(&CoreType::Planeswalker)
        }
        TypeFilter::Card | TypeFilter::Any => true,
    }
}

/// Check if an object satisfies a single FilterProp.
fn matches_filter_prop(
    prop: &FilterProp,
    obj: &GameObject,
    object_id: ObjectId,
    _source_id: ObjectId,
    source_controller: Option<PlayerId>,
    source_attached_to: Option<ObjectId>,
    source_chosen_creature_type: Option<&str>,
) -> bool {
    match prop {
        FilterProp::Token => {
            // A token has no card_id (card_id.0 == 0) in typical token creation
            // For now, permissive true -- tokens will be marked more explicitly later
            true
        }
        FilterProp::Attacking => {
            // Would check combat state -- permissive for now
            true
        }
        FilterProp::Tapped => obj.tapped,
        FilterProp::NonType { value } => {
            // Object does not have this type or subtype
            let core: Option<CoreType> = value.parse().ok();
            match core {
                Some(ct) => !obj.card_types.core_types.contains(&ct),
                None => {
                    // Not a core type — check subtypes (e.g., "non-Human")
                    !obj.card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(value))
                }
            }
        }
        FilterProp::WithKeyword { value } => {
            // Check if object has the keyword
            let kw: Result<crate::types::keywords::Keyword, _> = value.parse();
            match kw {
                Ok(k) => obj.has_keyword(&k),
                Err(_) => true, // Unknown keyword -- permissive
            }
        }
        FilterProp::CountersGE {
            counter_type,
            count,
        } => {
            let ct = parse_counter_type(counter_type);
            obj.counters.get(&ct).copied().unwrap_or(0) >= *count
        }
        FilterProp::CmcGE { value } => {
            let cmc = match &obj.mana_cost {
                crate::types::mana::ManaCost::NoCost => 0u32,
                crate::types::mana::ManaCost::Cost { shards, generic } => {
                    *generic + shards.len() as u32
                }
            };
            cmc >= *value
        }
        FilterProp::CmcLE { value } => {
            let cmc = match &obj.mana_cost {
                crate::types::mana::ManaCost::NoCost => 0u32,
                crate::types::mana::ManaCost::Cost { shards, generic } => {
                    *generic + shards.len() as u32
                }
            };
            cmc <= *value
        }
        FilterProp::InZone { zone } => obj.zone == *zone,
        FilterProp::Owned { controller } => match controller {
            ControllerRef::You => source_controller == Some(obj.owner),
            ControllerRef::Opponent => {
                source_controller.is_some() && source_controller != Some(obj.owner)
            }
        },
        FilterProp::EnchantedBy => source_attached_to == Some(object_id),
        FilterProp::EquippedBy => source_attached_to == Some(object_id),
        FilterProp::Another => object_id != _source_id,
        FilterProp::HasColor { color } => {
            use crate::types::mana::ManaColor;
            let mana_color = match color.as_str() {
                "White" => Some(ManaColor::White),
                "Blue" => Some(ManaColor::Blue),
                "Black" => Some(ManaColor::Black),
                "Red" => Some(ManaColor::Red),
                "Green" => Some(ManaColor::Green),
                _ => None,
            };
            match mana_color {
                Some(mc) => obj.color.contains(&mc),
                None => true, // Unknown color — permissive
            }
        }
        FilterProp::PowerLE { value } => obj.power.unwrap_or(0) <= *value,
        FilterProp::PowerGE { value } => obj.power.unwrap_or(0) >= *value,
        FilterProp::Multicolored => obj.color.len() > 1,
        FilterProp::HasSupertype { value } => {
            let st: Result<crate::types::card_type::Supertype, _> = value.parse();
            match st {
                Ok(supertype) => obj.card_types.supertypes.contains(&supertype),
                Err(_) => true, // Unknown supertype — permissive
            }
        }
        FilterProp::IsChosenCreatureType => match source_chosen_creature_type {
            Some(chosen) => obj
                .card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case(chosen)),
            None => false,
        },
        FilterProp::Other { .. } => true, // Permissive fallback for unrecognized properties
    }
}

fn parse_counter_type(s: &str) -> crate::game::game_object::CounterType {
    match s {
        "+1/+1" => crate::game::game_object::CounterType::Plus1Plus1,
        "-1/-1" => crate::game::game_object::CounterType::Minus1Minus1,
        "loyalty" => crate::game::game_object::CounterType::Loyalty,
        other => crate::game::game_object::CounterType::Generic(other.to_string()),
    }
}

/// Check if a player matches a typed player filter.
///
/// Used by static abilities that target players rather than objects.
pub fn player_matches_filter(
    player_id: PlayerId,
    filter: &str,
    source_controller: Option<PlayerId>,
) -> bool {
    for part in filter.split('+') {
        match part {
            "You" => {
                if source_controller != Some(player_id) {
                    return false;
                }
            }
            "Opp" => {
                if source_controller == Some(player_id) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    #[test]
    fn none_filter_matches_nothing() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(!matches_target_filter(&state, id, &TargetFilter::None, id));
    }

    #[test]
    fn any_filter_matches_everything() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(matches_target_filter(&state, id, &TargetFilter::Any, id));
    }

    #[test]
    fn type_filter_matches_correct_type() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let creature_filter = TargetFilter::Typed(TypedFilter::creature());
        let land_filter = TargetFilter::Typed(TypedFilter::land());
        let card_filter = TargetFilter::Typed(TypedFilter::card());
        assert!(matches_target_filter(&state, id, &creature_filter, id));
        assert!(!matches_target_filter(&state, id, &land_filter, id));
        assert!(matches_target_filter(&state, id, &card_filter, id));
    }

    #[test]
    fn self_filter() {
        let mut state = setup();
        let a = add_creature(&mut state, PlayerId(0), "A");
        let b = add_creature(&mut state, PlayerId(0), "B");
        assert!(matches_target_filter(&state, a, &TargetFilter::SelfRef, a));
        assert!(!matches_target_filter(&state, b, &TargetFilter::SelfRef, a));
    }

    #[test]
    fn other_filter_excludes_source() {
        let mut state = setup();
        let marshal = add_creature(&mut state, PlayerId(0), "Benalish Marshal");
        let bear = add_creature(&mut state, PlayerId(0), "Bear");

        // "Creature.Other+YouCtrl" = And(Typed{creature, You}, Not(SelfRef))
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::SelfRef),
                },
            ],
        };

        // Marshal should NOT match its own "Other" filter
        assert!(!matches_target_filter(&state, marshal, &filter, marshal));
        // Bear should match
        assert!(matches_target_filter(&state, bear, &filter, marshal));
    }

    #[test]
    fn you_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));

        assert!(matches_target_filter(&state, mine, &filter, mine));
        assert!(!matches_target_filter(&state, theirs, &filter, mine));
    }

    #[test]
    fn opp_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));

        assert!(!matches_target_filter(&state, mine, &filter, mine));
        assert!(matches_target_filter(&state, theirs, &filter, mine));
    }

    #[test]
    fn combined_type_and_controller() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Lord");
        let ally = add_creature(&mut state, PlayerId(0), "Ally");
        let enemy = add_creature(&mut state, PlayerId(1), "Enemy");

        // "Creature.Other+YouCtrl"
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::SelfRef),
                },
            ],
        };

        assert!(!matches_target_filter(&state, source, &filter, source));
        assert!(matches_target_filter(&state, ally, &filter, source));
        assert!(!matches_target_filter(&state, enemy, &filter, source));
    }

    #[test]
    fn permanent_matches_multiple_types() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let filter = TargetFilter::Typed(TypedFilter::permanent());
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn enchanted_by_only_matches_attached_creature() {
        let mut state = setup();
        let creature_a = add_creature(&mut state, PlayerId(0), "Bear A");
        let creature_b = add_creature(&mut state, PlayerId(0), "Bear B");

        // Create an aura (source) attached to creature_a
        let next_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(0),
            "Rancor".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        state.objects.get_mut(&aura).unwrap().attached_to = Some(creature_a);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));

        assert!(matches_target_filter(&state, creature_a, &filter, aura));
        assert!(
            !matches_target_filter(&state, creature_b, &filter, aura),
            "EnchantedBy must not match creatures the aura is NOT attached to"
        );
    }

    #[test]
    fn enchanted_by_no_attachment_matches_nothing() {
        let mut state = setup();
        let creature = add_creature(&mut state, PlayerId(0), "Bear");

        // Aura not attached to anything
        let next_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(0),
            "Floating Aura".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));

        assert!(
            !matches_target_filter(&state, creature, &filter, aura),
            "Unattached aura should not match any creature"
        );
    }

    #[test]
    fn player_filter_you() {
        assert!(player_matches_filter(PlayerId(0), "You", Some(PlayerId(0))));
        assert!(!player_matches_filter(
            PlayerId(1),
            "You",
            Some(PlayerId(0))
        ));
    }

    #[test]
    fn player_filter_opp() {
        assert!(!player_matches_filter(
            PlayerId(0),
            "Opp",
            Some(PlayerId(0))
        ));
        assert!(player_matches_filter(PlayerId(1), "Opp", Some(PlayerId(0))));
    }

    #[test]
    fn not_filter_inverts() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let not_self = TargetFilter::Not {
            filter: Box::new(TargetFilter::SelfRef),
        };
        assert!(!matches_target_filter(&state, id, &not_self, id));
    }

    #[test]
    fn or_filter_any_match() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::land()),
                TargetFilter::Typed(TypedFilter::creature()),
            ],
        };
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn tapped_property() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        state.objects.get_mut(&id).unwrap().tapped = true;

        let filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Tapped]));
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn has_supertype_basic_matches_basic_land() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Plains");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Basic);
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];

        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                }]),
            );
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn has_supertype_basic_rejects_nonbasic_land() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Stomping Ground");
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];

        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                }]),
            );
        assert!(!matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn controlled_variant_uses_explicit_controller() {
        let mut state = setup();
        let obj = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));

        // Source doesn't exist, but we pass controller explicitly
        let fake_source = ObjectId(9999);
        assert!(matches_target_filter_controlled(
            &state,
            obj,
            &filter,
            fake_source,
            PlayerId(0)
        ));
    }

    #[test]
    fn chosen_creature_type_matches_subtype() {
        use crate::types::ability::ChosenAttribute;

        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Mimic");
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));

        let elf = add_creature(&mut state, PlayerId(0), "Elf Warrior");
        state
            .objects
            .get_mut(&elf)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());

        let goblin = add_creature(&mut state, PlayerId(0), "Goblin");
        state
            .objects
            .get_mut(&goblin)
            .unwrap()
            .card_types
            .subtypes
            .push("Goblin".to_string());

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::IsChosenCreatureType]),
        );

        assert!(
            matches_target_filter(&state, elf, &filter, source),
            "Elf should match chosen creature type Elf"
        );
        assert!(
            !matches_target_filter(&state, goblin, &filter, source),
            "Goblin should not match chosen creature type Elf"
        );
    }
}
