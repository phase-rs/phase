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
//! `StaticMode::ReduceCost` is deliberately out of scope — cost reducers are a
//! follow-up feature.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, CostCategory, Effect, StaticDefinition,
    TargetFilter, TypeFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;

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
    if deck.is_empty() {
        return ManaRampFeature::default();
    }

    let mut dork_count = 0u32;
    let mut land_fetch_count = 0u32;
    let mut ritual_count = 0u32;
    let mut extra_landdrop_count = 0u32;

    for entry in deck {
        let face = &entry.card;
        // Each axis is independent — a card may match multiple axes (e.g., Azusa
        // is both an extra-landdrop enabler and a creature). Count each axis
        // separately via a per-axis bool sentinel to avoid double-counting within
        // the same axis for multi-ability cards.
        if is_mana_dork(face) {
            dork_count = dork_count.saturating_add(entry.count);
        }
        if is_land_fetch_spell(face) {
            land_fetch_count = land_fetch_count.saturating_add(entry.count);
        } else if is_ritual(face) {
            // Rituals and fetches are disjoint: a spell that searches for a land
            // is a fetch (not a ritual) even if it also produces mana in-chain.
            ritual_count = ritual_count.saturating_add(entry.count);
        }
        if is_extra_landdrop(face) {
            extra_landdrop_count = extra_landdrop_count.saturating_add(entry.count);
        }
    }

    // CR 106.1 + CR 605.1a: ramp accelerates mana availability. Payoff weight
    // mirrors landfall — dorks and fetches dominate; statics multiply actions.
    let commitment = f32::min(
        1.0,
        0.12 * dork_count as f32
            + 0.10 * land_fetch_count as f32
            + 0.08 * ritual_count as f32
            + 0.20 * extra_landdrop_count as f32,
    );

    ManaRampFeature {
        dork_count,
        land_fetch_count,
        ritual_count,
        extra_landdrop_count,
        commitment,
    }
}

/// A mana dork is a creature or artifact with an activated tap ability that
/// produces mana. Basic lands are explicitly excluded — they are `CoreType::Land`,
/// not `CoreType::Creature` or `CoreType::Artifact`.
///
/// CR 605.1a: activated mana ability — doesn't require a target, could add
/// mana when it resolves, is not a loyalty ability.
pub(crate) fn is_mana_dork(face: &CardFace) -> bool {
    is_mana_dork_parts(&face.card_type.core_types, &face.abilities)
}

/// Parts-based variant of [`is_mana_dork`] for callers holding a `GameObject`
/// rather than a `CardFace`. Both live views carry the same underlying data;
/// passing slices keeps the single source of truth without forcing a
/// `GameObject → CardFace` conversion. See also [`policies::ramp_timing`] and
/// [`policies::mulligan::ramp_keepables`] which delegate here.
pub(crate) fn is_mana_dork_parts(core_types: &[CoreType], abilities: &[AbilityDefinition]) -> bool {
    let is_permanent = core_types
        .iter()
        .any(|t| matches!(t, CoreType::Creature | CoreType::Artifact));
    if !is_permanent {
        return false;
    }
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Activated
            && ability.cost_categories().contains(&CostCategory::TapsSelf)
            && chain_has_mana_effect(ability)
    })
}

/// A land-fetch spell is an instant or sorcery that searches the library for
/// a land and moves it to the battlefield or hand. Opponent-scoped destinations
/// are excluded (CR 305.4 — "you put" pattern).
///
/// CR 701.23: search — the effect instructs the controller to look through
/// their library for a land card. CR 305.4: putting a land onto the
/// battlefield does not count as playing a land.
pub(crate) fn is_land_fetch_spell(face: &CardFace) -> bool {
    is_land_fetch_spell_parts(&face.card_type.core_types, &face.abilities)
}

pub(crate) fn is_land_fetch_spell_parts(
    core_types: &[CoreType],
    abilities: &[AbilityDefinition],
) -> bool {
    let is_instant_or_sorcery = core_types
        .iter()
        .any(|t| matches!(t, CoreType::Instant | CoreType::Sorcery));
    if !is_instant_or_sorcery {
        return false;
    }
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && chain_searches_for_land(ability)
            && chain_puts_land_to_safe_zone(ability)
    })
}

/// A ritual is a non-permanent spell with `Effect::Mana` in its chain that is
/// NOT a land-fetch spell. Disjoint with `is_land_fetch_spell`.
///
/// CR 605.5b: a spell can never be a mana ability — it is cast and resolves
/// normally. Rituals are instants/sorceries that produce mana as their effect.
pub(crate) fn is_ritual(face: &CardFace) -> bool {
    is_ritual_parts(&face.card_type.core_types, &face.abilities)
}

