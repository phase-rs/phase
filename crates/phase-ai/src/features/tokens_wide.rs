//! Tokens-wide feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `Effect::Token { types: Vec<String>, count: QuantityExpr, .. }` at
//!   `crates/engine/src/types/ability.rs:2131-2168`. CR 111.1: tokens created
//!   by spells/abilities. Creature tokens have `types.iter().any(|s| s == "Creature")`.
//! - `QuantityExpr::Fixed { value }` at `ability.rs:1360`. Mass generators
//!   have `value >= 2` or a non-Fixed count (dynamic quantity). CR 111.1.
//! - `TriggerDefinition { mode, execute, valid_card, .. }` at `ability.rs:4517`.
//!   `TriggerMode::TokenCreated` / `TriggerMode::TokenCreatedOnce` at
//!   `triggers.rs:478-479` are wide-payoff triggers. CR 111.1.
//! - `TriggerMode::Attacks` / `TriggerMode::AttackersDeclared` /
//!   `TriggerMode::YouAttack` at `triggers.rs:347-351,490` — attack-payoff
//!   trigger shapes. CR 508.3a (Attacks), CR 508.3d (YouAttack).
//! - `StaticDefinition { mode: StaticMode::Continuous, affected, modifications, .. }`
//!   at `ability.rs:4678-4694`. Anthem shape: affected references creatures
//!   you control, modifications include `AddPower`/`AddToughness`. CR 604.3 + CR 613.4c.
//! - `ContinuousModification::AddPower { value }` / `AddToughness { value }` at
//!   `ability.rs:5011-5016`. CR 613.4c: layer 7c power/toughness modification.
//! - `Effect::PumpAll { power, toughness, target }` at `ability.rs:2244-2251`.
//!   Mass combat pump. CR 613.4c.
//! - `TriggerDefinition.execute: Option<Box<AbilityDefinition>>` at `ability.rs:4520`.
//!   Token generators in trigger.execute chains are detected via
//!   `collect_chain_effects`. CR 603.6a: enters-the-battlefield trigger execution.
//!
//! **Boundary with `aristocrats`**: `token_generator_count` CAN overlap with
//! `aristocrats::fodder_source_count` — a card that generates creature tokens
//! (e.g., Bitterblossom) will increment both features. This is intentional.
//! The two features consume different axes of token generation: aristocrats
//! cares about *sacrifice-outlet + fodder supply*, tokens_wide cares about
//! *mass-creation + anthem amplification*. Policies from both features can
//! fire on the same deck.
//!
//! No parser remediation required — tokens-wide abilities classify structurally
//! using the existing typed AST.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, ContinuousModification, Effect, QuantityExpr, StaticDefinition,
    TargetFilter, TriggerDefinition,
};
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;

use crate::ability_chain::collect_chain_effects;
use crate::features::aristocrats::{
    filter_references_creature_you_control_or_any, typed_filter_is_creature_you_control_or_any,
};

/// Commitment floor below which the TokensWidePolicy opts out.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// Commitment floor for the mulligan policy.
pub const MULLIGAN_FLOOR: f32 = 0.40;

/// Minimum creatures on the board for an anthem to be "timely". CR 613.4c.
pub const ANTHEM_TIMELY_BOARD_FLOOR: u32 = 3;

/// Minimum attackers in a DeclareAttackers candidate for "swing wide" bonus.
/// CR 508.3d.
pub const WIDE_ATTACK_FLOOR: u32 = 3;

