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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManaSourceOption {
    pub object_id: ObjectId,
    pub ability_index: Option<usize>,
    /// Indexing/representative color for this row.
    /// For normal sources this is the exact mana type produced.
    /// For filter-land combination rows (`atomic_combination.is_some()`),
    /// this mirrors the first color of the combination and is used only for
    /// shard-matching lookup; the full mana production is driven by
    /// `atomic_combination`.
    pub mana_type: ManaType,
    /// True for Treasure-style costs (`Composite { Tap, Sacrifice }`).
    /// Used by auto-tap to deprioritize sacrifice sources as last resort.
    pub requires_sacrifice: bool,
    /// True for costs like `{T}, Pay 1 life`.
    /// Used by auto-tap to prefer equivalent sources that do not spend life.
    pub requires_life_payment: bool,
    /// True when resolving this mana ability can damage its controller or
    /// make them lose life (e.g., pain lands, pay-life mana abilities).
    /// Used by auto-pay to prefer equivalent sources that don't harm the user.
    pub harms_controller: bool,
    /// True iff manual `UntapLandForMana` can fully reverse this activation:
    /// tap-only cost and no non-mana side effects in the resolution graph.
    pub undoable: bool,
    /// CR 605.3b + CR 106.1a: Complete pre-chosen multi-mana sequence for
    /// `ManaProduction::ChoiceAmongCombinations` sources (Shadowmoor/Eventide
    /// filter lands). `None` for all other sources. When `Some`, a single
    /// activation of this ability produces **every** mana type listed here,
    /// atomically — the shard assigner must treat all combos sharing the same
    /// `(object_id, ability_index)` as alternatives (pick at most one).
    pub atomic_combination: Option<Vec<ManaType>>,
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

fn cost_is_reversible_tap_only(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            !costs.is_empty() && costs.iter().all(|c| matches!(c, AbilityCost::Tap))
        }
        _ => false,
    }
}

fn effect_harms_controller(effect: &Effect) -> bool {
    match effect {
        Effect::DealDamage { target, .. } => matches!(target, TargetFilter::Controller),
        Effect::LoseLife { target, .. } => {
            target.is_none() || matches!(target.as_ref(), Some(TargetFilter::Controller))
        }
        _ => false,
    }
}

fn chain_harms_controller(ability: &AbilityDefinition) -> bool {
    effect_harms_controller(&ability.effect)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(chain_harms_controller)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(chain_harms_controller)
}

fn chain_is_all_mana(ability: &AbilityDefinition) -> bool {
    matches!(*ability.effect, Effect::Mana { .. })
        && ability.sub_ability.as_deref().is_none_or(chain_is_all_mana)
        && ability
            .else_ability
            .as_deref()
            .is_none_or(chain_is_all_mana)
}

pub(crate) fn mana_ability_harms_controller(ability: &AbilityDefinition) -> bool {
    cost_requires_life_payment(&ability.cost) || chain_harms_controller(ability)
}

pub(crate) fn mana_ability_is_undoable(ability: &AbilityDefinition) -> bool {
    cost_is_reversible_tap_only(&ability.cost) && chain_is_all_mana(ability)
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
                harms_controller: false,
                undoable: true,
                atomic_combination: None,
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
            && !mana_abilities::can_activate_mana_ability_now(
                state,
                controller,
                object_id,
                ability_index,
                ability,
            )
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
        let harms_controller = mana_ability_harms_controller(ability);
        let undoable = mana_ability_is_undoable(ability);
        for row in emit_source_rows(state, controller, object_id, ability_index, ability) {
            let option = ManaSourceOption {
                object_id,
                ability_index: Some(ability_index),
                mana_type: row.mana_type,
                requires_sacrifice: sacrifice,
                requires_life_payment: life_payment,
                harms_controller,
                undoable,
                atomic_combination: row.atomic_combination,
            };
            if !options.contains(&option) {
                options.push(option);
            }
        }
    }
    options
}

/// Per-ability source row: either a plain per-color candidate or a full
/// multi-mana combination. Used by `scan_mana_abilities` to build
/// `ManaSourceOption` rows uniformly across `ManaProduction` variants.
struct SourceRow {
    mana_type: ManaType,
    atomic_combination: Option<Vec<ManaType>>,
}