pub(crate) fn is_ritual_parts(core_types: &[CoreType], abilities: &[AbilityDefinition]) -> bool {
    let is_instant_or_sorcery = core_types
        .iter()
        .any(|t| matches!(t, CoreType::Instant | CoreType::Sorcery));
    if !is_instant_or_sorcery {
        return false;
    }
    // Rituals and land-fetches must remain disjoint — a Cultivate-shape spell
    // that also produces mana in-chain is a fetch, not a ritual.
    if is_land_fetch_spell_parts(core_types, abilities) {
        return false;
    }
    abilities
        .iter()
        .any(|ability| ability.kind == AbilityKind::Spell && chain_has_mana_effect(ability))
}

/// A card grants extra land drops if it has a static ability with
/// `AdditionalLandDrop` or `MayPlayAdditionalLand` mode. CR 305.2.
pub(crate) fn is_extra_landdrop(face: &CardFace) -> bool {
    is_extra_landdrop_parts(&face.static_abilities)
}

pub(crate) fn is_extra_landdrop_parts(static_abilities: &[StaticDefinition]) -> bool {
    static_abilities.iter().any(|s| {
        matches!(
            s.mode,
            StaticMode::AdditionalLandDrop { .. } | StaticMode::MayPlayAdditionalLand
        )
    })
}

/// True when a card (via its typed parts) qualifies as any flavour of ramp —
/// a mana dork/rock, a land-fetch spell, a ritual, or an extra-land-drop
/// permanent (Exploration-shape). Used by the mulligan policy to classify
/// hand contents structurally, without re-implementing the four axis checks.
pub(crate) fn is_ramp_piece_parts(
    core_types: &[CoreType],
    abilities: &[AbilityDefinition],
    static_abilities: &[StaticDefinition],
) -> bool {
    is_mana_dork_parts(core_types, abilities)
        || is_land_fetch_spell_parts(core_types, abilities)
        || is_ritual_parts(core_types, abilities)
        || is_extra_landdrop_parts(static_abilities)
}

/// Walk every effect in the ability's chain and return true if any is
/// `Effect::Mana`. A damage-only chain (e.g., Prodigal Sorcerer) returns false.
pub(crate) fn chain_has_mana_effect(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|e| matches!(e, Effect::Mana { .. }))
}

/// Return true if the ability's effect chain includes `Effect::SearchLibrary`
/// whose filter references a Land (not any-card tutors).
fn chain_searches_for_land(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability).iter().any(|e| {
        matches!(
            e,
            Effect::SearchLibrary { filter, .. } if target_filter_references_land(filter)
        )
    })
}

/// Return true if the chain puts the found card to the battlefield or hand.
/// Rejects opponent-scoped `ChangeZone` targets (CR 305.2 — "your" land drop).
fn chain_puts_land_to_safe_zone(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability).iter().any(|e| match e {
        Effect::ChangeZone {
            destination,
            target,
            ..
        } => {
            matches!(destination, Zone::Battlefield | Zone::Hand)
                && !target_filter_is_opponent_scoped(target)
        }
        _ => false,
    })
}

fn target_filter_references_land(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_land),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_land)
        }
        _ => false,
    }
}

fn type_filter_is_land(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_land),
        _ => false,
    }
}