/// CR 111.1 + CR 613.4c + CR 508.3a: per-deck tokens-wide classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities`, `CardFace.triggers`, and `CardFace.static_abilities` —
/// never by card name. Policies consume this to weight token-generator casts,
/// anthem deployment timing, and mulligan hand evaluation.
#[derive(Debug, Clone, Default)]
pub struct TokensWideFeature {
    /// Cards with a creature-token-generating ability or trigger.execute chain.
    /// CR 111.1: token creation by spells/abilities.
    pub token_generator_count: u32,
    /// Subset of token generators that produce ≥2 tokens (fixed count ≥ 2 or
    /// a dynamic/non-Fixed count expression). CR 111.1.
    pub mass_token_generator_count: u32,
    /// Cards with a continuous "creatures you control get +X/+Y" static ability.
    /// CR 604.3 + CR 613.4c.
    pub anthem_count: u32,
    /// Cards with a `PumpAll` effect targeting creatures you control or wildcard.
    /// CR 613.4c.
    pub mass_pump_count: u32,
    /// Cards with a `TokenCreated`/`TokenCreatedOnce` trigger, or an attack
    /// trigger (`Attacks`/`AttackersDeclared`/`YouAttack`) scoped to you or wildcard.
    /// CR 111.1 + CR 508.3a + CR 508.3d.
    pub wide_payoff_count: u32,
    /// Weighted commitment score `0.0..=1.0`.
    pub commitment: f32,
    /// Card names of token generators — used for identity lookup in policies
    /// (not for classification).
    pub payoff_names: Vec<String>,
    /// Card names of anthem/pump cards — used for identity lookup in policies.
    pub anthem_names: Vec<String>,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across the tokens-wide axes.
pub fn detect(deck: &[DeckEntry]) -> TokensWideFeature {
    if deck.is_empty() {
        return TokensWideFeature::default();
    }

    let mut token_generator_count = 0u32;
    let mut mass_token_generator_count = 0u32;
    let mut anthem_count = 0u32;
    let mut mass_pump_count = 0u32;
    let mut wide_payoff_count = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();
    let mut anthem_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;

        let is_gen = is_token_generator_parts(&face.abilities, &face.triggers);
        let is_mass = is_gen && is_mass_token_generator_parts(&face.abilities, &face.triggers);
        let is_anthem = is_anthem_parts(&face.static_abilities);
        let is_pump = is_mass_pump_parts(&face.abilities);
        let is_payoff = is_wide_payoff_parts(&face.triggers);

        // Counts scale with playset size; identity-lookup name lists do not.
        // Push each unique face name once — mulligan policy iterates the lists,
        // so dedup keeps lookups O(unique_faces) rather than O(decklist).
        if is_gen {
            token_generator_count = token_generator_count.saturating_add(entry.count);
            payoff_names.push(face.name.clone());
        }
        if is_mass {
            mass_token_generator_count = mass_token_generator_count.saturating_add(entry.count);
        }
        if is_anthem {
            anthem_count = anthem_count.saturating_add(entry.count);
        }
        if is_pump {
            mass_pump_count = mass_pump_count.saturating_add(entry.count);
        }
        // Single push per face — anthem-and-pump combo cards (Glorious Anthem
        // shape with an Overrun activation) appear once in `anthem_names`.
        if is_anthem || is_pump {
            anthem_names.push(face.name.clone());
        }
        if is_payoff {
            wide_payoff_count = wide_payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = compute_commitment(
        token_generator_count,
        anthem_count,
        mass_pump_count,
        wide_payoff_count,
        mass_token_generator_count,
    );

    TokensWideFeature {
        token_generator_count,
        mass_token_generator_count,
        anthem_count,
        mass_pump_count,
        wide_payoff_count,
        commitment,
        payoff_names,
        anthem_names,
    }
}

/// Geometric-mean commitment with mass-generator bonus.
///
/// Calibration:
/// - Modern Squirrels (12 generators + 7 anthems/pump + 4 payoffs) → ≈ 1.0
/// - Mono-Red Burn (0 generators) → ≤ 0.15
fn compute_commitment(
    token_generator_count: u32,
    anthem_count: u32,
    mass_pump_count: u32,
    wide_payoff_count: u32,
    mass_token_generator_count: u32,
) -> f32 {
    let g = (token_generator_count as f32 / 6.0).min(1.0);
    let a = ((anthem_count + mass_pump_count) as f32 / 4.0).min(1.0);
    let p = (wide_payoff_count as f32 / 3.0).min(1.0);
    let mass_bonus = (0.04 * mass_token_generator_count as f32).min(0.15);

    // mass_token_generator_count is a strict subset of token_generator_count,
    // so when generators are absent mass_bonus is necessarily zero — the early
    // return collapses to 0.0. Keeping the branch explicit makes the
    // commitment-collapse semantics unambiguous.
    if token_generator_count == 0 || (anthem_count + mass_pump_count) == 0 {
        0.0
    } else {
        ((g * a * p).powf(1.0 / 3.0) + mass_bonus).min(1.0)
    }
}

// ─── Parts predicates ────────────────────────────────────────────────────────

/// True if this card has any creature-token-generating ability or
/// trigger.execute chain. CR 111.1: token creation by spells/abilities.
///
/// Walks `face.abilities` and `face.triggers[*].execute` via
/// `collect_chain_effects`. Treasure/Food/Clue tokens do NOT count — their
/// `types` field lacks "Creature".
pub(crate) fn is_token_generator_parts(
    abilities: &[AbilityDefinition],
    triggers: &[TriggerDefinition],
) -> bool {
    // Check direct abilities.
    if abilities.iter().any(|a| {
        collect_chain_effects(a)
            .iter()
            .any(effect_is_creature_token)
    }) {
        return true;
    }
    // Check trigger.execute chains — e.g., "at the beginning of your upkeep,
    // create a 1/1 Faerie token". CR 603.6a.
    triggers.iter().any(|t| {
        t.execute.as_ref().is_some_and(|exec| {
            collect_chain_effects(exec)
                .iter()
                .any(effect_is_creature_token)
        })
    })
}

/// True if this card is a token generator AND produces ≥2 tokens
/// (Fixed count ≥ 2 or any non-Fixed QuantityExpr). CR 111.1.
pub(crate) fn is_mass_token_generator_parts(
    abilities: &[AbilityDefinition],
    triggers: &[TriggerDefinition],
) -> bool {
    // Check direct abilities.
    if abilities
        .iter()
        .any(|a| collect_chain_effects(a).iter().any(effect_is_mass_token))
    {
        return true;
    }
    // Check trigger.execute chains.
    triggers.iter().any(|t| {
        t.execute
            .as_ref()
            .is_some_and(|exec| collect_chain_effects(exec).iter().any(effect_is_mass_token))
    })
}

/// True if this card has a continuous "creatures you control get +X/+Y" static.
/// The affected filter must reference creatures you control (or be unscoped),
/// and the modifications must include `AddPower`/`AddToughness` with value > 0.
/// CR 604.3: static abilities. CR 613.4c: layer 7c power/toughness modifications.
pub(crate) fn is_anthem_parts(statics: &[StaticDefinition]) -> bool {
    statics.iter().any(static_is_anthem)
}

/// True if this card has a `PumpAll` effect targeting creatures you control or
/// any creature (wildcard). CR 613.4c: layer 7c P/T modifications.
pub(crate) fn is_mass_pump_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|a| {
        collect_chain_effects(a)
            .iter()
            .any(effect_is_creature_pumpall_you)
    })
}

