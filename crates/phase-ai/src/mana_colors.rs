//! Shared mana-color extraction: which colors a land can produce.
//!
//! One building block used by both draft fixing-land evaluation
//! (`draft_eval::produced_color_count`) and the mulligan land-count keepables
//! (`policies::mulligan::keepables_by_land_count`). Operates on *parts*
//! (`subtypes` + `abilities`) so a `GameObject` view and a `CardFace` view share
//! a single implementation, mirroring the `*_parts` pattern in `features`.

use engine::game::mana_payment::{land_subtype_to_mana_type, outer_cost_color_demand, ColorDemand};
use engine::game::mana_sources::{activatable_mana_actions_for_player, mana_color_to_type};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ManaProduction};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;

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

/// Whether `mana_type` satisfies a colored pip the in-flight cost still demands.
/// WUBRG demand slot per color; Colorless has no slot, so it never satisfies a
/// colored pip.
fn color_is_demanded(demand: ColorDemand, mana_type: ManaType) -> bool {
    match mana_type {
        ManaType::White => demand[0] > 0,
        ManaType::Blue => demand[1] > 0,
        ManaType::Black => demand[2] > 0,
        ManaType::Red => demand[3] > 0,
        ManaType::Green => demand[4] > 0,
        ManaType::Colorless => false,
    }
}

/// CR 106.3 + CR 608.2d: which color a flexible source produces during a pending
/// cast is mechanical, not a policy judgment — the source must produce a color the
/// in-flight cost demands. True when tapping `source` for `mana_type` satisfies no
/// colored pip of the pending cast *while that same source has a live mana row that
/// would*, i.e. taking this row strands the demanded pip in a `ManaPayment`
/// dead-end (a U/R dual tapped for {R} against a {2}{U} spell).
///
/// The color is carried in each `TapLandForMana` candidate's
/// `ManaSourceSelection::mana_type`, so the choice is expressed as a *set of
/// candidates* and must be resolved by eliminating the stranding ones rather than
/// by returning a color.
///
/// Deliberately scoped to the color only: it never compares two different sources,
/// so *which* source to tap remains the strategic judgment it should be. If the
/// source cannot produce a demanded color at all, nothing is stranded and this is
/// false — tapping it for an undemanded color may still be a fine way to pay
/// generic.
///
pub(crate) fn tap_strands_demanded_color(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    mana_type: ManaType,
) -> bool {
    let Some(pending_cast) = state.pending_cast.as_deref() else {
        return false;
    };
    let demand = outer_cost_color_demand(&pending_cast.cost);
    if color_is_demanded(demand, mana_type) {
        return false;
    }
    // Only reached for an undemanded color, so the enumeration is off the hot
    // path for every correctly-colored tap.
    activatable_mana_actions_for_player(state, player)
        .iter()
        .any(|action| match action {
            GameAction::TapLandForMana { selection } => {
                selection.source.object_id == source
                    && color_is_demanded(demand, selection.mana_type)
            }
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
    use engine::types::game_state::PendingCast;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::player::PlayerId;

    /// Battlefield fixture: one land for `P0` with `oracle_text`, plus a pending
    /// cast of `{2}{U}` — the shape of the measured `Metallic Rebuke` repro.
    fn state_with_land(oracle_text: &str) -> (GameState, ObjectId) {
        let mut scenario = engine::game::scenario::GameScenario::new();
        let land = scenario
            .add_land_from_oracle(PlayerId(0), "Test Dual", oracle_text)
            .id();
        let runner = scenario.build();
        let mut state = runner.state().clone();
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
        )));
        (state, land)
    }

    /// The repro, at the unit level: a U/R source tapped for {R} against {2}{U}
    /// strands the blue pip, because that same source could have produced {U}.
    #[test]
    fn strands_when_source_can_produce_the_demanded_color() {
        let (state, land) = state_with_land("{T}: Add {U} or {R}.");
        assert!(tap_strands_demanded_color(
            &state,
            PlayerId(0),
            land,
            ManaType::Red
        ));
    }

    /// The demanded color is never stranding — this is the row the gate must keep.
    #[test]
    fn demanded_color_does_not_strand() {
        let (state, land) = state_with_land("{T}: Add {U} or {R}.");
        assert!(!tap_strands_demanded_color(
            &state,
            PlayerId(0),
            land,
            ManaType::Blue
        ));
    }

    /// Sibling negative — the reason this is scoped to ONE source. A mono-red
    /// source cannot produce {U}, so tapping it for {R} strands nothing: that is
    /// a legitimate way to pay the {2} generic. Rejecting it would break ordinary
    /// payment, which is the over-reject failure this test exists to catch.
    #[test]
    fn undemanded_color_does_not_strand_when_source_cannot_produce_the_demand() {
        let (state, land) = state_with_land("{T}: Add {R}.");
        assert!(!tap_strands_demanded_color(
            &state,
            PlayerId(0),
            land,
            ManaType::Red
        ));
    }

    /// No pending cast ⇒ no in-flight demand ⇒ nothing to strand.
    #[test]
    fn no_pending_cast_never_strands() {
        let (mut state, land) = state_with_land("{T}: Add {U} or {R}.");
        state.pending_cast = None;
        assert!(!tap_strands_demanded_color(
            &state,
            PlayerId(0),
            land,
            ManaType::Red
        ));
    }
}
