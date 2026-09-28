//! Mana ramp feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - Activated mana ability: `AbilityKind::Activated` at `crates/engine/src/types/ability.rs:3747`;
//!   `AbilityCost::Tap` at `ability.rs:1883`; `Effect::Mana { produced: ManaProduction, .. }`
//!   at `ability.rs:2520`; `ManaProduction` at `ability.rs:476`.
//! - Cost lookup via `AbilityDefinition::cost_categories()` at `ability.rs:4031` yielding
//!   `CostCategory::TapsSelf` at `ability.rs:1883`.
//! - Sorcery/instant rituals: same `Effect::Mana`, hung off `AbilityKind::Spell` at
//!   `ability.rs:3749`; differentiated by `CardFace.card_type.core_types` containing
//!   `CoreType::Instant` or `CoreType::Sorcery` (`card_type.rs:74-77`).
//! - Land-fetch: `Effect::SearchLibrary { filter, .. }` at `ability.rs:2557` →
//!   `TargetFilter::Typed(TypedFilter)` → `TypeFilter::Land` at `ability.rs:778`;
//!   followed by `Effect::ChangeZone { destination: Zone::Battlefield | Zone::Hand, .. }`
//!   at `ability.rs:2271`. Walk chain via `phase_ai::ability_chain::collect_chain_effects`.
//! - Additional land drops: `StaticMode::AdditionalLandDrop { count }` at
//!   `crates/engine/src/types/statics.rs:268`, `StaticMode::MayPlayAdditionalLand`
//!   at `statics.rs:280`.
//! - Controller scoping: `TypedFilter.controller: Option<ControllerRef>` at
//!   `ability.rs:815-818`.
//!
//! `StaticMode::ModifyCost` is deliberately out of scope here — this axis
//! measures mana *added* to the pool. Cost reducers (cost *removed* from spells,
//! CR 601.2f) are the disjoint `features::cost_reduction` axis; a card is never
//! counted by both.

use engine::analysis::deck_signals::{self, DeckCard};
use engine::game::DeckEntry;

use crate::features::commitment;

pub use engine::analysis::deck_signals::mana_ramp::{
    chain_has_mana_effect, is_extra_landdrop_parts, is_land_fetch_spell_parts, is_mana_dork_parts,
    is_ramp_piece_parts, is_ritual_parts, target_filter_references_land,
};

/// CR 106.1 + CR 605.1a: per-deck mana ramp classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities` and `CardFace.static_abilities` — never by card name.
/// Policies consume this feature to weight ramp timing and mulligan decisions.
#[derive(Debug, Clone, Default)]
pub struct ManaRampFeature {
    /// Tap-for-mana permanents — creatures (mana dorks) AND artifact mana rocks
    /// (Sol Ring shape). Activated ability with `CostCategory::TapsSelf` and a
    /// `Effect::Mana` anywhere in the chain.
    pub dork_count: u32,
    /// Sorcery or instant spells that search the library for a land and put it
    /// onto the battlefield or into hand (Cultivate / Rampant Growth shape).
    pub land_fetch_count: u32,
    /// Non-permanent spells with `Effect::Mana` in their ability chain that are
    /// NOT land-fetch spells (Dark Ritual shape). Disjoint from `land_fetch_count`.
    pub ritual_count: u32,
    /// Cards granting extra land drops via `StaticMode::AdditionalLandDrop` or
    /// `StaticMode::MayPlayAdditionalLand` (Azusa / Exploration shape).
    pub extra_landdrop_count: u32,
    /// `0.0..=1.0` — how central mana ramp is to this deck. Consumed by
    /// `RampTimingPolicy`'s `activation()` as the single scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across four independent ramp axes.
pub fn detect(deck: &[DeckEntry]) -> ManaRampFeature {
    let cards = DeckCard::from_deck_entries(deck);
    let signals = deck_signals::mana_ramp::detect(&cards);
    let total_nonland = deck_signals::nonland_card_count(&cards);

    // CR 106.1 + CR 605.1a: ramp accelerates mana availability. Payoff weight
    // mirrors landfall — dorks and fetches dominate; statics multiply actions.
    let commitment = commitment::weighted_sum(&[
        (
            0.12,
            commitment::density_per_60(signals.dork_count, total_nonland),
        ),
        (
            0.10,
            commitment::density_per_60(signals.land_fetch_count, total_nonland),
        ),
        (
            0.08,
            commitment::density_per_60(signals.ritual_count, total_nonland),
        ),
        (
            0.20,
            commitment::density_per_60(signals.extra_landdrop_count, total_nonland),
        ),
    ]);

    ManaRampFeature {
        dork_count: signals.dork_count,
        land_fetch_count: signals.land_fetch_count,
        ritual_count: signals.ritual_count,
        extra_landdrop_count: signals.extra_landdrop_count,
        commitment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaContribution,
        ManaProduction, QuantityExpr, SearchSelectionConstraint, StaticDefinition, TargetFilter,
        TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::statics::StaticMode;
    use engine::types::zones::{EtbTapState, Zone};

    fn face(name: &str, core_type: CoreType) -> CardFace {
        CardFace {
            name: name.into(),
            card_type: CardType {
                core_types: vec![core_type],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn mana_effect() -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: Vec::new(),
                contribution: ManaContribution::Base,
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    #[test]
    fn mana_ramp_feature_fields_match_engine_signals() {
        let mut dork = face("Dork", CoreType::Creature);
        let mut dork_ability = AbilityDefinition::new(AbilityKind::Activated, mana_effect());
        dork_ability.cost = Some(AbilityCost::Tap);
        dork.abilities.push(dork_ability);

        let mut fetch = face("Fetch", CoreType::Sorcery);
        let mut fetch_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
        );
        fetch_ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )));
        fetch.abilities.push(fetch_ability);

        let mut ritual = face("Ritual", CoreType::Instant);
        ritual
            .abilities
            .push(AbilityDefinition::new(AbilityKind::Spell, mana_effect()));

        let mut extra_land = face("Exploration", CoreType::Enchantment);
        extra_land
            .static_abilities
            .push(StaticDefinition::new(StaticMode::MayPlayAdditionalLand));

        let deck = vec![
            DeckEntry {
                card: dork,
                count: 2,
            },
            DeckEntry {
                card: fetch,
                count: 3,
            },
            DeckEntry {
                card: ritual,
                count: 4,
            },
            DeckEntry {
                card: extra_land,
                count: 5,
            },
        ];
        let cards = DeckCard::from_deck_entries(&deck);
        let signals = deck_signals::mana_ramp::detect(&cards);
        let total_nonland = deck_signals::nonland_card_count(&cards);
        let feature = detect(&deck);

        assert_eq!(feature.dork_count, signals.dork_count);
        assert_eq!(feature.land_fetch_count, signals.land_fetch_count);
        assert_eq!(feature.ritual_count, signals.ritual_count);
        assert_eq!(feature.extra_landdrop_count, signals.extra_landdrop_count);

        let commitment = commitment::weighted_sum(&[
            (
                0.12,
                commitment::density_per_60(signals.dork_count, total_nonland),
            ),
            (
                0.10,
                commitment::density_per_60(signals.land_fetch_count, total_nonland),
            ),
            (
                0.08,
                commitment::density_per_60(signals.ritual_count, total_nonland),
            ),
            (
                0.20,
                commitment::density_per_60(signals.extra_landdrop_count, total_nonland),
            ),
        ]);
        assert_eq!(feature.commitment, commitment);
    }
}
