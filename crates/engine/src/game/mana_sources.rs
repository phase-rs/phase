use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, TargetFilter,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaType};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::combat;
use super::mana_abilities;
use super::mana_payment;
use super::restrictions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManaSourceOption {
    pub object_id: ObjectId,
    pub ability_index: Option<usize>,
    pub mana_type: ManaType,
    /// True for Treasure-style costs (`Composite { Tap, Sacrifice }`).
    /// Used by auto-tap to deprioritize sacrifice sources as last resort.
    pub requires_sacrifice: bool,
    /// True for costs like `{T}, Pay 1 life`.
    /// Used by auto-tap to prefer equivalent sources that do not spend life.
    pub requires_life_payment: bool,
}

/// Check whether an ability cost includes a tap component.
/// Matches both `AbilityCost::Tap` and `Composite` costs containing `Tap`.
pub(crate) fn has_tap_component(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| matches!(c, AbilityCost::Tap))
        }
        _ => false,
    }
}

/// Check whether an ability cost requires sacrificing the source permanent.
fn cost_requires_sacrifice(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Composite { costs }) => costs.iter().any(|c| {
            matches!(
                c,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        }),
        _ => false,
    }
}

fn cost_requires_life_payment(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::PayLife { .. }) => true,
        Some(AbilityCost::Composite { costs }) => costs
            .iter()
            .any(|c| matches!(c, AbilityCost::PayLife { .. })),
        _ => false,
    }
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

/// CR 605.1b: Return activatable tap-mana options for ANY untapped permanent.
/// CR 302.6: Creatures with summoning sickness cannot activate tap abilities.
///
/// Used by auto-pay affordability checks and AI candidate generation to include
/// non-land mana sources (mana dorks, Treasure tokens, mana artifacts).
pub fn activatable_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaSourceOption> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller || obj.tapped {
        return Vec::new();
    }
    // CR 602.5a + CR 302.6: Creatures with summoning sickness cannot activate tap abilities.
    if combat::has_summoning_sickness(obj, state.turn_number) {
        return Vec::new();
    }
    scan_mana_abilities(state, obj, object_id, controller, true)
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
    if obj.zone != Zone::Battlefield || obj.controller != controller {
        return Vec::new();
    }
    if !obj.card_types.core_types.contains(&CoreType::Land) {
        return Vec::new();
    }
    if require_untapped && obj.tapped {
        return Vec::new();
    }
    // CR 602.5a + CR 302.6: Land-creatures (e.g., Dryad Arbor) have summoning sickness and
    // cannot activate tap abilities the turn they enter the battlefield.
    if require_untapped && combat::has_summoning_sickness(obj, state.turn_number) {
        return Vec::new();
    }

    let mut options = scan_mana_abilities(state, obj, object_id, controller, require_untapped);

    // Legacy fallback for basic-land subtype-only objects (no explicit mana ability).
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
                requires_sacrifice: false,
                requires_life_payment: false,
            });
        }
    }

    options
}

/// Scan an object's abilities for activated mana abilities with a tap cost component.
/// Type-agnostic — works for lands, creatures, artifacts, etc.
fn scan_mana_abilities(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    object_id: ObjectId,
    controller: PlayerId,
    require_current_payability: bool,
) -> Vec<ManaSourceOption> {
    let mut options = Vec::new();
    for (ability_index, ability) in obj.abilities.iter().enumerate() {
        if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
            continue;
        }
        if require_current_payability
            && !mana_abilities::can_activate_mana_ability_now(state, controller, object_id, ability)
        {
            continue;
        }
        if !has_tap_component(&ability.cost) {
            continue;
        }
        if !activation_condition_satisfied(state, controller, object_id, ability_index, ability) {
            continue;
        }

        let sacrifice = cost_requires_sacrifice(&ability.cost);
        let life_payment = cost_requires_life_payment(&ability.cost);
        for mana_type in mana_options_from_ability(state, controller, ability) {
            let option = ManaSourceOption {
                object_id,
                ability_index: Some(ability_index),
                mana_type,
                requires_sacrifice: sacrifice,
                requires_life_payment: life_payment,
            };
            if !options.contains(&option) {
                options.push(option);
            }
        }
    }
    options
}

