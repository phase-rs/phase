//! Shared mana-color extraction: which colors a land can produce.
//!
//! One building block used by both draft fixing-land evaluation
//! (`draft_eval::produced_color_count`) and the mulligan land-count keepables
//! (`policies::mulligan::keepables_by_land_count`). Operates on *parts*
//! (`subtypes` + `abilities`) so a `GameObject` view and a `CardFace` view share
//! a single implementation, mirroring the `*_parts` pattern in `features`.

use engine::game::mana_payment::{land_subtype_to_mana_type, outer_cost_color_demand, ColorDemand};
use engine::game::mana_sources::mana_color_to_type;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ManaProduction};
use engine::types::game_state::GameState;
use engine::types::mana::ManaType;

/// Distinct colored-mana types a land can produce, unioning (a) intrinsic mana
/// from its basic land subtypes (a typed dual like "Land — Plains Island" makes
/// W and U with no printed `Effect::Mana`) and (b) the colors of every activated
/// `Effect::Mana` ability (painlands, filter lands, etc.). Colorless never counts
/// as a color, so the length is the count of *colored* sources — `>= 2` marks a
/// fixing land.
pub fn land_produced_color_types(
    subtypes: &[String],
    abilities: &[AbilityDefinition],
) -> Vec<ManaType> {
    let mut colors = Vec::new();
    for subtype in subtypes {
        if let Some(mana_type) = land_subtype_to_mana_type(subtype) {
            push_color(&mut colors, mana_type);
        }
    }
    for ability in abilities {
        if ability.kind != AbilityKind::Activated {
            continue;
        }
        let Effect::Mana { produced, .. } = &*ability.effect else {
            continue;
        };
        collect_mana_production_colors(&mut colors, produced);
    }
    colors
}

/// Union the colors of a single `ManaProduction` into `colors` (deduplicated,
/// colorless excluded). Exhaustive over every `ManaProduction` variant: the
/// statically-known producers (Fixed/Mixed/AnyOneColor/AnyCombination, and the
/// filter-land `ChoiceAmongCombinations`) contribute their colors; the dynamic
/// producers (chosen/opponent/commander-identity/etc.) and pure Colorless
/// contribute nothing, since their colors aren't known from the card alone.
pub(crate) fn collect_mana_production_colors(
    colors: &mut Vec<ManaType>,
    produced: &ManaProduction,
) {
    match produced {
        ManaProduction::Fixed {
            colors: produced, ..
        }
        | ManaProduction::Mixed {
            colors: produced, ..
        }
        | ManaProduction::AnyOneColor {
            color_options: produced,
            ..
        }
        | ManaProduction::AnyCombination {
            color_options: produced,
            ..
        } => {
            for color in produced {
                push_color(colors, mana_color_to_type(color));
            }
        }
        ManaProduction::ChoiceAmongCombinations { options } => {
            for option in options {
                for color in option {
                    push_color(colors, mana_color_to_type(color));
                }
            }
        }
        ManaProduction::Colorless { .. }
        | ManaProduction::ChosenColor { .. }
        | ManaProduction::OpponentLandColors { .. }
        | ManaProduction::AnyTypeProduceableBy { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::AnyInCommandersColorIdentity { .. }
        | ManaProduction::DistinctColorsAmongPermanents { .. }
        | ManaProduction::AnyOneColorAmongPermanents { .. }
        // CR 202.2c: Omnath, Locus of All — colors come from a target object
        // resolved at trigger time, not known from the card alone.
        | ManaProduction::AnyCombinationOfObjectColors { .. }
        | ManaProduction::TriggerEventManaType => {}
    }
}

fn push_color(colors: &mut Vec<ManaType>, mana_type: ManaType) {
    if mana_type != ManaType::Colorless && !colors.contains(&mana_type) {
        colors.push(mana_type);
    }
}

/// Pick which color a flexible mana source (dual land, Mox Opal, City of Brass)
/// should produce when answering a `ManaChoicePrompt::SingleColor` *during a
/// pending cast*. The AI must produce the color the in-flight spell actually
/// demands, not the first option in the list: tapping a U/R source for {R} when
/// the spell needs {U} strands the colored pip and dead-ends the ManaPayment.
///
/// Returns the first option whose WUBRG colored demand of the pending cast is
/// nonzero; if none of the options match a demanded color (generic-only cost, no
/// pending cast), falls back to the first option — identical to the old `first()`
/// behavior, so this is a strict improvement everywhere.
///
/// Limitation: `pending_cast.cost` is the FULL locked outer cost, not decremented
/// per-pip as colors are produced. Demand is therefore exact for single-colored-pip
/// costs (the repro: {2}{U}, {5}{U}) and a strict improvement over `first()` for
/// all costs. Incremental per-pip demand tracking for multi-colored-pip costs
/// (e.g. {U}{U}{R}, where two blues must be produced before red) is a documented
/// follow-up, out of scope here.
pub(crate) fn demand_aware_single_color(
    options: &[ManaType],
    state: &GameState,
) -> Option<ManaType> {
    let demand: ColorDemand = state
        .pending_cast
        .as_deref()
        .map(|pc| outer_cost_color_demand(&pc.cost))
        .unwrap_or([0u32; 5]);

    options
        .iter()
        .copied()
        .find(|&opt| match opt {
            // WUBRG demand slot per color; Colorless has none, so it never
            // satisfies a colored pip.
            ManaType::White => demand[0] > 0,
            ManaType::Blue => demand[1] > 0,
            ManaType::Black => demand[2] > 0,
            ManaType::Red => demand[3] > 0,
            ManaType::Green => demand[4] > 0,
            ManaType::Colorless => false,
        })
        .or_else(|| options.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
    use engine::types::game_state::PendingCast;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::player::PlayerId;

    fn state_with_cost(shards: Vec<ManaCostShard>, generic: u32) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(100),
                PlayerId(0),
            ),
            ManaCost::Cost { shards, generic },
        )));
        state
    }

    #[test]
    fn picks_demanded_blue_over_first_red() {
        // {2}{U}: a U/R source offered [Red, Blue] must produce Blue (the
        // demanded color), not Red (the first option).
        let state = state_with_cost(vec![ManaCostShard::Blue], 2);
        assert_eq!(
            demand_aware_single_color(&[ManaType::Red, ManaType::Blue], &state),
            Some(ManaType::Blue)
        );
    }

    #[test]
    fn generic_only_cost_falls_back_to_first() {
        // {2}: no colored demand, so the first option (Red) is fine.
        let state = state_with_cost(Vec::new(), 2);
        assert_eq!(
            demand_aware_single_color(&[ManaType::Red, ManaType::Blue], &state),
            Some(ManaType::Red)
        );
    }

    #[test]
    fn no_pending_cast_falls_back_to_first() {
        let state = GameState::new_two_player(42);
        assert!(state.pending_cast.is_none());
        assert_eq!(
            demand_aware_single_color(&[ManaType::Red, ManaType::Blue], &state),
            Some(ManaType::Red)
        );
    }

    #[test]
    fn empty_options_returns_none() {
        let state = state_with_cost(vec![ManaCostShard::Blue], 2);
        assert_eq!(demand_aware_single_color(&[], &state), None);
    }
}
