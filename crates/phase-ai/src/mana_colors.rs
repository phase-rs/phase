//! Shared mana-color extraction: which colors a land can produce.
//!
//! One building block used by both draft fixing-land evaluation
//! (`draft_eval::produced_color_count`) and the mulligan land-count keepables
//! (`policies::mulligan::keepables_by_land_count`). Operates on *parts*
//! (`subtypes` + `abilities`) so a `GameObject` view and a `CardFace` view share
//! a single implementation, mirroring the `*_parts` pattern in `features`.

use engine::game::mana_payment::land_subtype_to_mana_type;
use engine::game::mana_sources::mana_color_to_type;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ManaProduction};
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
        | ManaProduction::TriggerEventManaType => {}
    }
}

fn push_color(colors: &mut Vec<ManaType>, mana_type: ManaType) {
    if mana_type != ManaType::Colorless && !colors.contains(&mana_type) {
        colors.push(mana_type);
    }
}
