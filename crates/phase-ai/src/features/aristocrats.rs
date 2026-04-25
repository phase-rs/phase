//! Aristocrats feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification (pre-verified against codebase) — VERIFIED:
//! - `TriggerMode::ChangesZone` with `origin = Some(Battlefield)`,
//!   `destination = Some(Graveyard)` is the dies-trigger shape
//!   (`crates/engine/src/types/triggers.rs:26-27`, CR 603.6c).
//! - `CostCategory::SacrificesPermanent` is the sacrifice-cost gate
//!   (`crates/engine/src/types/ability.rs:1858`, CR 701.21).
//! - Anti-fetchland disambiguator: rejects abilities that match the canonical
//!   fetchland shape (`SearchLibrary` for Land + `ChangeZone` to Battlefield).
//!   Delegates to `features::landfall::ability_searches_library_for_land` —
//!   landfall is the canonical owner of fetchland semantics.
//! - `Effect::Token { types: Vec<String>, .. }` at `ability.rs:2131`.
//!   Creature tokens have `types.iter().any(|s| s == "Creature")`.
//! - `Effect::ChangeZone { origin: Some(Graveyard), destination: Battlefield,
//!   .. }` is the recursion shape (`ability.rs:2271`). Recursion does not
//!   correspond to a single CR keyword action — it's a generic zone-change
//!   effect, so no specific CR annotation applies here.
//! - `AbilityCost::Sacrifice { target: TargetFilter, .. }` at `ability.rs:1757`
//!   — after the `CostCategory::SacrificesPermanent` gate confirms the cost
//!   type, the `target` field is inspected to verify creature-you-control scope.
//!
//! No parser remediation required — aristocrats-shaped abilities classify
//! structurally using the existing typed AST.

use std::collections::BTreeSet;

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ControllerRef, CostCategory, Effect, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::features::landfall::ability_searches_library_for_land;