/// Reject effects targeting an opponent-controlled zone — e.g., a spell that
/// puts a land into the opponent's hand is not ramp for us.
fn target_filter_is_opponent_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => {
            matches!(typed.controller, Some(ControllerRef::Opponent))
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().all(target_filter_is_opponent_scoped)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaContribution,
        ManaProduction, QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    fn card_face_with_types(name: &str, core_types: Vec<CoreType>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types,
                subtypes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    /// Tap-for-mana activated ability (mana dork / mana rock shape).
    fn tap_for_mana_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    /// Sorcery-speed land search → battlefield (Cultivate / Rampant Growth shape).
    fn land_fetch_spell_ability(destination: Zone) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                up_to: false,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        ability
    }

    /// Ritual spell ability (Dark Ritual shape — produces mana, no land search).
    fn ritual_spell_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
            },
        )
    }

    /// Tap-for-damage ability (Prodigal Sorcerer shape — should NOT be a dork).
    fn tap_for_damage_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    #[test]
    fn detects_mana_dork() {
        let mut face = card_face_with_types("Llanowar Elves", vec![CoreType::Creature]);
        face.abilities.push(tap_for_mana_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 4);
        assert_eq!(feature.land_fetch_count, 0);
        assert_eq!(feature.ritual_count, 0);
        assert!(feature.commitment > 0.0);
    }

    #[test]
    fn detects_mana_rock_as_dork() {
        // Sol Ring shape: Artifact with {T}: Add {C}{C}
        let mut face = card_face_with_types("Sol Ring", vec![CoreType::Artifact]);
        face.abilities.push(tap_for_mana_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 1);
    }

    #[test]
    fn detects_land_fetch_sorcery() {
        let mut face = card_face_with_types("Rampant Growth", vec![CoreType::Sorcery]);
        face.abilities
            .push(land_fetch_spell_ability(Zone::Battlefield));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.land_fetch_count, 4);
        assert_eq!(feature.ritual_count, 0);
    }

    #[test]
    fn detects_land_fetch_to_hand() {
        // Crop Rotation / Nature's Lore shape — fetches land to hand.
        let mut face = card_face_with_types("Nature's Lore", vec![CoreType::Sorcery]);
        face.abilities.push(land_fetch_spell_ability(Zone::Hand));
        let deck = vec![entry(face, 2)];

        let feature = detect(&deck);
        assert_eq!(feature.land_fetch_count, 2);
        assert_eq!(feature.ritual_count, 0);
    }

    #[test]
    fn detects_ritual() {
        let mut face = card_face_with_types("Dark Ritual", vec![CoreType::Instant]);
        face.abilities.push(ritual_spell_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.ritual_count, 4);
        assert_eq!(feature.land_fetch_count, 0);
    }

    #[test]
    fn detects_additional_land_drop_static() {
        let mut face = card_face_with_types("Azusa", vec![CoreType::Creature]);
        face.static_abilities
            .push(engine::types::ability::StaticDefinition::new(
                StaticMode::AdditionalLandDrop { count: 2 },
            ));
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.extra_landdrop_count, 1);
    }

    #[test]
    fn detects_may_play_additional_land_static() {
        let mut face = card_face_with_types("Exploration", vec![CoreType::Enchantment]);
        face.static_abilities
            .push(engine::types::ability::StaticDefinition::new(
                StaticMode::MayPlayAdditionalLand,
            ));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.extra_landdrop_count, 4);
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let face = card_face_with_types("Grizzly Bears", vec![CoreType::Creature]);
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 0);
        assert_eq!(feature.land_fetch_count, 0);
        assert_eq!(feature.ritual_count, 0);
        assert_eq!(feature.extra_landdrop_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn basic_land_not_counted_as_dork() {
        // Forest has {T}: Add {G} but is CoreType::Land — must be rejected
        // by the creature/artifact gate before inspecting abilities.
        let mut face = card_face_with_types("Forest", vec![CoreType::Land]);
        face.abilities.push(tap_for_mana_ability());
        let deck = vec![entry(face, 7)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 0);
    }

    #[test]
    fn damage_dealing_tap_ability_not_dork() {
        // Prodigal Sorcerer: {T}: deals 1 damage — no Effect::Mana anywhere.
        let mut face = card_face_with_types("Prodigal Sorcerer", vec![CoreType::Creature]);
        face.abilities.push(tap_for_damage_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 0);
    }

    #[test]
    fn opponent_scope_mana_grant_ignored() {
        // A land-search effect that moves the land to the opponent's hand/BF.
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                up_to: false,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::Opponent),
                ),
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        let mut face = card_face_with_types("Gift Spell", vec![CoreType::Sorcery]);
        face.abilities.push(ability);
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.land_fetch_count, 0);
    }

    #[test]
    fn fetchland_not_counted() {
        // Fetchland: activated ability with sacrifice + search — but no
        // Effect::Mana anywhere, and it's CoreType::Land (not Creature/Artifact),
        // so dork gate rejects it. Not a spell, so not a ritual or fetch-spell.
        let mut fetch_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                up_to: false,
            },
        );
        fetch_ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });
        fetch_ability.sub_ability = Some(Box::new(AbilityDefinition::new(
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
        )));
        let mut face = card_face_with_types("Fetchland", vec![CoreType::Land]);
        face.abilities.push(fetch_ability);
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.dork_count, 0);
        assert_eq!(feature.land_fetch_count, 0);
        assert_eq!(feature.ritual_count, 0);
        assert_eq!(feature.extra_landdrop_count, 0);
    }

    #[test]
    fn modal_spell_with_one_ramp_mode_counted_once() {
        // A spell with two abilities — one ritual, one non-ramp. The card should
        // count as a ritual once (per-axis bool sentinel: `else if` branch).
        let mut face = card_face_with_types("Modal Ramp", vec![CoreType::Sorcery]);
        face.abilities.push(ritual_spell_ability());
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        ));
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        // Only counted once (not twice) because `is_land_fetch_spell` and
        // `is_ritual` both inspect the face; `is_ritual` short-circuits on first
        // matching ability.
        assert_eq!(feature.ritual_count, 1);
        assert_eq!(feature.land_fetch_count, 0);
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.dork_count, 0);
        assert_eq!(feature.land_fetch_count, 0);
        assert_eq!(feature.ritual_count, 0);
        assert_eq!(feature.extra_landdrop_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        let mut dork = card_face_with_types("Mana Elf", vec![CoreType::Creature]);
        dork.abilities.push(tap_for_mana_ability());
        let deck = vec![entry(dork, 40)];

        let feature = detect(&deck);
        assert!((feature.commitment - 1.0).abs() < 1e-5);
    }
}
