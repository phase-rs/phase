use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction};
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaType};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::mana_abilities;
use super::mana_payment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManaSourceOption {
    pub object_id: ObjectId,
    pub ability_index: Option<usize>,
    pub mana_type: ManaType,
}

/// Return all currently activatable tap-mana options for a land.
///
/// This is used by legal action generation and auto-pay. It evaluates supported
/// activation restrictions (currently land-subtype control clauses) and returns
/// one or more candidate colors for the source.
pub fn activatable_land_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaSourceOption> {
    land_mana_options(state, object_id, controller, true)
}

/// Return display colors for a land based on mana abilities that are currently
/// available under game-state conditions.
///
/// Unlike `activatable_land_mana_options`, this ignores tapped state so frame
/// colors remain stable while permanents are tapped.
pub fn display_land_mana_colors(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaColor> {
    let mut colors = Vec::new();
    for option in land_mana_options(state, object_id, controller, false) {
        if let Some(color) = mana_type_to_color(option.mana_type) {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }
    }
    colors
}

fn land_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    require_untapped: bool,
) -> Vec<ManaSourceOption> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    if obj.zone != Zone::Battlefield {
        return Vec::new();
    }
    if obj.controller != controller {
        return Vec::new();
    }
    if !obj.card_types.core_types.contains(&CoreType::Land) {
        return Vec::new();
    }
    if require_untapped && obj.tapped {
        return Vec::new();
    }

    let mut options = Vec::new();
    for (ability_index, ability) in obj.abilities.iter().enumerate() {
        if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
            continue;
        }
        if !matches!(ability.cost, Some(AbilityCost::Tap)) {
            continue;
        }
        if !activation_condition_satisfied(state, controller, ability) {
            continue;
        }

        for mana_type in mana_options_from_ability(ability) {
            let option = ManaSourceOption {
                object_id,
                ability_index: Some(ability_index),
                mana_type,
            };
            if !options.contains(&option) {
                options.push(option);
            }
        }
    }

    // Legacy fallback for basic-land subtype-only objects.
    if options.is_empty() {
        if let Some(mana_type) = obj
            .card_types
            .subtypes
            .iter()
            .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
        {
            options.push(ManaSourceOption {
                object_id,
                ability_index: None,
                mana_type,
            });
        }
    }

    options
}

fn activation_condition_satisfied(
    state: &GameState,
    controller: PlayerId,
    ability: &AbilityDefinition,
) -> bool {
    let Some(required_subtypes) = extract_land_subtype_activation_condition(ability) else {
        return true;
    };

    state.battlefield.iter().any(|oid| {
        let Some(obj) = state.objects.get(oid) else {
            return false;
        };
        obj.controller == controller
            && obj.card_types.core_types.contains(&CoreType::Land)
            && obj
                .card_types
                .subtypes
                .iter()
                .any(|subtype| required_subtypes.iter().any(|s| s == subtype))
    })
}

fn extract_land_subtype_activation_condition(ability: &AbilityDefinition) -> Option<Vec<String>> {
    let mut cursor = ability.sub_ability.as_deref();
    while let Some(def) = cursor {
        if let Some(subtypes) = parse_land_subtype_condition_from_effect(&def.effect) {
            return Some(subtypes);
        }
        cursor = def.sub_ability.as_deref();
    }
    None
}

fn parse_land_subtype_condition_from_effect(effect: &Effect) -> Option<Vec<String>> {
    let Effect::Unimplemented { name, description } = effect else {
        return None;
    };
    let description = description.as_deref()?;

    if name == "activate_only_if_controls_land_subtype_any" {
        let mut subtypes = Vec::new();
        for raw in description
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Some(subtype) = canonical_basic_land_subtype(raw) {
                if !subtypes.contains(&subtype.to_string()) {
                    subtypes.push(subtype.to_string());
                }
            } else {
                return None;
            }
        }
        return (!subtypes.is_empty()).then_some(subtypes);
    }

    if name == "activate" {
        return parse_basic_land_subtype_activation_text(description);
    }

    None
}

fn parse_basic_land_subtype_activation_text(text: &str) -> Option<Vec<String>> {
    let lower = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let prefix = "activate only if you control ";
    if !lower.starts_with(prefix) {
        return None;
    }

    let rest = &lower[prefix.len()..];
    let mut subtypes = Vec::new();
    for raw_part in rest.split(" or ") {
        let part = raw_part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .trim();
        let subtype = canonical_basic_land_subtype(part)?;
        let subtype = subtype.to_string();
        if !subtypes.contains(&subtype) {
            subtypes.push(subtype);
        }
    }

    (!subtypes.is_empty()).then_some(subtypes)
}