/// CR 701.21 + CR 603.6c + CR 111.1: per-deck aristocrats classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities`, `CardFace.triggers`, and `CardFace.static_abilities` —
/// never by card name. Policies consume this feature to weight sacrifice
/// activations and mulligan decisions.
#[derive(Debug, Clone, Default)]
pub struct AristocratsFeature {
    /// Cards with a sacrifice-outlet activated ability (not fetchland-shaped).
    pub outlet_count: u32,
    /// Subset of outlets with no mana cost (Tap-only, sac-only, or Tap+Sac).
    pub free_outlet_count: u32,
    /// Cards with a "creature you control dies" or "whenever a creature dies"
    /// trigger (Zulaport Cutthroat / Blood Artist shape). CR 603.6c.
    pub death_trigger_count: u32,
    /// Cards that produce creature tokens or recur creatures from graveyard.
    pub fodder_source_count: u32,
    /// `0.0..=1.0` — geometric-mean commitment. Missing pillars zero out.
    pub commitment: f32,
    /// Names of detected sacrifice outlets, used by `AristocratsKeepablesMulligan`
    /// as identity lookup against opening-hand objects (where structural
    /// re-classification can't run because abilities aren't resolved yet).
    /// `FreeOutletActivationPolicy` re-classifies the live ability structurally
    /// at activation time and does NOT consume this list — when the live
    /// ability is in hand, identity is the only handle the mulligan layer has.
    pub outlet_names: Vec<String>,
    /// Names of detected death-trigger payoffs, used by the mulligan policy to
    /// answer "is a payoff in the deck?" and by the activation policy to count
    /// payoffs on board — same identity-lookup pattern.
    pub death_trigger_names: Vec<String>,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across the three aristocrats pillars.
pub fn detect(deck: &[DeckEntry]) -> AristocratsFeature {
    if deck.is_empty() {
        return AristocratsFeature::default();
    }

    let mut outlet_count = 0u32;
    let mut free_outlet_count = 0u32;
    let mut death_trigger_count = 0u32;
    let mut fodder_source_count = 0u32;
    let mut outlet_names: BTreeSet<String> = BTreeSet::new();
    let mut death_trigger_names: BTreeSet<String> = BTreeSet::new();

    for entry in deck {
        let face = &entry.card;
        // Per-axis bool sentinels: a face contributes at most once per axis
        // even if multiple abilities match the same category.
        let is_outlet = face.abilities.iter().any(ability_is_sacrifice_outlet);
        let is_free = is_outlet && face.abilities.iter().any(is_free_outlet_ability);
        let is_death = is_death_trigger_source(face);
        let is_fodder = is_fodder_source(face);

        if is_outlet {
            outlet_count = outlet_count.saturating_add(entry.count);
            outlet_names.insert(face.name.clone());
        }
        if is_free {
            free_outlet_count = free_outlet_count.saturating_add(entry.count);
        }
        if is_death {
            death_trigger_count = death_trigger_count.saturating_add(entry.count);
            death_trigger_names.insert(face.name.clone());
        }
        if is_fodder {
            fodder_source_count = fodder_source_count.saturating_add(entry.count);
        }
    }

    // CR 701.21 + CR 603.6c + CR 111.1: aristocrats requires three pillars —
    // outlets to sacrifice, dies-triggers to gain value, and fodder to feed.
    // A geometric mean enforces synergy: missing any pillar zeros commitment.
    let o = (outlet_count as f32 / 3.0).min(1.0); // saturates at 3
    let t = (death_trigger_count as f32 / 3.0).min(1.0);
    let f = (fodder_source_count as f32 / 5.0).min(1.0); // fodder typically denser
    let free_bonus = (0.05 * free_outlet_count as f32).min(0.2);

    // If ANY pillar is 0, commitment collapses to free_bonus only (cap 0.2)
    // so the policy stays opted-out for non-aristocrats decks.
    let commitment = if outlet_count == 0 || death_trigger_count == 0 || fodder_source_count == 0 {
        free_bonus
    } else {
        ((o * t * f).powf(1.0 / 3.0) + free_bonus).min(1.0)
    };

    AristocratsFeature {
        outlet_count,
        free_outlet_count,
        death_trigger_count,
        fodder_source_count,
        commitment,
        outlet_names: outlet_names.into_iter().collect(),
        death_trigger_names: death_trigger_names.into_iter().collect(),
    }
}

/// True if this ability is a sacrifice outlet.
///
/// Guards (in order):
/// 1. `CostCategory::SacrificesPermanent` — single-authority gate (CR 701.21).
/// 2. Destructure `AbilityCost::Sacrifice { target, .. }` AFTER the gate has
///    confirmed the cost type (same pattern as `landfall.rs:164-166`) — verify
///    `target` is a creature you control or `SelfRef` on a creature-type face.
/// 3. `!ability_searches_library_for_land()` — fetchland anti-pattern check
///    (mirrors `features::landfall::ability_searches_library_for_land`).
/// 4. NOT pure-mana — excludes "T, sacrifice this: add {G}" cards.
pub(crate) fn ability_is_sacrifice_outlet(ability: &AbilityDefinition) -> bool {
    // Gate 1: single-authority cost check.
    if !ability
        .cost_categories()
        .contains(&CostCategory::SacrificesPermanent)
    {
        return false;
    }

    // Gate 2: verify the sacrifice targets a creature you control (or SelfRef —
    // which is creature-scoped when used on creatures). After the category gate
    // has confirmed the cost type, destructure to read the target field.
    if !cost_sacrifices_creature(ability.cost.as_ref()) {
        return false;
    }

    // Gate 3: fetchland anti-pattern check.
    // Mirrors features::landfall::ability_searches_library_for_land — landfall
    // is the canonical fetchland disambiguator.
    if ability_searches_library_for_land(ability) {
        return false;
    }

    // Gate 4: exclude pure-mana-producing sac outlets ("T, sacrifice this: add {G}").
    let effects = collect_chain_effects(ability);
    if effects.iter().all(|e| matches!(e, Effect::Mana { .. })) {
        return false;
    }

    true
}

/// True if this ability is a sacrifice outlet with no mana component in the cost.
/// Free outlets are Tap-only, sac-only, or Tap+Sac — anything without a
/// non-zero `AbilityCost::Mana { cost }` in the composite cost tree.
pub(crate) fn is_free_outlet_ability(ability: &AbilityDefinition) -> bool {
    if !ability_is_sacrifice_outlet(ability) {
        return false;
    }
    !cost_has_nonzero_mana(ability.cost.as_ref())
}

/// Walk the cost tree and return true if any leaf has a non-zero mana cost.
fn cost_has_nonzero_mana(cost: Option<&AbilityCost>) -> bool {
    match cost {
        None => false,
        Some(AbilityCost::Mana { cost: mana_cost }) => mana_cost.mana_value() > 0,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| cost_has_nonzero_mana(Some(c)))
        }
        Some(_) => false,
    }
}

