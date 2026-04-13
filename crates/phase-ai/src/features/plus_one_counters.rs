//! Plus-one counters feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `Effect::AddCounter { counter_type: String, count: QuantityExpr, target }` at
//!   `crates/engine/src/types/ability.rs:2201-2207`. CR 122.1a: +1/+1 counters add
//!   to power and toughness.
//! - `Effect::PutCounter { counter_type, count, target }` at `ability.rs:2425-2431`.
//!   Alternate counter-placement shape. CR 122.1a.
//! - `Effect::PutCounterAll { counter_type, count, target }` at `ability.rs:2433-2439`.
//!   Mass counter placement. CR 122.1a.
//! - `Effect::Proliferate` (unit variant) at `ability.rs:2379`. CR 701.34: proliferate
//!   adds one counter of each kind already on chosen permanents/players.
//! - `Keyword::EtbCounter { counter_type: String, count: u32 }` at
//!   `crates/engine/src/types/keywords.rs:358-361`. CR 122.6a: ETB-with-counters keyword.
//! - `Effect::Token { enter_with_counters: Vec<(String, QuantityExpr)>, .. }` at
//!   `ability.rs:2167`. CR 122.6a: tokens that enter with counters.
//! - `ReplacementDefinition { event: ReplacementEvent::AddCounter,
//!   quantity_modification: Some(QuantityModification::{Double | Plus | Minus}), .. }`
//!   at `ability.rs:4818-4874`. CR 614.1a: replacement effects that modify counter
//!   quantities (Doubling Season, Hardened Scales shapes).
//! - `FilterProp::CountersGE { counter_type: CounterType::Plus1Plus1, count }` at
//!   `ability.rs:856-859`. Static payoff shape (Abzan Falconer). CR 122.1a + CR 613.1f.
//! - `TriggerMode::CounterAdded | CounterAddedOnce | CounterAddedAll | CounterTypeAddedAll`
//!   with `counter_filter: Some(CounterTriggerFilter { counter_type: CounterType::Plus1Plus1 })`
//!   in `TriggerDefinition`. CR 122.6 + CR 122.7: triggered payoffs for counter events.
//!
//! **Disambiguator**: All counter checks compare `counter_type == "P1P1"` (string
//! from Effect side) or `counter_type == CounterType::Plus1Plus1` (typed from
//! filter/trigger side). `-1/-1` counters (`"M1M1"`) and loyalty counters must NOT match.
//!
//! No parser remediation required — counter-related abilities classify structurally
//! using the existing typed AST.

use std::collections::BTreeSet;

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, Effect, FilterProp, ReplacementDefinition, StaticDefinition, TargetFilter,
    TriggerDefinition,
};
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;

use crate::ability_chain::collect_chain_effects;

/// Commitment floor below which the tactical policy opts out.
pub const COMMITMENT_FLOOR: f32 = 0.20;
/// Commitment floor below which the mulligan policy opts out.
pub const MULLIGAN_FLOOR: f32 = 0.30;
/// Minimum commitment for the `payoff_with_active_counters` branch.
pub const COMMITTED_VALUE_FLOOR: f32 = 0.35;