fn canonical_basic_land_subtype(raw: &str) -> Option<&'static str> {
    match raw {
        "plains" | "Plains" => Some("Plains"),
        "island" | "Island" => Some("Island"),
        "swamp" | "Swamp" => Some("Swamp"),
        "mountain" | "Mountain" => Some("Mountain"),
        "forest" | "Forest" => Some("Forest"),
        _ => None,
    }
}

fn mana_options_from_ability(ability: &AbilityDefinition) -> Vec<ManaType> {
    let Effect::Mana { produced, .. } = &ability.effect else {
        return Vec::new();
    };
    mana_options_from_production(produced)
}

fn mana_options_from_production(produced: &ManaProduction) -> Vec<ManaType> {
    match produced {
        ManaProduction::Fixed { colors } => {
            let mut options = Vec::new();
            for color in colors {
                let mana_type = mana_color_to_type(color);
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
            options
        }
        ManaProduction::Colorless { .. } => vec![ManaType::Colorless],
        ManaProduction::AnyOneColor { color_options, .. }
        | ManaProduction::AnyCombination { color_options, .. } => color_options
            .first()
            .map(mana_color_to_type)
            .into_iter()
            .collect(),
        // TODO: resolve from object's chosen_attributes when mana source analysis
        // gets access to the source object's state
        ManaProduction::ChosenColor { .. } => Vec::new(),
    }
}

fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

fn mana_type_to_color(mana_type: ManaType) -> Option<ManaColor> {
    match mana_type {
        ManaType::White => Some(ManaColor::White),
        ManaType::Blue => Some(ManaColor::Blue),
        ManaType::Black => Some(ManaColor::Black),
        ManaType::Red => Some(ManaColor::Red),
        ManaType::Green => Some(ManaColor::Green),
        ManaType::Colorless => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind};
    use crate::types::identifiers::CardId;

    fn verge_ability(color: ManaColor) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                },
                restrictions: vec![],
            },
        )
        .cost(AbilityCost::Tap)
    }

    fn add_gloomlake_verge(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let verge = create_object(
            state,
            CardId(100),
            controller,
            "Gloomlake Verge".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&verge).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(verge_ability(ManaColor::Blue));
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Black],
                    },
                    restrictions: vec![],
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Unimplemented {
                    name: "activate".to_string(),
                    description: Some(
                        "Activate only if you control an Island or a Swamp".to_string(),
                    ),
                },
            )),
        );
        verge
    }

    #[test]
    fn conditional_land_options_include_only_unconditional_color_without_support() {
        let mut state = GameState::new_two_player(42);
        let verge = add_gloomlake_verge(&mut state, PlayerId(0));

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let colors: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(colors.contains(&ManaType::Blue));
        assert!(!colors.contains(&ManaType::Black));
    }

    #[test]
    fn conditional_land_options_include_secondary_color_with_supporting_subtype() {
        let mut state = GameState::new_two_player(42);
        let verge = add_gloomlake_verge(&mut state, PlayerId(0));
        let island = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let island_obj = state.objects.get_mut(&island).unwrap();
        island_obj.card_types.core_types.push(CoreType::Land);
        island_obj.card_types.subtypes.push("Island".to_string());

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let colors: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(colors.contains(&ManaType::Blue));
        assert!(colors.contains(&ManaType::Black));
    }

    #[test]
    fn display_colors_ignore_tapped_state() {
        let mut state = GameState::new_two_player(42);
        let verge = add_gloomlake_verge(&mut state, PlayerId(0));
        let swamp = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        let swamp_obj = state.objects.get_mut(&swamp).unwrap();
        swamp_obj.card_types.core_types.push(CoreType::Land);
        swamp_obj.card_types.subtypes.push("Swamp".to_string());
        state.objects.get_mut(&verge).unwrap().tapped = true;

        let colors = display_land_mana_colors(&state, verge, PlayerId(0));
        assert!(colors.contains(&ManaColor::Blue));
        assert!(colors.contains(&ManaColor::Black));
    }

    #[test]
    fn parse_canonical_condition_payload() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Blue],
                },
                restrictions: vec![],
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Unimplemented {
                name: "activate_only_if_controls_land_subtype_any".to_string(),
                description: Some("Island|Swamp".to_string()),
            },
        ));

        let parsed = extract_land_subtype_activation_condition(&def).unwrap();
        assert_eq!(parsed, vec!["Island".to_string(), "Swamp".to_string()]);
    }
}