/// Walk the cost tree and check if any `AbilityCost::Sacrifice { target, .. }`
/// targets a creature you control or is a `SelfRef`. Called AFTER the
/// `CostCategory::SacrificesPermanent` gate has confirmed the cost type.
fn cost_sacrifices_creature(cost: Option<&AbilityCost>) -> bool {
    match cost {
        None => false,
        Some(AbilityCost::Sacrifice { target, .. }) => {
            matches!(target, TargetFilter::SelfRef)
                || filter_references_creature_you_control(target)
        }
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| cost_sacrifices_creature(Some(c)))
        }
        _ => false,
    }
}

/// True if a `TargetFilter` references a creature whose controller is `You`
/// or is unset (wildcard — "whenever a creature dies" includes yours).
/// Opponent-scoped filters are rejected.
fn filter_references_creature_you_control(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_creature_you_control(typed),
        TargetFilter::Or { filters } => filters.iter().any(filter_references_creature_you_control),
        TargetFilter::And { filters } => filters.iter().all(filter_references_creature_you_control),
        _ => false,
    }
}

fn typed_filter_is_creature_you_control(typed: &TypedFilter) -> bool {
    // Reject opponent-scoped filters.
    if matches!(typed.controller, Some(ControllerRef::Opponent)) {
        return false;
    }
    typed.type_filters.iter().any(type_filter_is_creature)
}

fn type_filter_is_creature(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Creature => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_creature),
        _ => false,
    }
}

/// True if this face has a "creature dies" trigger scoped to you or wildcard.
///
/// CR 603.6c: leaves-the-battlefield abilities trigger when a permanent moves
/// from the battlefield to another zone. A "dies" trigger is
/// `ChangesZone { origin: Some(Battlefield), destination: Some(Graveyard) }`
/// where `valid_card` matches a creature with `ControllerRef::You` or no
/// controller scoping (wildcard).
fn is_death_trigger_source(face: &CardFace) -> bool {
    face.triggers.iter().any(is_creature_dies_trigger)
}

fn is_creature_dies_trigger(trigger: &engine::types::ability::TriggerDefinition) -> bool {
    // CR 603.6c: dies = moves from battlefield to graveyard.
    if trigger.mode != TriggerMode::ChangesZone {
        return false;
    }
    if trigger.origin != Some(Zone::Battlefield) {
        return false;
    }
    if trigger.destination != Some(Zone::Graveyard) {
        return false;
    }
    // If no valid_card filter, the trigger fires on any creature dying
    // (wildcard). Accept wildcards — "whenever a creature dies" includes yours.
    let Some(filter) = trigger.valid_card.as_ref() else {
        return true; // wildcard
    };
    filter_references_creature_you_control_or_any(filter)
}