fn emit_source_rows(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    _ability_index: usize,
    ability: &AbilityDefinition,
) -> Vec<SourceRow> {
    let Effect::Mana { produced, .. } = &*ability.effect else {
        return Vec::new();
    };
    match produced {
        // CR 605.3b + CR 106.1a: Filter-land combinations. Emit one row per
        // combination so the auto-tap shard assigner can pick whichever
        // combination satisfies the pending cost.
        ManaProduction::ChoiceAmongCombinations { options } => options
            .iter()
            .filter_map(|combo| {
                let types: Vec<ManaType> = combo.iter().map(mana_color_to_type).collect();
                types.first().copied().map(|first| SourceRow {
                    mana_type: first,
                    atomic_combination: Some(types),
                })
            })
            .collect(),
        _ => mana_options_from_production(state, controller, object_id, produced)
            .into_iter()
            .map(|mana_type| SourceRow {
                mana_type,
                atomic_combination: None,
            })
            .collect(),
    }
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

fn mana_options_from_production(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    produced: &ManaProduction,
) -> Vec<ManaType> {
    match produced {
        ManaProduction::Fixed { colors, .. } => {
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
        // CR 106.1a: Resolve the source permanent's chosen color. If the
        // permanent has no chosen color yet (e.g., still in library / sideboard
        // preview), return empty so it isn't offered as a mana source.
        ManaProduction::ChosenColor { .. } => state
            .objects
            .get(&object_id)
            .and_then(|obj| obj.chosen_color())
            .map(|color| vec![mana_color_to_type(&color)])
            .unwrap_or_default(),
        // CR 106.7: Compute colors dynamically from opponent-controlled lands.
        ManaProduction::OpponentLandColors { .. } => opponent_land_color_options(state, controller),
        // CR 605.1a + CR 406.1 + CR 610.3: Compute colors dynamically from cards
        // exiled-with this source via `state.exile_links` (Pit of Offerings).
        ManaProduction::ChoiceAmongExiledColors { source } => {
            super::effects::mana::exiled_color_options(state, *source, object_id)
        }
        // CR 605.3b + CR 106.1a: Filter lands — union of all colors appearing
        // across the combination options. Used for UI frame-color display
        // (`display_land_mana_colors`). The shard assigner in `casting_costs`
        // does NOT consume this — it uses `atomic_combination` on each
        // source row so combos are picked atomically.
        ManaProduction::ChoiceAmongCombinations { options } => {
            let mut out = Vec::new();
            for combo in options {
                for color in combo {
                    let mana_type = mana_color_to_type(color);
                    if !out.contains(&mana_type) {
                        out.push(mana_type);
                    }
                }
            }
            out
        }
        // CR 106.1: Mixed colorless + colored (e.g. {C}{W}, {C}{C}{R}).
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let mut out = Vec::new();
            if *colorless_count > 0 {
                out.push(ManaType::Colorless);
            }
            for color in colors {
                let mana_type = mana_color_to_type(color);
                if !out.contains(&mana_type) {
                    out.push(mana_type);
                }
            }
            out
        }
        // CR 903.4 + CR 903.4f: Compute colors dynamically from the
        // activator's commander(s)' combined color identity.
        ManaProduction::AnyInCommandersColorIdentity { .. } => {
            super::commander::commander_color_identity(state, controller)
                .iter()
                .map(mana_color_to_type)
                .collect()
        }
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
    for (object_id, obj) in state.objects.iter() {
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
            for mana_type in mana_options_from_production(state, controller, *object_id, produced) {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
        // Fallback: basic-land subtype-only objects (no explicit mana ability).
        // Check whether this specific object contributed any colors above — if not,
        // fall back to its land subtypes. (Must be per-object, not global, otherwise
        // once any land adds a color via an explicit ability, later basic lands with
        // no explicit ability silently skip the fallback.)
        let obj_had_explicit_ability = obj.abilities.iter().any(|ability| {
            if ability.kind != AbilityKind::Activated
                || !super::mana_abilities::is_mana_ability(ability)
                || !has_tap_component(&ability.cost)
            {
                return false;
            }
            !matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors { .. },
                    ..
                }
            )
        });
        if !obj_had_explicit_ability {
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
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ManaContribution, QuantityExpr, TargetFilter,
    };
    use crate::types::identifiers::CardId;

    fn verge_ability(color: ManaColor) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    fn brushland_colored_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green, ManaColor::White],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
            },
        ))
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
        assert!(!options[0].harms_controller);
        assert!(options[0].undoable);
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

        use crate::types::ability::{ManaContribution, ManaProduction, QuantityExpr, TargetFilter};
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
                    contribution: ManaContribution::Base,
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
        assert!(
            options.iter().all(|o| !o.harms_controller),
            "Treasure should not be treated as a controller-harming source"
        );
        assert!(
            options.iter().all(|o| !o.undoable),
            "Treasure activations are irreversible because they sacrifice the source"
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
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::PayLife {
                        amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                ],
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
        assert!(
            options.iter().all(|o| o.harms_controller),
            "pay-life mana sources should be flagged as controller-harming"
        );
        assert!(
            options.iter().all(|o| !o.undoable),
            "pay-life mana sources should not be undoable"
        );
    }

    #[test]
    fn pain_land_option_marks_controller_harm_and_non_undoable() {
        let mut state = GameState::new_two_player(42);
        let brushland = create_object(
            &mut state,
            CardId(304),
            PlayerId(0),
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&brushland).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(brushland_colored_ability());

        let options = activatable_land_mana_options(&state, brushland, PlayerId(0));
        assert_eq!(
            options.len(),
            2,
            "Brushland should expose green and white rows"
        );
        assert!(
            options.iter().all(|o| o.harms_controller),
            "pain-land colored rows should be marked as controller-harming"
        );
        assert!(
            options.iter().all(|o| !o.undoable),
            "pain-land activations with damage continuations are not pure-mana undoable"
        );
    }
}