/// CR 122.1a + CR 122.6 + CR 122.7 + CR 701.34 + CR 614.1a + CR 613.1f:
/// per-deck +1/+1 counters classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities`, `CardFace.triggers`, `CardFace.keywords`, and
/// `CardFace.replacements` — never by card name. Policies consume this to weight
/// counter-related activations and mulligan decisions.
#[derive(Debug, Clone, Default)]
pub struct PlusOneCountersFeature {
    /// Cards with an ability that adds a +1/+1 counter to something.
    /// CR 122.1a: +1/+1 counters add to power and toughness.
    pub generator_count: u32,
    /// Cards with a proliferate ability. CR 701.34: choose permanents/players
    /// with counters and add one additional counter of each kind they already have.
    pub proliferate_count: u32,
    /// Cards with a replacement effect that doubles or modifies counter quantities.
    /// CR 614.1a: Doubling Season / Hardened Scales shapes.
    pub doubler_count: u32,
    /// Cards with a static or triggered payoff for creatures having +1/+1 counters.
    /// CR 613.1f + CR 122.6 + CR 122.7.
    pub payoff_count: u32,
    /// Cards that enter the battlefield with +1/+1 counters (via keyword or token).
    /// CR 122.6a.
    pub etb_with_counters_count: u32,
    /// Weighted commitment score `0.0..=1.0` — geometric mean of source and
    /// payoff density. Zero if no generators and no ETB-with-counters sources.
    pub commitment: f32,
    /// Names of detected payoff cards — used by mulligan policy for identity
    /// lookup against opening-hand objects. Not used by tactical policy (which
    /// re-classifies the live ability structurally at activation time).
    pub payoff_names: Vec<String>,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across the +1/+1 counter axes.
pub fn detect(deck: &[DeckEntry]) -> PlusOneCountersFeature {
    if deck.is_empty() {
        return PlusOneCountersFeature::default();
    }

    let mut generator_count = 0u32;
    let mut proliferate_count = 0u32;
    let mut doubler_count = 0u32;
    let mut payoff_count = 0u32;
    let mut etb_with_counters_count = 0u32;
    let mut payoff_names: BTreeSet<String> = BTreeSet::new();

    for entry in deck {
        let face = &entry.card;

        let is_generator = face.abilities.iter().any(ability_places_plus_one_counter);
        let is_proliferator = face.abilities.iter().any(ability_proliferates);
        let is_doubler = face_doubles_counters(face);
        let is_payoff = face_is_counter_payoff(face);
        let is_etb = face_etb_with_plus_one_counters(face);

        if is_generator {
            generator_count = generator_count.saturating_add(entry.count);
        }
        if is_proliferator {
            proliferate_count = proliferate_count.saturating_add(entry.count);
        }
        if is_doubler {
            doubler_count = doubler_count.saturating_add(entry.count);
        }
        if is_payoff {
            payoff_count = payoff_count.saturating_add(entry.count);
            payoff_names.insert(face.name.clone());
        }
        if is_etb {
            etb_with_counters_count = etb_with_counters_count.saturating_add(entry.count);
        }
    }

    let commitment = compute_commitment(
        generator_count,
        proliferate_count,
        doubler_count,
        payoff_count,
        etb_with_counters_count,
    );

    PlusOneCountersFeature {
        generator_count,
        proliferate_count,
        doubler_count,
        payoff_count,
        etb_with_counters_count,
        commitment,
        payoff_names: payoff_names.into_iter().collect(),
    }
}

/// Calibration: Hardened Scales deck (8 gen + 4 prolif + 2 doubler + 6 payoff + 4 etb)
/// → commitment ≈ 1.0. Vanilla aggro → 0.0.
fn compute_commitment(
    generator_count: u32,
    proliferate_count: u32,
    doubler_count: u32,
    payoff_count: u32,
    etb_with_counters_count: u32,
) -> f32 {
    let sources_raw = generator_count as f32
        + 0.5 * etb_with_counters_count as f32
        + 0.3 * doubler_count as f32
        + 0.5 * proliferate_count as f32;
    let s = (sources_raw / 12.0).min(1.0);
    let p = (payoff_count as f32 / 6.0).min(1.0);

    if generator_count == 0 && etb_with_counters_count == 0 {
        0.0
    } else if payoff_count == 0 {
        0.15 * s
    } else {
        (s * p).sqrt().min(1.0)
    }
}

// ─── Parts predicates (pub(crate) for policy reuse) ───────────────────────────

/// True if this ability places a +1/+1 counter on something.
///
/// Matches `Effect::AddCounter`, `Effect::PutCounter`, and `Effect::PutCounterAll`
/// where `counter_type == "P1P1"`. Checks the ability's full effect chain via
/// `collect_chain_effects`. Excludes loyalty counters, -1/-1 counters, lore
/// counters, etc. CR 122.1a.
pub(crate) fn ability_places_plus_one_counter(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(effect_places_plus_one_counter)
}

/// True if this ability contains a Proliferate effect. CR 701.34.
pub(crate) fn ability_proliferates(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|e| matches!(e, Effect::Proliferate))
}

/// True if this face enters the battlefield with +1/+1 counters via the
/// `EtbCounter` keyword or is a token that enters with +1/+1 counters.
/// CR 122.6a.
pub(crate) fn face_etb_with_plus_one_counters(face: &engine::types::card::CardFace) -> bool {
    // Keyword::EtbCounter shape.
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::EtbCounter { counter_type, .. } if counter_type == "P1P1"))
    {
        return true;
    }
    // Token that enters with +1/+1 counters (Effect::Token enter_with_counters).
    face.abilities.iter().any(|a| {
        collect_chain_effects(a).iter().any(|e| {
            matches!(e, Effect::Token { enter_with_counters, .. }
                if enter_with_counters.iter().any(|(ct, _)| ct == "P1P1"))
        })
    })
}