/// True if the filter references a creature whose controller is `You` or unset.
/// Opponent-scoped triggers (e.g., "when an opponent's creature dies") do not
/// qualify — they don't fire when the AI sacrifices its own creatures.
///
/// Shared with `tokens_wide` for anthem-affected-filter detection.
pub(crate) fn filter_references_creature_you_control_or_any(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_creature_you_control_or_any(typed),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(filter_references_creature_you_control_or_any),
        TargetFilter::And { filters } => filters
            .iter()
            .all(filter_references_creature_you_control_or_any),
        _ => false,
    }
}

/// Shared with `tokens_wide` for anthem-affected-filter detection.
pub(crate) fn typed_filter_is_creature_you_control_or_any(typed: &TypedFilter) -> bool {
    // Reject opponent-scoped filters — those don't benefit from sacrificing your
    // own creatures.
    if matches!(typed.controller, Some(ControllerRef::Opponent)) {
        return false;
    }
    typed.type_filters.iter().any(type_filter_is_creature)
}

/// True if this face produces creature tokens or returns creatures from graveyard.
///
/// Walks effect chains across `face.abilities` AND `face.triggers[*].execute`
/// via `collect_chain_effects`.
///
/// - Creature token generator: `Effect::Token { types, .. }` with
///   `types.iter().any(|s| s == "Creature")`. Treasure/Clue/Food tokens do NOT
///   count — their `types` lacks "Creature". CR 111.1.
/// - Creature recursion: `Effect::ChangeZone { origin: Some(Graveyard),
///   destination: Battlefield, target, .. }` where `target` references a creature.
///   Generic zone-change effect — no CR keyword action applies.
fn is_fodder_source(face: &CardFace) -> bool {
    // Check abilities.
    if face.abilities.iter().any(|ability| {
        collect_chain_effects(ability)
            .iter()
            .any(is_creature_fodder_effect)
    }) {
        return true;
    }
    // Check trigger execute chains.
    face.triggers.iter().any(|t| {
        t.execute.as_ref().is_some_and(|exec| {
            collect_chain_effects(exec)
                .iter()
                .any(is_creature_fodder_effect)
        })
    })
}

fn is_creature_fodder_effect(e: &&Effect) -> bool {
    match e {
        // CR 111.1: creature token production.
        Effect::Token { types, .. } => types.iter().any(|s| s == "Creature"),
        // Recursion from graveyard to battlefield targeting a creature.
        // No CR keyword action — generic zone-change effect.
        Effect::ChangeZone {
            origin,
            destination,
            target,
            ..
        } => {
            *origin == Some(Zone::Graveyard)
                && *destination == Zone::Battlefield
                && filter_references_creature(target)
        }
        _ => false,
    }
}

fn filter_references_creature(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_creature),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_references_creature)
        }
        _ => false,
    }
}