/// CR 605.3b — Mana abilities must still satisfy activation conditions.
/// Delegates to the shared restriction checker so that `RequiresCondition`,
/// once-per-turn limits, sorcery-speed, and all other restriction types
/// are enforced uniformly for mana source analysis.
pub(crate) fn activation_condition_satisfied(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    ability_index: usize,
    ability: &AbilityDefinition,
) -> bool {
    restrictions::check_activation_restrictions(
        state,
        controller,
        object_id,
        ability_index,
        &ability.activation_restrictions,
    )
    .is_ok()
}

fn mana_options_from_ability(
    state: &GameState,
    controller: PlayerId,
    ability: &AbilityDefinition,
) -> Vec<ManaType> {
    let Effect::Mana { produced, .. } = &*ability.effect else {
        return Vec::new();
    };
    mana_options_from_production(state, controller, produced)
}

fn mana_options_from_production(
    state: &GameState,
    controller: PlayerId,
    produced: &ManaProduction,
) -> Vec<ManaType> {
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
        | ManaProduction::AnyCombination { color_options, .. } => {
            let mut options = Vec::new();
            for color in color_options {
                let mana_type = mana_color_to_type(color);
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
            options
        }
        // TODO: resolve from object's chosen_attributes when mana source analysis
        // gets access to the source object's state
        ManaProduction::ChosenColor { .. } => Vec::new(),
        // CR 106.7: Compute colors dynamically from opponent-controlled lands.
        ManaProduction::OpponentLandColors { .. } => opponent_land_color_options(state, controller),
    }
}