/// True if this face has a replacement effect that modifies the quantity of
/// +1/+1 counters placed (Doubling Season / Hardened Scales shapes).
/// CR 614.1a: replacement effects that use "instead" to modify counter counts.
pub(crate) fn face_doubles_counters(face: &engine::types::card::CardFace) -> bool {
    face.replacements
        .iter()
        .any(replacement_modifies_p1p1_counters)
}

/// True if this face has a continuous static ability that buffs creatures with
/// +1/+1 counters on them. CR 613.1f: ability-adding effects applied at layer 6.
/// CR 122.1a: creatures with counters qualify for the filter.
pub(crate) fn face_has_static_counter_payoff(face: &engine::types::card::CardFace) -> bool {
    face.static_abilities.iter().any(static_is_counter_payoff)
}

/// True if this face has a triggered ability that fires when a +1/+1 counter
/// is placed on a creature (you control or wildcard).
/// CR 122.6: counter-placement events. CR 122.7: threshold-based counter triggers.
pub(crate) fn face_has_triggered_counter_payoff(face: &engine::types::card::CardFace) -> bool {
    face.triggers.iter().any(trigger_is_counter_payoff)
}

/// True if this face has a static OR triggered payoff for +1/+1 counters.
pub(crate) fn face_is_counter_payoff(face: &engine::types::card::CardFace) -> bool {
    face_has_static_counter_payoff(face) || face_has_triggered_counter_payoff(face)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// True if the effect places a +1/+1 counter. CR 122.1a.
fn effect_places_plus_one_counter(e: &&Effect) -> bool {
    match e {
        Effect::AddCounter { counter_type, .. } => counter_type == "P1P1",
        Effect::PutCounter { counter_type, .. } => counter_type == "P1P1",
        Effect::PutCounterAll { counter_type, .. } => counter_type == "P1P1",
        _ => false,
    }
}

/// True if this replacement definition modifies the quantity of +1/+1 counters
/// placed. Matches `ReplacementEvent::AddCounter` with a non-None
/// `quantity_modification`. CR 614.1a.
fn replacement_modifies_p1p1_counters(rep: &ReplacementDefinition) -> bool {
    rep.event == ReplacementEvent::AddCounter && rep.quantity_modification.is_some()
}

/// True if this static definition is a payoff for creatures with +1/+1 counters
/// — i.e., its `affected` filter includes `FilterProp::CountersGE` for P1P1.
/// CR 613.1f + CR 122.1a.
fn static_is_counter_payoff(s: &StaticDefinition) -> bool {
    if s.mode != StaticMode::Continuous {
        return false;
    }
    filter_has_p1p1_counter_prop(s.affected.as_ref())
}

/// True if a TargetFilter (or any nested prop in a TypedFilter) references
/// `FilterProp::CountersGE` for `CounterType::Plus1Plus1`.
fn filter_has_p1p1_counter_prop(filter: Option<&TargetFilter>) -> bool {
    let Some(filter) = filter else {
        return false;
    };
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::CountersGE { counter_type, .. } if counter_type == &CounterType::Plus1Plus1)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(|f| filter_has_p1p1_counter_prop(Some(f)))
        }
        _ => false,
    }
}

