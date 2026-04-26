//! Landfall feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification (Phase A task 1) — VERIFIED:
//! - `TriggerMode::ChangesZone` captures land-ETB events
//!   (see `crates/engine/src/types/triggers.rs:24-27`, CR 603.6a).
//! - `ControllerRef::You` vs `ControllerRef::Opponent` distinguish controller
//!   in trigger filters (see `crates/engine/src/types/ability.rs:813-818`).
//! - `Zone::Battlefield` is recoverable from `TriggerDefinition.destination`
//!   (see `crates/engine/src/types/ability.rs:4443`).
//!
//! No parser remediation required — landfall-shaped triggers can be
//! structurally classified using existing typed AST.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, CostCategory, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;

/// CR 603.6a: Per-deck landfall classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.triggers`, `CardFace.abilities`, and `CardFace.static_abilities` —
/// never by card name. Policies consume this feature to weight fetch-cracking
/// and other land-drop decisions.
#[derive(Debug, Clone, Default)]
pub struct LandfallFeature {
    /// Cards whose triggers fire on a land you control entering the battlefield.
    pub payoff_count: u32,
    /// Cards that produce extra land-ETB events (fetches, Azusa-likes, Oracle of Mul Daya).
    pub enabler_count: u32,
    /// `0.0..=1.0` — how central landfall is to this deck. Consumed by
    /// `landfall_timing`'s `activation()` as the single scaling knob.
    pub commitment: f32,
    /// Names of detected payoff cards. These are *not* used for structural
    /// classification — classification already happened against the AST. They
    /// are used as battlefield identifiers at decision time so the policy can
    /// answer "is a payoff currently on my battlefield?" via object-name match.
    /// This is not the "name matching" anti-pattern the feature lint forbids.
    pub payoff_names: Vec<String>,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards as landfall payoffs and/or enablers.
pub fn detect(deck: &[DeckEntry]) -> LandfallFeature {
    if deck.is_empty() {
        return LandfallFeature::default();
    }

    let mut payoff_count = 0u32;
    let mut enabler_count = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if is_landfall_payoff(face) {
            payoff_count = payoff_count.saturating_add(entry.count);
            payoff_names.push(face.name.clone());
        }
        if is_landfall_enabler(face) {
            enabler_count = enabler_count.saturating_add(entry.count);
        }
    }

    // Commitment scales roughly with payoff density. Payoffs dominate
    // because enablers alone (e.g., a fetchland cycle in a non-landfall
    // deck) don't indicate landfall intent.
    let commitment = f32::min(1.0, 0.3 * payoff_count as f32 + 0.1 * enabler_count as f32);

    LandfallFeature {
        payoff_count,
        enabler_count,
        commitment,
        payoff_names,
    }
}

/// A payoff has a `TriggerMode::ChangesZone` trigger firing when a land
/// you control enters the battlefield. CR 603.6a.
fn is_landfall_payoff(face: &CardFace) -> bool {
    face.triggers.iter().any(is_landfall_trigger)
}

fn is_landfall_trigger(trigger: &engine::types::ability::TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::ChangesZone {
        return false;
    }
    // Destination must be battlefield (CR 603.6a — "enters the battlefield").
    if trigger.destination != Some(Zone::Battlefield) {
        return false;
    }
    // Origin, when set, must NOT be battlefield (otherwise it's a "leaves"
    // trigger masquerading as ChangesZone). Unset origin == "from anywhere".
    if matches!(trigger.origin, Some(Zone::Battlefield)) {
        return false;
    }
    let Some(filter) = trigger.valid_card.as_ref() else {
        return false;
    };
    filter_matches_land_you_control(filter)
}

/// Unwraps a `TargetFilter` and returns true if it matches a Land whose
/// controller is `You`. Opponent-scoped triggers never count.
fn filter_matches_land_you_control(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_land_you_control(typed),
        TargetFilter::Or { filters } => filters.iter().any(filter_matches_land_you_control),
        // CR 109.3 conjunction: every constraint of an `And` must hold. An
        // `And { [Typed(Land, You), Typed(Flying)] }` is not a "land you
        // control" match — there are no flying lands. Use `.all()` so a
        // conjunction only matches when every sub-constraint is itself a
        // land-you-control match (mirrors `engine::game::filter`'s `.all()`).
        TargetFilter::And { filters } => filters.iter().all(filter_matches_land_you_control),
        _ => false,
    }
}

fn typed_filter_is_land_you_control(typed: &TypedFilter) -> bool {
    let controller_you = matches!(
        typed.controller,
        Some(engine::types::ability::ControllerRef::You)
    );
    if !controller_you {
        return false;
    }
    typed.type_filters.iter().any(type_filter_is_land)
}

pub(crate) fn type_filter_is_land(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_land),
        _ => false,
    }
}