/// CR 106.7: Compute the mana colors that lands controlled by opponents could produce.
///
/// Iterates over all opponent-controlled lands on the battlefield and collects the
/// union of mana colors their non-`OpponentLandColors` mana abilities could produce.
/// `OpponentLandColors` abilities are excluded to prevent infinite recursion when
/// an opponent also controls a card like Exotic Orchard.
pub(crate) fn opponent_land_color_options(
    state: &GameState,
    controller: PlayerId,
) -> Vec<ManaType> {
    let opponents = super::players::opponents(state, controller);
    let mut options = Vec::new();
    for obj in state.objects.values() {
        if obj.zone != Zone::Battlefield {
            continue;
        }
        if !opponents.contains(&obj.controller) {
            continue;
        }
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        // Scan each mana ability, skipping OpponentLandColors to prevent recursion.
        for ability in &obj.abilities {
            if ability.kind != AbilityKind::Activated
                || !super::mana_abilities::is_mana_ability(ability)
            {
                continue;
            }
            if !has_tap_component(&ability.cost) {
                continue;
            }
            let Effect::Mana { produced, .. } = &*ability.effect else {
                continue;
            };
            // CR 106.7: Skip OpponentLandColors — an Exotic Orchard facing another
            // Exotic Orchard with no other lands produces no mana.
            if matches!(produced, ManaProduction::OpponentLandColors { .. }) {
                continue;
            }
            for mana_type in mana_options_from_production(state, controller, produced) {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
        // Fallback: basic-land subtype-only objects (no explicit mana ability).
        if options.is_empty() {
            if let Some(mana_type) = obj
                .card_types
                .subtypes
                .iter()
                .find_map(|s| super::mana_payment::land_subtype_to_mana_type(s))
            {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
    }
    options
}

pub fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

pub fn mana_type_to_color(mana_type: ManaType) -> Option<ManaColor> {
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
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use crate::types::identifiers::CardId;

    fn verge_ability(color: ManaColor) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    fn add_verge_land(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        unconditional_color: ManaColor,
        conditional_color: ManaColor,
        condition_text: &str,
    ) -> ObjectId {
        use crate::types::ability::ActivationRestriction;

        let verge = create_object(
            state,
            CardId(100),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&verge).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(verge_ability(unconditional_color));
        obj.abilities.push(
            verge_ability(conditional_color).activation_restrictions(vec![
                ActivationRestriction::RequiresCondition {
                    condition: crate::parser::oracle_condition::parse_restriction_condition(
                        condition_text,
                    ),
                },
            ]),
        );
        verge
    }

    #[test]
    fn conditional_mana_blocked_without_supporting_land() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Blue));
        assert!(!types.contains(&ManaType::Black));
    }

    #[test]
    fn conditional_mana_allowed_with_supporting_land() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );
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
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Blue));
        assert!(types.contains(&ManaType::Black));
    }

    #[test]
    fn display_colors_ignore_tapped_state() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );
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
    fn riverpyre_verge_blocks_blue_without_island_or_mountain() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Riverpyre Verge",
            ManaColor::Red,
            ManaColor::Blue,
            "you control an Island or a Mountain",
        );

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(
            types.contains(&ManaType::Red),
            "unconditional red should be available"
        );
        assert!(
            !types.contains(&ManaType::Blue),
            "blue should NOT be available without Island/Mountain"
        );
    }

    #[test]
    fn riverpyre_verge_allows_blue_with_mountain() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Riverpyre Verge",
            ManaColor::Red,
            ManaColor::Blue,
            "you control an Island or a Mountain",
        );
        let mountain = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&mountain).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Red));
        assert!(
            types.contains(&ManaType::Blue),
            "blue should be available with Mountain in play"
        );
    }

    // ── activatable_mana_options tests ────────────────────────────────

    #[test]
    fn creature_mana_dork_returns_mana_options() {
        let mut state = GameState::new_two_player(42);
        let elf = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&elf).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.abilities.push(verge_ability(ManaColor::Green));
        // No summoning sickness: entered on a previous turn
        obj.entered_battlefield_turn = Some(0);
        state.turn_number = 2;

        let options = activatable_mana_options(&state, elf, PlayerId(0));
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].mana_type, ManaType::Green);
        assert!(!options[0].requires_sacrifice);
        assert!(!options[0].requires_life_payment);
    }

    #[test]
    fn creature_with_summoning_sickness_returns_empty() {
        // CR 302.6: Creature that just entered can't activate tap abilities.
        let mut state = GameState::new_two_player(42);
        let elf = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&elf).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.abilities.push(verge_ability(ManaColor::Green));
        obj.entered_battlefield_turn = Some(1);
        state.turn_number = 1; // Same turn — summoning sickness

        let options = activatable_mana_options(&state, elf, PlayerId(0));
        assert!(
            options.is_empty(),
            "should be empty due to summoning sickness"
        );
    }

    #[test]
    fn treasure_token_returns_sacrifice_option() {
        // CR 111.10a: Treasure — "{T}, Sacrifice this artifact: Add one mana of any color."
        let mut state = GameState::new_two_player(42);
        let treasure = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&treasure).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Treasure".to_string());

        use crate::types::ability::{ManaProduction, QuantityExpr, TargetFilter};
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });
        let obj = state.objects.get_mut(&treasure).unwrap();
        obj.abilities.push(ability);

        let options = activatable_mana_options(&state, treasure, PlayerId(0));
        assert!(!options.is_empty(), "Treasure should have mana options");
        assert!(
            options.iter().all(|o| o.requires_sacrifice),
            "all Treasure options should require sacrifice"
        );
        assert!(
            options.iter().all(|o| !o.requires_life_payment),
            "Treasure options should not require life payment"
        );
        // Should have 5 color options
        assert_eq!(options.len(), 5);
    }

    #[test]
    fn life_payment_mana_source_marks_life_cost() {
        let mut state = GameState::new_two_player(42);
        let town = create_object(
            &mut state,
            CardId(303),
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&town).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, AbilityCost::PayLife { amount: 1 }],
            }),
        );

        let options = activatable_land_mana_options(&state, town, PlayerId(0));
        assert!(
            !options.is_empty(),
            "Starting Town should expose mana options"
        );
        assert!(
            options.iter().all(|o| o.requires_life_payment),
            "all colored Starting Town options should be marked as life-payment options"
        );
        assert!(
            options.iter().all(|o| !o.requires_sacrifice),
            "Starting Town should not be treated as a sacrifice source"
        );
    }
}