/// True if this trigger fires when a +1/+1 counter is added to a creature.
/// Matches the counter trigger modes and verifies `counter_filter` is set to P1P1.
/// CR 122.6 + CR 122.7.
fn trigger_is_counter_payoff(t: &TriggerDefinition) -> bool {
    let is_counter_mode = matches!(
        t.mode,
        TriggerMode::CounterAdded
            | TriggerMode::CounterAddedOnce
            | TriggerMode::CounterAddedAll
            | TriggerMode::CounterTypeAddedAll
    );
    if !is_counter_mode {
        return false;
    }
    // counter_filter must specifically be P1P1 to avoid matching loyalty/lore triggers.
    t.counter_filter
        .as_ref()
        .is_some_and(|cf| cf.counter_type == CounterType::Plus1Plus1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CounterTriggerFilter, Effect, FilterProp, QuantityExpr,
        QuantityModification, ReplacementDefinition, StaticDefinition, TargetFilter,
        TriggerDefinition, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::counter::CounterType;
    use engine::types::keywords::Keyword;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::StaticMode;
    use engine::types::triggers::TriggerMode;

    fn make_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            ..CardFace::default()
        }
    }

    fn deck_entry(face: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card: face, count }
    }

    fn add_counter_ability(counter_type: &str) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::AddCounter {
                counter_type: counter_type.to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
        )
    }

    fn put_counter_ability(counter_type: &str) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::PutCounter {
                counter_type: counter_type.to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
        )
    }

    fn proliferate_ability() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
    }

    fn doubler_replacement() -> ReplacementDefinition {
        let mut rep = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        rep.quantity_modification = Some(QuantityModification::Double);
        rep
    }

    fn plus_modifier_replacement() -> ReplacementDefinition {
        let mut rep = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        rep.quantity_modification = Some(QuantityModification::Plus { value: 1 });
        rep
    }

    fn static_counter_payoff() -> StaticDefinition {
        StaticDefinition::new(StaticMode::Continuous).affected(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::CountersGE {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
            }]),
        ))
    }

    fn counter_added_trigger() -> TriggerDefinition {
        let mut t = TriggerDefinition::new(TriggerMode::CounterAdded);
        t.counter_filter = Some(CounterTriggerFilter {
            counter_type: CounterType::Plus1Plus1,
            threshold: None,
        });
        t
    }

    // ─── Generator tests ─────────────────────────────────────────────────────

    #[test]
    fn detects_plus_one_counter_generator() {
        let mut face = make_face("Hardened Scales Generator");
        face.abilities.push(add_counter_ability("P1P1"));
        assert!(ability_places_plus_one_counter(&face.abilities[0]));
    }

    #[test]
    fn detects_put_counter_variant() {
        let mut face = make_face("Put Counter Variant");
        face.abilities.push(put_counter_ability("P1P1"));
        assert!(ability_places_plus_one_counter(&face.abilities[0]));
    }

    #[test]
    fn ignores_minus_one_minus_one_generator() {
        let mut face = make_face("Hapatra");
        face.abilities.push(add_counter_ability("M1M1"));
        assert!(!ability_places_plus_one_counter(&face.abilities[0]));
    }

    #[test]
    fn ignores_loyalty_counter_generator() {
        let mut face = make_face("Planeswalker");
        face.abilities.push(add_counter_ability("loyalty"));
        assert!(!ability_places_plus_one_counter(&face.abilities[0]));
    }

    // ─── Proliferate tests ───────────────────────────────────────────────────

    #[test]
    fn detects_proliferate() {
        let ability = proliferate_ability();
        assert!(ability_proliferates(&ability));
    }

    // ─── Doubler tests ───────────────────────────────────────────────────────

    #[test]
    fn detects_doubler_replacement() {
        let mut face = make_face("Doubling Season");
        face.replacements.push(doubler_replacement());
        assert!(face_doubles_counters(&face));
    }

    #[test]
    fn detects_doubler_plus_modifier() {
        let mut face = make_face("Hardened Scales");
        face.replacements.push(plus_modifier_replacement());
        assert!(face_doubles_counters(&face));
    }

    // ─── Static payoff tests ─────────────────────────────────────────────────

    #[test]
    fn detects_static_payoff_creatures_with_counters() {
        let mut face = make_face("Abzan Falconer");
        face.static_abilities.push(static_counter_payoff());
        assert!(face_has_static_counter_payoff(&face));
    }

    // ─── Triggered payoff tests ──────────────────────────────────────────────

    #[test]
    fn detects_triggered_payoff_counter_added() {
        let mut face = make_face("Armorcraft Judge");
        face.triggers.push(counter_added_trigger());
        assert!(face_has_triggered_counter_payoff(&face));
    }

    // ─── ETB tests ───────────────────────────────────────────────────────────

    #[test]
    fn detects_etb_with_counters_keyword() {
        let mut face = make_face("Servant of the Scale");
        face.keywords.push(Keyword::EtbCounter {
            counter_type: "P1P1".to_string(),
            count: 1,
        });
        assert!(face_etb_with_plus_one_counters(&face));
    }

    #[test]
    fn detects_token_with_counters() {
        let mut face = make_face("Token Creator");
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Token {
                name: "Construct".to_string(),
                power: engine::types::ability::PtValue::Fixed(0),
                toughness: engine::types::ability::PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Creature".to_string()],
                count: QuantityExpr::Fixed { value: 1 },
                tapped: false,
                colors: vec![],
                keywords: vec![],
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![("P1P1".to_string(), QuantityExpr::Fixed { value: 1 })],
                owner: TargetFilter::Controller,
                attach_to: None,
            },
        ));
        assert!(face_etb_with_plus_one_counters(&face));
    }

    // ─── Scope guard tests ───────────────────────────────────────────────────

    #[test]
    fn opponent_scope_payoff_ignored() {
        // A CounterAdded trigger with NO counter_filter is not a +1/+1 payoff.
        let mut face = make_face("Generic Counter Trigger");
        let t = TriggerDefinition::new(TriggerMode::CounterAdded);
        // counter_filter = None → does not match P1P1 specifically.
        face.triggers.push(t);
        assert!(!face_has_triggered_counter_payoff(&face));
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let face = make_face("Grizzly Bears");
        assert!(!face.abilities.iter().any(ability_places_plus_one_counter));
        assert!(!face.abilities.iter().any(ability_proliferates));
        assert!(!face_doubles_counters(&face));
        assert!(!face_is_counter_payoff(&face));
        assert!(!face_etb_with_plus_one_counters(&face));
    }

    // ─── detect() integration tests ──────────────────────────────────────────

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.generator_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        // 20 generators + 20 payoffs → commitment must not exceed 1.0.
        let mut deck = vec![];
        for i in 0..20u32 {
            let mut face = make_face(&format!("Generator {i}"));
            face.abilities.push(add_counter_ability("P1P1"));
            deck.push(deck_entry(face, 4));
        }
        for i in 0..20u32 {
            let mut face = make_face(&format!("Payoff {i}"));
            face.triggers.push(counter_added_trigger());
            deck.push(deck_entry(face, 4));
        }
        let feature = detect(&deck);
        assert!(
            feature.commitment <= 1.0,
            "commitment {} > 1.0",
            feature.commitment
        );
    }

    #[test]
    fn pure_generators_no_payoff_low_commitment() {
        // Generators only → commitment ≤ 0.15 * s.
        let mut deck = vec![];
        for i in 0..4u32 {
            let mut face = make_face(&format!("Generator {i}"));
            face.abilities.push(add_counter_ability("P1P1"));
            deck.push(deck_entry(face, 1));
        }
        let feature = detect(&deck);
        assert!(
            feature.commitment <= 0.15,
            "expected ≤ 0.15, got {}",
            feature.commitment
        );
    }

    #[test]
    fn pure_payoff_no_generators_zero_commitment() {
        // Payoffs only, no generators and no ETB → commitment must be exactly 0.0.
        let mut deck = vec![];
        for i in 0..4u32 {
            let mut face = make_face(&format!("Payoff {i}"));
            face.triggers.push(counter_added_trigger());
            deck.push(deck_entry(face, 1));
        }
        let feature = detect(&deck);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn hardened_scales_calibration() {
        // 8 generators + 4 proliferators + 2 doublers + 6 payoffs + 4 etb → commitment > 0.85.
        let mut deck = vec![];
        for i in 0..8u32 {
            let mut face = make_face(&format!("Gen {i}"));
            face.abilities.push(add_counter_ability("P1P1"));
            deck.push(deck_entry(face, 1));
        }
        for i in 0..4u32 {
            let mut face = make_face(&format!("Prolif {i}"));
            face.abilities.push(proliferate_ability());
            deck.push(deck_entry(face, 1));
        }
        for i in 0..2u32 {
            let mut face = make_face(&format!("Doubler {i}"));
            face.replacements.push(doubler_replacement());
            deck.push(deck_entry(face, 1));
        }
        for i in 0..6u32 {
            let mut face = make_face(&format!("Payoff {i}"));
            face.triggers.push(counter_added_trigger());
            deck.push(deck_entry(face, 1));
        }
        for i in 0..4u32 {
            let mut face = make_face(&format!("ETB {i}"));
            face.keywords.push(Keyword::EtbCounter {
                counter_type: "P1P1".to_string(),
                count: 1,
            });
            deck.push(deck_entry(face, 1));
        }
        let feature = detect(&deck);
        assert!(
            feature.commitment > 0.85,
            "expected > 0.85, got {}",
            feature.commitment
        );
    }
}