/// An enabler either:
/// - Has an activated ability whose cost sacrifices a permanent AND whose
///   effect (including sub-abilities) searches the library for a land and
///   puts it onto the battlefield — i.e., a fetchland
///   (CR 701.21 sacrifice + CR 305.4 put-land-onto-battlefield + CR 701.23 search).
/// - Has a static granting extra land drops (`AdditionalLandDrop`,
///   `MayPlayAdditionalLand`) — Azusa / Exploration / Oracle of Mul Daya
///   (CR 305.2).
fn is_landfall_enabler(face: &CardFace) -> bool {
    has_fetch_shaped_ability(face) || has_extra_land_drop_static(face)
}

fn has_fetch_shaped_ability(face: &CardFace) -> bool {
    face.abilities.iter().any(|ability| {
        ability_sacrifices_permanent(ability) && ability_searches_library_for_land(ability)
    })
}

fn ability_sacrifices_permanent(ability: &AbilityDefinition) -> bool {
    // Use the single authority for costs — never destructure `AbilityCost::Sacrifice`.
    ability
        .cost_categories()
        .contains(&CostCategory::SacrificesPermanent)
}

/// Walk the ability's effect chain looking for a `SearchLibrary` whose filter
/// matches a Land, followed (in the same chain) by a `ChangeZone` to the
/// battlefield. The canonical fetchland shape is
/// `SearchLibrary { filter: Land ... } → ChangeZone { destination: Battlefield }`.
pub(crate) fn ability_searches_library_for_land(ability: &AbilityDefinition) -> bool {
    let effects = collect_chain_effects(ability);
    let searches_land = effects.iter().any(|e| {
        matches!(
            e,
            Effect::SearchLibrary { filter, .. } if target_filter_references_land(filter)
        )
    });
    let puts_onto_battlefield = effects.iter().any(|e| {
        matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        )
    });
    searches_land && puts_onto_battlefield
}

pub(crate) fn target_filter_references_land(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_land),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_land)
        }
        _ => false,
    }
}

fn has_extra_land_drop_static(face: &CardFace) -> bool {
    face.static_abilities.iter().any(|s| {
        matches!(
            s.mode,
            StaticMode::AdditionalLandDrop { .. } | StaticMode::MayPlayAdditionalLand
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, TriggerDefinition,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::statics::StaticMode;

    fn card_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    fn payoff_trigger() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::You),
            ))
            .destination(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
            ))
    }

    fn opponent_scope_trigger() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::Opponent),
            ))
            .destination(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
            ))
    }

    fn fetch_ability() -> AbilityDefinition {
        let search = Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            up_to: false,
        };
        let put_in_play = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        );
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, search);
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });
        ability.sub_ability = Some(Box::new(put_in_play));
        ability
    }

    #[test]
    fn detects_landfall_payoff() {
        let mut face = card_face("Landfall Payoff");
        face.triggers.push(payoff_trigger());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.payoff_count, 1);
        assert_eq!(feature.enabler_count, 0);
        assert_eq!(feature.payoff_names, vec!["Landfall Payoff".to_string()]);
        assert!(feature.commitment > 0.0);
    }

    #[test]
    fn detects_fetchland_as_enabler() {
        let mut face = card_face("Generic Fetchland");
        face.card_type.core_types = vec![CoreType::Land];
        face.abilities.push(fetch_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.payoff_count, 0);
        assert_eq!(feature.enabler_count, 1);
        assert!(feature.payoff_names.is_empty());
    }

    #[test]
    fn detects_additional_land_drop_static_as_enabler() {
        let mut face = card_face("Ramp Creature");
        face.static_abilities
            .push(engine::types::ability::StaticDefinition::new(
                StaticMode::AdditionalLandDrop { count: 2 },
            ));
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.enabler_count, 1);
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let face = card_face("Vanilla Bear");
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.payoff_count, 0);
        assert_eq!(feature.enabler_count, 0);
        assert_eq!(feature.commitment, 0.0);
        assert!(feature.payoff_names.is_empty());
    }

    #[test]
    fn opponent_scope_trigger_ignored() {
        let mut face = card_face("Punisher");
        face.triggers.push(opponent_scope_trigger());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.payoff_count, 0);
        assert!(feature.payoff_names.is_empty());
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.payoff_count, 0);
        assert_eq!(feature.enabler_count, 0);
        assert_eq!(feature.commitment, 0.0);
        assert!(feature.payoff_names.is_empty());
    }

    #[test]
    fn commitment_clamps_to_one() {
        let mut payoff_a = card_face("Payoff A");
        payoff_a.triggers.push(payoff_trigger());
        let mut payoff_b = card_face("Payoff B");
        payoff_b.triggers.push(payoff_trigger());
        let mut payoff_c = card_face("Payoff C");
        payoff_c.triggers.push(payoff_trigger());
        let mut fetch = card_face("Fetch");
        fetch.card_type.core_types = vec![CoreType::Land];
        fetch.abilities.push(fetch_ability());

        let deck = vec![
            entry(payoff_a, 1),
            entry(payoff_b, 1),
            entry(payoff_c, 1),
            entry(fetch, 4),
        ];
        let feature = detect(&deck);
        assert_eq!(feature.payoff_count, 3);
        assert_eq!(feature.enabler_count, 4);
        assert!((feature.commitment - 1.0).abs() < 1e-5);
    }
}