/// True if this card has a wide-payoff trigger.
///
/// Qualifying shapes:
/// - `TokenCreated` / `TokenCreatedOnce` — always a payoff. CR 111.1.
/// - `Attacks` / `AttackersDeclared` / `YouAttack` — attack payoffs scoped to
///   you/wildcard (not opponent-scoped). CR 508.3a + CR 508.3d.
pub(crate) fn is_wide_payoff_parts(triggers: &[TriggerDefinition]) -> bool {
    triggers.iter().any(trigger_is_wide_payoff)
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// True if the effect creates a creature token. CR 111.1.
fn effect_is_creature_token(e: &&Effect) -> bool {
    matches!(e, Effect::Token { types, .. } if types.iter().any(|s| s == "Creature"))
}

/// True if the effect creates ≥2 creature tokens (mass generator). CR 111.1.
fn effect_is_mass_token(e: &&Effect) -> bool {
    match e {
        Effect::Token { types, count, .. } => {
            if !types.iter().any(|s| s == "Creature") {
                return false;
            }
            match count {
                // Fixed count ≥ 2 is unambiguously mass.
                QuantityExpr::Fixed { value } => *value >= 2,
                // Non-Fixed (Ref, HalfRounded, Offset, Multiply) — dynamic
                // quantity, treat as mass since it can exceed 1. CR 111.1.
                _ => true,
            }
        }
        _ => false,
    }
}

/// True if the static definition is an anthem: Continuous mode, affected
/// references creatures you control or any creature, and modifications include
/// AddPower or AddToughness with value > 0. CR 604.3 + CR 613.4c.
fn static_is_anthem(s: &StaticDefinition) -> bool {
    if s.mode != StaticMode::Continuous {
        return false;
    }
    // Affected must reference creatures you control, or be unscoped (wildcard).
    let filter_ok = match &s.affected {
        None => true, // wildcard scope — rare but valid
        Some(filter) => filter_references_creature_you_control_or_any(filter),
    };
    if !filter_ok {
        return false;
    }
    // Must boost power or toughness. CR 613.4c.
    s.modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddPower { value } | ContinuousModification::AddToughness { value }
            if *value > 0
        )
    })
}