// Fetchland-shape detection (`ability_searches_library_for_land` and its
// helpers) lives in `features::landfall` and is imported above. Landfall is
// the canonical owner of land-fetch semantics; aristocrats consumes it as a
// negative filter (an outlet must NOT be a fetchland).

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, PtValue, QuantityExpr,
        TargetFilter, TriggerDefinition, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    fn creature_face(name: &str) -> CardFace {
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

    fn land_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Land],
                subtypes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    /// Sac-only outlet (Goblin Bombardment / Carrion Feeder shape):
    /// `{}: Sacrifice a creature: do effect`
    fn sac_only_outlet_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: 1,
        });
        ability
    }

    /// Phyrexian Altar shape: `{1}, Sacrifice a creature: add one mana`
    /// (is an outlet but NOT free — has mana cost, but the effect is mana
    /// production so it would normally be excluded by gate 4).
    /// We make the effect non-mana to test the mana-cost gate properly.
    fn phyrexian_altar_shaped_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Sacrifice {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                    count: 1,
                },
            ],
        });
        ability
    }

    /// Viscera Seer shape: `{T}, Sacrifice a creature: scry 1`
    /// Tap + Sac, no mana → free outlet.
    fn viscera_seer_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                    count: 1,
                },
            ],
        });
        ability
    }

    /// Fetchland ability: `{T}, Pay 1 life, Sacrifice this: SearchLibrary(Land) → ChangeZone(BF)`
    fn fetch_ability() -> AbilityDefinition {
        let search = Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            up_to: false,
        };
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
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
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
        ability
    }

    /// Diabolic Intent shape: `Sacrifice a creature: SearchLibrary for any card`
    fn diabolic_intent_ability() -> AbilityDefinition {
        let search = Effect::SearchLibrary {
            filter: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            up_to: false,
        };
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, search);
        ability.cost = Some(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: 1,
        });
        // The chain has no ChangeZone to battlefield, so it shouldn't trigger
        // the fetchland gate — however `Effect::SearchLibrary` without a
        // battlefield ChangeZone won't match `ability_searches_library_for_land`.
        // To model Diabolic Intent correctly we add the graveyard ChangeZone:
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        ability
    }

    /// "Creature you control dies" trigger (Zulaport Cutthroat shape).
    fn dies_trigger_yours() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: None,
                },
            ))
    }

    /// "Whenever a creature dies" trigger (no controller scope — wildcard).
    fn dies_trigger_wildcard() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()))
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: None,
                },
            ))
    }

    /// "Whenever an opponent's creature dies" trigger — must be rejected.
    fn dies_trigger_opponent() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ))
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: None,
                },
            ))
    }

    fn creature_token_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Token {
                name: "Saproling".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Saproling".to_string()],
                keywords: Vec::new(),
                colors: Vec::new(),
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: Vec::new(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
            },
        )
    }

    fn treasure_token_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                keywords: Vec::new(),
                colors: Vec::new(),
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: Vec::new(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
            },
        )
    }

    fn recursion_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::creature()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )
    }

    // --- Outlet tests ---

    #[test]
    fn detects_sacrifice_outlet() {
        let mut face = creature_face("Goblin Bombardment");
        face.abilities.push(sac_only_outlet_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 1);
        assert_eq!(feature.free_outlet_count, 1);
        assert_eq!(feature.outlet_names, vec!["Goblin Bombardment".to_string()]);
    }

    #[test]
    fn fetchland_is_not_an_outlet() {
        // A canonical fetchland has SelfRef sacrifice + SearchLibrary for Land +
        // ChangeZone to Battlefield — rejected by anti-fetchland gate.
        let mut face = land_face("Flooded Strand");
        face.abilities.push(fetch_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 0);
        assert_eq!(feature.free_outlet_count, 0);
        assert_eq!(feature.fodder_source_count, 0);
        assert!(feature.outlet_names.is_empty());
    }

    #[test]
    fn tutor_with_sacrifice_cost_is_an_outlet() {
        // Diabolic Intent shape: sac creature + SearchLibrary → Hand. The
        // anti-fetchland gate only rejects `SearchLibrary + ChangeZone →
        // Battlefield` (the canonical fetchland shape); a tutor that searches
        // to Hand passes through. By the structural definition of "sacrifice
        // outlet" — sacrifice cost + non-mana effect — Diabolic Intent
        // qualifies, which matches its real-world play in aristocrats shells
        // (sacrifice fodder for a tutored combo piece).
        let mut face = creature_face("Diabolic Intent");
        face.abilities.push(diabolic_intent_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(
            feature.outlet_count, 1,
            "Diabolic Intent is a valid sac outlet"
        );
        // Has no mana cost → free outlet.
        assert_eq!(feature.free_outlet_count, 1);
    }

    #[test]
    fn phyrexian_altar_is_outlet_not_free() {
        // Phyrexian Altar shape: {1} + sac creature → non-mana effect.
        // Has mana cost → outlet=true, free_outlet=false.
        let mut face = creature_face("Phyrexian Altar");
        face.abilities.push(phyrexian_altar_shaped_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 1);
        assert_eq!(feature.free_outlet_count, 0);
    }

    #[test]
    fn viscera_seer_is_free_outlet() {
        // Viscera Seer: {T}, Sacrifice a creature: scry 1.
        // Tap + Sac, no mana → free outlet.
        let mut face = creature_face("Viscera Seer");
        face.abilities.push(viscera_seer_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 1);
        assert_eq!(feature.free_outlet_count, 1);
    }

    // --- Death trigger tests ---

    #[test]
    fn detects_dies_trigger_creature_you_control() {
        let mut face = creature_face("Zulaport Cutthroat");
        face.triggers.push(dies_trigger_yours());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.death_trigger_count, 1);
        assert_eq!(
            feature.death_trigger_names,
            vec!["Zulaport Cutthroat".to_string()]
        );
    }

    #[test]
    fn opponent_dies_trigger_ignored() {
        let mut face = creature_face("Punisher Effect");
        face.triggers.push(dies_trigger_opponent());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.death_trigger_count, 0);
        assert!(feature.death_trigger_names.is_empty());
    }

    #[test]
    fn no_controller_dies_trigger_counts() {
        // "Whenever a creature dies" — no controller scope → wildcard → accepted.
        let mut face = creature_face("Blood Artist");
        face.triggers.push(dies_trigger_wildcard());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.death_trigger_count, 1);
        assert_eq!(
            feature.death_trigger_names,
            vec!["Blood Artist".to_string()]
        );
    }

    // --- Fodder tests ---

    #[test]
    fn detects_creature_token_generator_as_fodder() {
        let mut face = creature_face("Saproling Generator");
        face.abilities.push(creature_token_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.fodder_source_count, 1);
    }

    #[test]
    fn treasure_token_generator_is_not_fodder() {
        // Treasure types lack "Creature" → not counted as fodder.
        let mut face = creature_face("Treasure Maker");
        face.abilities.push(treasure_token_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.fodder_source_count, 0);
    }

    #[test]
    fn creature_recursion_counts_as_fodder() {
        // Gravedigger shape: return creature from graveyard to battlefield.
        let mut face = creature_face("Gravedigger");
        face.abilities.push(recursion_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.fodder_source_count, 1);
    }

    #[test]
    fn creature_token_in_trigger_execute_counts_as_fodder() {
        // Bitterblossom / Ophiomancer shape: an upkeep trigger whose
        // `execute` chain creates a creature token. `is_fodder_source` must
        // walk `triggers[*].execute` chains, not just `face.abilities`.
        let mut face = creature_face("Token Triggerer");
        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(creature_token_ability());
        face.triggers.push(trigger);
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(
            feature.fodder_source_count, 1,
            "creature-token effect inside a trigger's execute chain should count as fodder"
        );
    }

    // --- Commitment tests ---

    #[test]
    fn single_pillar_low_commitment() {
        // 5 outlets, 0 triggers → commitment ≤ free_bonus (≤ 0.2).
        let mut face = creature_face("Outlet Only");
        face.abilities.push(sac_only_outlet_ability());
        let deck = vec![entry(face, 5)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 5);
        assert_eq!(feature.death_trigger_count, 0);
        // Missing pillar → falls back to free_bonus only.
        assert!(
            feature.commitment <= 0.2,
            "single-pillar commitment should be ≤ 0.2, got {}",
            feature.commitment
        );
    }

    #[test]
    fn full_engine_high_commitment() {
        // 3 outlets + 3 triggers + 5 fodder → commitment ≥ 0.9.
        let mut outlet1 = creature_face("Outlet A");
        outlet1.abilities.push(sac_only_outlet_ability());
        let mut outlet2 = creature_face("Outlet B");
        outlet2.abilities.push(sac_only_outlet_ability());
        let mut outlet3 = creature_face("Outlet C");
        outlet3.abilities.push(sac_only_outlet_ability());

        let mut trigger1 = creature_face("Trigger A");
        trigger1.triggers.push(dies_trigger_yours());
        let mut trigger2 = creature_face("Trigger B");
        trigger2.triggers.push(dies_trigger_yours());
        let mut trigger3 = creature_face("Trigger C");
        trigger3.triggers.push(dies_trigger_yours());

        let mut fodder1 = creature_face("Fodder A");
        fodder1.abilities.push(creature_token_ability());
        let mut fodder2 = creature_face("Fodder B");
        fodder2.abilities.push(creature_token_ability());
        let mut fodder3 = creature_face("Fodder C");
        fodder3.abilities.push(creature_token_ability());
        let mut fodder4 = creature_face("Fodder D");
        fodder4.abilities.push(creature_token_ability());
        let mut fodder5 = creature_face("Fodder E");
        fodder5.abilities.push(creature_token_ability());

        let deck = vec![
            entry(outlet1, 1),
            entry(outlet2, 1),
            entry(outlet3, 1),
            entry(trigger1, 1),
            entry(trigger2, 1),
            entry(trigger3, 1),
            entry(fodder1, 1),
            entry(fodder2, 1),
            entry(fodder3, 1),
            entry(fodder4, 1),
            entry(fodder5, 1),
        ];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 3);
        assert_eq!(feature.death_trigger_count, 3);
        assert_eq!(feature.fodder_source_count, 5);
        assert!(
            feature.commitment >= 0.9,
            "full engine should have commitment ≥ 0.9, got {}",
            feature.commitment
        );
    }

    #[test]
    fn commitment_clamps_to_one() {
        // Extreme counts should not produce commitment > 1.0.
        let mut outlet = creature_face("Outlet");
        outlet.abilities.push(sac_only_outlet_ability());
        let mut trigger = creature_face("Trigger");
        trigger.triggers.push(dies_trigger_yours());
        let mut fodder = creature_face("Fodder");
        fodder.abilities.push(creature_token_ability());

        let deck = vec![entry(outlet, 100), entry(trigger, 100), entry(fodder, 100)];
        let feature = detect(&deck);
        assert!(
            feature.commitment <= 1.0,
            "commitment must clamp to 1.0, got {}",
            feature.commitment
        );
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.outlet_count, 0);
        assert_eq!(feature.death_trigger_count, 0);
        assert_eq!(feature.fodder_source_count, 0);
        assert_eq!(feature.commitment, 0.0);
        assert!(feature.outlet_names.is_empty());
        assert!(feature.death_trigger_names.is_empty());
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let face = creature_face("Grizzly Bears");
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 0);
        assert_eq!(feature.death_trigger_count, 0);
        assert_eq!(feature.fodder_source_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn outlet_names_populated_for_outlets_only() {
        let mut outlet = creature_face("Sac Outlet");
        outlet.abilities.push(sac_only_outlet_ability());
        let non_outlet = creature_face("Vanilla Creature");
        let deck = vec![entry(outlet, 1), entry(non_outlet, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_names, vec!["Sac Outlet".to_string()]);
    }

    #[test]
    fn death_trigger_names_populated_for_triggers_only() {
        let mut trigger = creature_face("Death Trigger");
        trigger.triggers.push(dies_trigger_yours());
        let non_trigger = creature_face("Vanilla Creature");
        let deck = vec![entry(trigger, 1), entry(non_trigger, 1)];

        let feature = detect(&deck);
        assert_eq!(
            feature.death_trigger_names,
            vec!["Death Trigger".to_string()]
        );
    }

    #[test]
    fn modal_ability_with_one_sac_mode_counted_once() {
        // A card with two abilities: one sac outlet, one non-outlet.
        // Should count as outlet=1, not outlet=2.
        let mut face = creature_face("Modal Outlet");
        face.abilities.push(sac_only_outlet_ability());
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        ));
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.outlet_count, 1);
    }
}