/// True if the effect is a `PumpAll` targeting creatures you control or wildcard.
/// CR 613.4c: layer 7c mass P/T modification.
fn effect_is_creature_pumpall_you(e: &&Effect) -> bool {
    match e {
        Effect::PumpAll { target, .. } => match target {
            TargetFilter::None => true, // wildcard — pumps everything
            filter => filter_or_typed_is_creature_you(filter),
        },
        _ => false,
    }
}

/// True if a trigger is a wide-payoff trigger shape.
fn trigger_is_wide_payoff(t: &TriggerDefinition) -> bool {
    match t.mode {
        // Token-creation triggers always count — they scale with token output.
        // CR 111.1.
        TriggerMode::TokenCreated | TriggerMode::TokenCreatedOnce => true,
        // Attack triggers count when scoped to you/wildcard (not opponent).
        // CR 508.3a (creature attacks), CR 508.3d (you attack).
        TriggerMode::Attacks | TriggerMode::AttackersDeclared | TriggerMode::YouAttack => {
            // valid_card scoped to opponent means "when an opponent's creature
            // attacks" — does not benefit the tokens-wide plan.
            match &t.valid_card {
                None => true, // wildcard
                Some(filter) => filter_references_creature_you_control_or_any(filter),
            }
        }
        _ => false,
    }
}

/// Convenience wrapper: true if a TargetFilter references creatures you control
/// or any creature (no opponent-scoping). Delegates to the shared
/// `typed_filter_is_creature_you_control_or_any` for TypedFilter branches.
fn filter_or_typed_is_creature_you(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_creature_you_control_or_any(typed),
        TargetFilter::Or { filters } => filters.iter().any(filter_or_typed_is_creature_you),
        TargetFilter::And { filters } => filters.iter().all(filter_or_typed_is_creature_you),
        _ => false,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
        QuantityExpr, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::triggers::TriggerMode;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn empty_face(name: &str) -> CardFace {
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

    fn creature_token_effect(count: QuantityExpr) -> Effect {
        Effect::Token {
            name: "Saproling".to_string(),
            power: engine::types::ability::PtValue::Fixed(1),
            toughness: engine::types::ability::PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count,
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }

    fn treasure_token_effect() -> Effect {
        Effect::Token {
            name: "Treasure".to_string(),
            power: engine::types::ability::PtValue::Fixed(0),
            toughness: engine::types::ability::PtValue::Fixed(0),
            types: vec!["Artifact".to_string()], // No "Creature"
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }

    fn spell_ability_with(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    fn creature_you_control_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            type_filters: vec![TypeFilter::Creature],
            ..TypedFilter::default()
        })
    }

    fn anthem_static(target: TargetFilter) -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(target)
            .modifications(vec![ContinuousModification::AddPower { value: 1 }])
    }

    fn deck_entry(face: CardFace) -> DeckEntry {
        DeckEntry {
            card: face,
            count: 1,
        }
    }

    // ── Feature-detection tests ────────────────────────────────────────────

    #[test]
    fn detect_creature_token_generator_via_spell() {
        let mut face = empty_face("Saproling Swarm");
        face.abilities
            .push(spell_ability_with(creature_token_effect(
                QuantityExpr::Fixed { value: 1 },
            )));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.token_generator_count, 1);
        assert!(!feature.payoff_names.is_empty());
    }

    #[test]
    fn detect_mass_token_generator() {
        let mut face = empty_face("Raise the Alarm");
        face.abilities
            .push(spell_ability_with(creature_token_effect(
                QuantityExpr::Fixed { value: 2 },
            )));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.token_generator_count, 1);
        assert_eq!(feature.mass_token_generator_count, 1);
    }

    #[test]
    fn treasure_token_does_not_count() {
        let mut face = empty_face("Treasure Mage");
        face.abilities
            .push(spell_ability_with(treasure_token_effect()));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.token_generator_count, 0);
        assert_eq!(feature.mass_token_generator_count, 0);
    }

    #[test]
    fn detect_token_generator_in_trigger_execute() {
        let mut face = empty_face("Bitterblossom");
        // Trigger: at the beginning of your upkeep, create a 1/1 Faerie.
        let exec = AbilityDefinition::new(
            AbilityKind::Spell,
            creature_token_effect(QuantityExpr::Fixed { value: 1 }),
        );
        // Use Phase (at the beginning of your upkeep) as a representative mode.
        // CR 603.2b: "at the beginning of [phase/step]" triggers at phase start.
        let mut trigger = TriggerDefinition::new(TriggerMode::Phase);
        trigger.execute = Some(Box::new(exec));
        face.triggers.push(trigger);
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.token_generator_count, 1);
    }

    #[test]
    fn detect_anthem_static() {
        let mut face = empty_face("Glorious Anthem");
        face.static_abilities
            .push(anthem_static(creature_you_control_filter()));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.anthem_count, 1);
        assert!(!feature.anthem_names.is_empty());
    }

    #[test]
    fn anthem_must_scope_to_you_not_opponent() {
        let mut face = empty_face("Opponents Anthem");
        let opponent_filter = TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            type_filters: vec![TypeFilter::Creature],
            ..TypedFilter::default()
        });
        face.static_abilities.push(anthem_static(opponent_filter));
        let feature = detect(&[deck_entry(face)]);
        // Opponent-scoped anthem must NOT count — it doesn't benefit AI tokens.
        assert_eq!(feature.anthem_count, 0);
    }

    #[test]
    fn pump_all_increments_mass_pump() {
        let mut face = empty_face("Overrun");
        face.abilities.push(spell_ability_with(Effect::PumpAll {
            power: engine::types::ability::PtValue::Fixed(3),
            toughness: engine::types::ability::PtValue::Fixed(3),
            target: creature_you_control_filter(),
        }));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.mass_pump_count, 1);
    }

    #[test]
    fn single_target_pump_does_not_count() {
        let mut face = empty_face("Giant Growth");
        face.abilities.push(spell_ability_with(Effect::Pump {
            power: engine::types::ability::PtValue::Fixed(3),
            toughness: engine::types::ability::PtValue::Fixed(3),
            target: TargetFilter::Any,
        }));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.mass_pump_count, 0);
    }

    #[test]
    fn attacks_trigger_payoff() {
        let mut face = empty_face("Reconnaissance Mission");
        let mut trigger = TriggerDefinition::new(TriggerMode::Attacks);
        trigger.valid_card = Some(creature_you_control_filter());
        face.triggers.push(trigger);
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.wide_payoff_count, 1);
    }

    #[test]
    fn token_created_trigger_payoff() {
        let mut face = empty_face("Anointed Procession");
        face.triggers
            .push(TriggerDefinition::new(TriggerMode::TokenCreated));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.wide_payoff_count, 1);
    }

    #[test]
    fn commitment_collapse_no_generators() {
        // No token generators → commitment is just the mass_bonus floor.
        let mut face = empty_face("Plains");
        face.card_type.core_types = vec![CoreType::Land];
        let feature = detect(&[deck_entry(face)]);
        assert!(
            feature.commitment <= 0.15,
            "no generators → commitment ≤ 0.15, got {}",
            feature.commitment
        );
    }

    #[test]
    fn commitment_collapse_no_anthems_no_pump() {
        // Generator only (no anthem/pump) → commitment ≤ mass_bonus only.
        let mut face = empty_face("Pest Infestation");
        face.abilities
            .push(spell_ability_with(creature_token_effect(
                QuantityExpr::Fixed { value: 2 },
            )));
        let feature = detect(&[deck_entry(face)]);
        // anthem_count = 0, mass_pump_count = 0 → geometric mean collapses.
        assert!(
            feature.commitment <= 0.15,
            "no anthems → commitment ≤ 0.15, got {}",
            feature.commitment
        );
    }

    #[test]
    fn commitment_clamps_to_one() {
        // 12 generators + 7 anthems + 4 payoffs → near 1.0 (or exactly 1.0).
        let gen_face = |name: &str| {
            let mut f = empty_face(name);
            f.abilities.push(spell_ability_with(creature_token_effect(
                QuantityExpr::Fixed { value: 3 },
            )));
            f.static_abilities
                .push(anthem_static(creature_you_control_filter()));
            f.triggers
                .push(TriggerDefinition::new(TriggerMode::TokenCreated));
            DeckEntry { card: f, count: 4 }
        };
        let deck: Vec<DeckEntry> = (0..4).map(|i| gen_face(&format!("Maker {i}"))).collect();
        let feature = detect(&deck);
        assert!(
            feature.commitment <= 1.0,
            "commitment must clamp to ≤ 1.0, got {}",
            feature.commitment
        );
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let feature = detect(&[deck_entry(empty_face("Grizzly Bears"))]);
        assert_eq!(feature.token_generator_count, 0);
        assert_eq!(feature.anthem_count, 0);
        assert_eq!(feature.mass_pump_count, 0);
        assert_eq!(feature.wide_payoff_count, 0);
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.token_generator_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn payoff_names_populated() {
        let mut face = empty_face("Llanowar Elves Clone");
        face.abilities
            .push(spell_ability_with(creature_token_effect(
                QuantityExpr::Fixed { value: 1 },
            )));
        let feature = detect(&[deck_entry(face)]);
        assert!(
            feature
                .payoff_names
                .contains(&"Llanowar Elves Clone".to_string()),
            "payoff_names should include the generator's name"
        );
    }

    #[test]
    fn anthem_names_populated() {
        let mut face = empty_face("Glorious Anthem");
        face.static_abilities
            .push(anthem_static(creature_you_control_filter()));
        let feature = detect(&[deck_entry(face)]);
        assert!(
            feature
                .anthem_names
                .contains(&"Glorious Anthem".to_string()),
            "anthem_names should include the anthem's name"
        );
    }

    #[test]
    fn aristocrats_overlap_independent() {
        // A Bitterblossom-shape card (upkeep trigger creating a 1/1 faerie)
        // should count as BOTH a token generator (tokens_wide) and a fodder
        // source (aristocrats). The two features are independent. This test
        // verifies tokens_wide registers it; aristocrats has its own test.
        let mut face = empty_face("Bitterblossom");
        let exec = AbilityDefinition::new(
            AbilityKind::Spell,
            creature_token_effect(QuantityExpr::Fixed { value: 1 }),
        );
        // CR 603.2b: "at the beginning of your upkeep" → TriggerMode::Phase.
        let mut trigger = TriggerDefinition::new(TriggerMode::Phase);
        trigger.execute = Some(Box::new(exec));
        face.triggers.push(trigger);
        let feature = detect(&[deck_entry(face)]);
        // tokens_wide registers Bitterblossom as a generator.
        assert_eq!(feature.token_generator_count, 1);
    }

    #[test]
    fn non_fixed_count_is_mass() {
        // A dynamic quantity (Ref) counts as mass even though we can't
        // statically bound it — the design intent is "could produce many".
        let mut face = empty_face("Pest Infestation");
        let dynamic_count = QuantityExpr::Ref {
            qty: engine::types::ability::QuantityRef::HandSize {
                player: engine::types::ability::PlayerScope::Controller,
            },
        };
        face.abilities
            .push(spell_ability_with(creature_token_effect(dynamic_count)));
        let feature = detect(&[deck_entry(face)]);
        assert_eq!(feature.mass_token_generator_count, 1);
    }
}
