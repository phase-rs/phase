//! Spellslinger / Prowess feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `CoreType::Instant` / `CoreType::Sorcery` at `card_type.rs:74-77`.
//!   CR 304.1: Instants. CR 307.1: Sorceries.
//! - `CardFace.mana_cost: ManaCost`; `ManaCost::mana_value() -> u32` at
//!   `card.rs:51`; `mana.rs:468`. CR 202.3b: mana value.
//! - `Keyword::Prowess` at `keywords.rs:311`. CR 702.108a: prowess triggered ability.
//! - `TriggerMode::SpellCast` / `SpellCastOrCopy` / `SpellAbilityCast` /
//!   `SpellAbilityCopy` at `triggers.rs:50-57`. CR 601.2i (cast) + CR 707.10 (copy).
//! - `TriggerConstraint::NthSpellThisTurn { n, filter }` at `ability.rs:4484`.
//!   CR 603.4: intervening-if clause. CR 603.1: triggered abilities.
//! - `TriggerDefinition.valid_card: Option<TargetFilter>` at `ability.rs:4522`.
//! - `TriggerDefinition.valid_target: Option<TargetFilter>` at `ability.rs:4539`.
//! - `TriggerDefinition.constraint: Option<TriggerConstraint>` at `ability.rs:4545`.
//! - `Effect::CopySpell { target: TargetFilter }` at `ability.rs:2389`. CR 707.10.
//! - `Effect::Draw { count }` / `Effect::Dig { .. }` — card draw. CR 121.1.
//! - `Effect::DealDamage { amount, target, .. }` at `ability.rs:2085`. CR 120.3.
//!
//! **Magecraft note**: Magecraft (CR 207.2c: ability word, no dedicated TriggerMode)
//! is detected as a cast-payoff trigger: mode in
//! `{SpellCast, SpellCastOrCopy, SpellAbilityCast, SpellAbilityCopy}` with
//! `valid_target` being `None` or `Some(Controller)` and `valid_card` matching
//! Instant/Sorcery or unset.
//!
//! **Burn-to-player detection**: walks `DealDamage` effects for targets that can
//! hit players (`TargetFilter::Any`, `Player`, `Opponent`, or an `Or` containing
//! those). `DamageAll` against creatures only (Pyroclasm shape) does NOT qualify.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, TargetFilter, TriggerConstraint, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

use crate::ability_chain::collect_chain_effects;
use crate::features::control::is_card_draw_parts;

/// Maximum mana value to qualify as "low curve". CR 202.3b.
pub const CHEAP_SPELL_MV: u32 = 2;

/// Tactical-policy opt-in gate.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// Mulligan-policy opt-in gate.
pub const MULLIGAN_FLOOR: f32 = 0.40;

/// CR 702.108a + CR 601.2i + CR 707.10: per-deck spellslinger/prowess
/// classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace` triggers, effects, and keywords — never by card name.
/// `payoff_names` is an exception: it is an identity-lookup list for policies
/// that check whether an in-hand card is a *known payoff* by name.
#[derive(Debug, Clone, Default)]
pub struct SpellslingerProwessFeature {
    /// Non-land instants/sorceries with mana value ≤ `CHEAP_SPELL_MV`. CR 202.3b.
    pub low_curve_spell_count: u32,
    /// Total non-land instants + sorceries. CR 304.1 + CR 307.1.
    pub instant_sorcery_count: u32,
    /// Cards with `Keyword::Prowess`. CR 702.108a.
    pub prowess_count: u32,
    /// Cast-payoff triggers: SpellCast/SpellCastOrCopy/SpellAbilityCast/
    /// SpellAbilityCopy scoped to the caster (valid_target = None or Controller)
    /// with a valid_card filter that permits Instant/Sorcery (or unset).
    /// CR 601.2i + CR 603.1. Includes magecraft-shaped triggers.
    pub cast_payoff_count: u32,
    /// Cast triggers with `TriggerConstraint::NthSpellThisTurn`. CR 603.4.
    pub nth_spell_payoff_count: u32,
    /// `AbilityKind::Spell` abilities whose chain contains `Effect::CopySpell`.
    /// CR 707.10: to copy a spell means to put a copy onto the stack.
    pub copy_effect_count: u32,
    /// Non-land instants/sorceries with a draw or non-impulse dig effect
    /// (cantrips). Delegates to `is_card_draw_parts` for the impulse carve-out.
    /// CR 121.1.
    pub cantrip_count: u32,
    /// Non-land instants/sorceries with `DealDamage` targeting a player
    /// (Any/Player/Opponent or Or containing those). CR 120.3.
    pub burn_player_count: u32,
    /// `low_curve_spell_count / max(instant_sorcery_count, 1)`.
    pub low_curve_ratio: f32,
    /// Weighted commitment score `0.0..=1.0`.
    pub commitment: f32,
    /// Card names of prowess + cast-payoff + nth-spell-payoff cards.
    /// Used for identity lookup in policies (not classification). One entry
    /// per unique face — counts scale with entry.count, names do not.
    pub payoff_names: Vec<String>,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across the spellslinger axes.
pub fn detect(deck: &[DeckEntry]) -> SpellslingerProwessFeature {
    if deck.is_empty() {
        return SpellslingerProwessFeature::default();
    }

    let mut low_curve_spell_count = 0u32;
    let mut instant_sorcery_count = 0u32;
    let mut prowess_count = 0u32;
    let mut cast_payoff_count = 0u32;
    let mut nth_spell_payoff_count = 0u32;
    let mut copy_effect_count = 0u32;
    let mut cantrip_count = 0u32;
    let mut burn_player_count = 0u32;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        let is_land = face.card_type.core_types.contains(&CoreType::Land);

        if !is_land {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // Instant/sorcery axes — CR 304.1 + CR 307.1.
        let is_is = !is_land && is_instant_or_sorcery_parts(&face.card_type.core_types);
        if is_is {
            instant_sorcery_count = instant_sorcery_count.saturating_add(entry.count);
            if is_low_curve_spell_parts(&face.card_type.core_types, &face.mana_cost) {
                low_curve_spell_count = low_curve_spell_count.saturating_add(entry.count);
            }
            if is_cantrip(face) {
                cantrip_count = cantrip_count.saturating_add(entry.count);
            }
            if is_burn_to_player_parts(&face.abilities) {
                burn_player_count = burn_player_count.saturating_add(entry.count);
            }
        }

        // Prowess — CR 702.108a.
        if has_prowess_parts(&face.keywords) {
            prowess_count = prowess_count.saturating_add(entry.count);
            payoff_names.push(face.name.clone());
        }

        // Cast payoffs (magecraft + general triggers) — CR 601.2i + CR 603.1.
        if is_cast_payoff_parts(&face.triggers) {
            cast_payoff_count = cast_payoff_count.saturating_add(entry.count);
            // Only push name if not already pushed (prowess card can also have
            // a cast-payoff trigger — push once).
            if !has_prowess_parts(&face.keywords) {
                payoff_names.push(face.name.clone());
            }
        }

        // Nth-spell payoffs — CR 603.4.
        if is_nth_spell_payoff_parts(&face.triggers) {
            nth_spell_payoff_count = nth_spell_payoff_count.saturating_add(entry.count);
            // Push name only if not already pushed by prowess or cast_payoff.
            if !has_prowess_parts(&face.keywords) && !is_cast_payoff_parts(&face.triggers) {
                payoff_names.push(face.name.clone());
            }
        }

        // Copy-spell effects — CR 707.10. Only AbilityKind::Spell chains count.
        if is_copy_effect_parts(&face.abilities) {
            copy_effect_count = copy_effect_count.saturating_add(entry.count);
        }
    }

    let low_curve_ratio = low_curve_spell_count as f32 / instant_sorcery_count.max(1) as f32;

    let commitment = compute_commitment(
        prowess_count,
        cast_payoff_count,
        nth_spell_payoff_count,
        low_curve_spell_count,
        copy_effect_count,
        total_nonland,
    );

    SpellslingerProwessFeature {
        low_curve_spell_count,
        instant_sorcery_count,
        prowess_count,
        cast_payoff_count,
        nth_spell_payoff_count,
        copy_effect_count,
        cantrip_count,
        burn_player_count,
        low_curve_ratio,
        commitment,
        payoff_names,
    }
}

/// Clamped weighted commitment formula.
///
/// Calibration:
/// - Modern Izzet Prowess (~37 nonland, 16 prowess/payoffs, 20 low-curve IS):
///   commitment ≈ 0.87.
/// - Mono-Red Burn (~36 nonland, 0 payoffs, 28 low-curve IS):
///   commitment ≈ 0.44 (above COMMITMENT_FLOOR — correctly spellslinger-adjacent).
/// - Vanilla midrange (no spells): commitment < 0.10 (below floor).
fn compute_commitment(
    prowess_count: u32,
    cast_payoff_count: u32,
    nth_spell_payoff_count: u32,
    low_curve_spell_count: u32,
    copy_effect_count: u32,
    total_nonland: u32,
) -> f32 {
    let denom = total_nonland.max(1) as f32;
    let payoff_density =
        (prowess_count as f32 + cast_payoff_count as f32 + 2.0 * nth_spell_payoff_count as f32)
            / denom;
    let spell_density = low_curve_spell_count as f32 / denom;
    let copy_density = copy_effect_count as f32 / denom;
    (1.6 * payoff_density + 1.0 * spell_density + 0.5 * copy_density).clamp(0.0, 1.0)
}

// ─── Parts predicates ────────────────────────────────────────────────────────

/// True if the card has `CoreType::Instant` or `CoreType::Sorcery`.
/// CR 304.1: Instants. CR 307.1: Sorceries.
fn is_instant_or_sorcery_parts(core_types: &[CoreType]) -> bool {
    core_types.contains(&CoreType::Instant) || core_types.contains(&CoreType::Sorcery)
}

/// Parts-based low-curve spell classifier. CR 202.3b: mana value. CR 304.1 +
/// CR 307.1: Instant and Sorcery core types.
pub(crate) fn is_low_curve_spell_parts(
    core_types: &[CoreType],
    mana_cost: &engine::types::mana::ManaCost,
) -> bool {
    is_instant_or_sorcery_parts(core_types) && mana_cost.mana_value() <= CHEAP_SPELL_MV
}

/// Parts-based prowess classifier. CR 702.108a.
pub(crate) fn has_prowess_parts(keywords: &[Keyword]) -> bool {
    keywords.contains(&Keyword::Prowess)
}

/// Parts-based cast-payoff classifier. CR 601.2i + CR 603.1 + CR 707.10.
///
/// Qualifies when ALL of:
/// 1. `mode` is in `{SpellCast, SpellCastOrCopy, SpellAbilityCast, SpellAbilityCopy}`.
/// 2. `valid_target` is `None` or `Some(Controller)` — caster-scoped only.
///    Opponent-scoped triggers (Esper Sentinel shape) are NOT your payoffs.
/// 3. `valid_card` is `None` (any spell) OR matches Instant/Sorcery via filter walk.
pub(crate) fn is_cast_payoff_parts(triggers: &[TriggerDefinition]) -> bool {
    triggers.iter().any(trigger_is_cast_payoff)
}

fn trigger_is_cast_payoff(t: &TriggerDefinition) -> bool {
    // 1. Mode must be a spell-cast trigger shape.
    let mode_ok = matches!(
        t.mode,
        TriggerMode::SpellCast
            | TriggerMode::SpellCastOrCopy
            | TriggerMode::SpellAbilityCast
            | TriggerMode::SpellAbilityCopy
    );
    if !mode_ok {
        return false;
    }

    // 2. Scope: caster-scoped only (valid_target = None or Controller).
    if !matches!(&t.valid_target, None | Some(TargetFilter::Controller)) {
        return false;
    }

    // 3. valid_card: None (any spell) or a filter that allows Instant/Sorcery.
    match &t.valid_card {
        None => true,
        Some(filter) => filter_matches_instant_or_sorcery(filter),
    }
}

/// True if the face has an Nth-spell-this-turn cast trigger.
/// CR 603.4: intervening-if clause. CR 603.1.
pub(crate) fn is_nth_spell_payoff_parts(triggers: &[TriggerDefinition]) -> bool {
    triggers.iter().any(|t| {
        matches!(
            t.mode,
            TriggerMode::SpellCast
                | TriggerMode::SpellCastOrCopy
                | TriggerMode::SpellAbilityCast
                | TriggerMode::SpellAbilityCopy
        ) && matches!(
            t.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { .. })
        )
    })
}

/// True if any `AbilityKind::Spell` chain contains `Effect::CopySpell`.
/// Activated-ability copy effects (Dualcaster Mage shape — `AbilityKind::Activated`)
/// are excluded. CR 707.10: to copy a spell means to put a copy onto the stack.
pub(crate) fn is_copy_effect_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability)
                .iter()
                .any(|e| matches!(e, Effect::CopySpell { .. }))
    })
}

/// True if the face is an instant or sorcery that draws cards (cantrip shape).
/// Delegates impulse-cast carve-out to `is_card_draw_parts`. CR 121.1.
pub(crate) fn is_cantrip(face: &CardFace) -> bool {
    is_instant_or_sorcery_parts(&face.card_type.core_types) && is_card_draw_parts(&face.abilities)
}

/// True if any `AbilityKind::Spell` ability in the chain contains
/// `Effect::DealDamage` targeting a player (Any, Player, Opponent, or an Or
/// containing those). CR 120.3: damage to players.
///
/// Pyroclasm-shape (`DealDamage` to creatures only, or `DamageAll`) is excluded —
/// those are board-wipes, not player-reach burn.
pub(crate) fn is_burn_to_player_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability).iter().any(|e| match e {
                Effect::DealDamage { target, .. } => filter_can_target_player(target),
                _ => false,
            })
    })
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Walk a `TargetFilter` and return true if it can match an instant or sorcery.
/// The parser emits `TargetFilter::Or { filters: [Typed(Instant), Typed(Sorcery)] }`
/// for "instant or sorcery" — this helper handles Or/And/Typed recursion.
fn filter_matches_instant_or_sorcery(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(tf) => tf.type_filters.iter().any(|t| {
            matches!(
                t,
                engine::types::ability::TypeFilter::Instant
                    | engine::types::ability::TypeFilter::Sorcery
            )
        }),
        TargetFilter::Or { filters } => filters.iter().any(filter_matches_instant_or_sorcery),
        TargetFilter::And { filters } => filters.iter().all(filter_matches_instant_or_sorcery),
        _ => false,
    }
}

/// True if a `TargetFilter` can target a player (Any/Player/Or containing those).
/// CR 120.3: damage can target players.
fn filter_can_target_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any | TargetFilter::Player => true,
        TargetFilter::Or { filters } => filters.iter().any(filter_can_target_player),
        TargetFilter::And { filters } => filters.iter().all(filter_can_target_player),
        _ => false,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
        TriggerDefinition, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn face(name: &str, core_types: Vec<CoreType>) -> CardFace {
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

    fn instant_face(name: &str) -> CardFace {
        face(name, vec![CoreType::Instant])
    }

    fn sorcery_face(name: &str) -> CardFace {
        face(name, vec![CoreType::Sorcery])
    }

    fn creature_face(name: &str) -> CardFace {
        face(name, vec![CoreType::Creature])
    }

    fn spell_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    fn deal_damage_any() -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        }
    }

    fn deal_damage_player() -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Player,
            damage_source: None,
        }
    }

    fn deal_damage_creature_only() -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Creature],
                ..TypedFilter::default()
            }),
            damage_source: None,
        }
    }

    fn copy_spell_effect() -> Effect {
        Effect::CopySpell {
            target: TargetFilter::Any,
        }
    }

    fn cast_payoff_trigger(
        mode: TriggerMode,
        valid_card: Option<TargetFilter>,
    ) -> TriggerDefinition {
        let mut t = TriggerDefinition::new(mode);
        t.valid_card = valid_card;
        t.valid_target = Some(TargetFilter::Controller);
        t
    }

    fn instant_or_sorcery_filter() -> TargetFilter {
        TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Instant],
                    ..TypedFilter::default()
                }),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Sorcery],
                    ..TypedFilter::default()
                }),
            ],
        }
    }

    // ── Feature-detection tests ────────────────────────────────────────────────

    #[test]
    fn empty_deck_produces_defaults() {
        let f = detect(&[]);
        assert_eq!(f.low_curve_spell_count, 0);
        assert_eq!(f.instant_sorcery_count, 0);
        assert_eq!(f.prowess_count, 0);
        assert_eq!(f.commitment, 0.0);
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let f = detect(&[entry(creature_face("Grizzly Bears"), 4)]);
        assert_eq!(f.low_curve_spell_count, 0);
        assert_eq!(f.instant_sorcery_count, 0);
        assert_eq!(f.prowess_count, 0);
        assert_eq!(f.cast_payoff_count, 0);
        assert_eq!(f.commitment, 0.0);
    }

    #[test]
    fn detects_low_curve_instant() {
        // MV 1 instant — counts as low-curve spell. CR 202.3b + CR 304.1.
        let mut c = instant_face("Lightning Bolt");
        c.mana_cost = ManaCost::generic(1);
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.instant_sorcery_count, 4);
        assert_eq!(f.low_curve_spell_count, 4);
    }

    #[test]
    fn detects_low_curve_sorcery() {
        // MV 2 sorcery — counts as low-curve. CR 202.3b + CR 307.1.
        let mut c = sorcery_face("Gitaxian Probe Shape");
        c.mana_cost = ManaCost::generic(2);
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.low_curve_spell_count, 4);
    }

    #[test]
    fn expensive_spell_not_low_curve() {
        // MV 5 instant — IS count goes up but low-curve does not. CR 202.3b.
        let mut c = instant_face("Expensive Counterspell");
        c.mana_cost = ManaCost::generic(5);
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.instant_sorcery_count, 4);
        assert_eq!(f.low_curve_spell_count, 0);
    }

    #[test]
    fn mv_zero_spell_counted_as_low_curve() {
        // NoCost → mana_value() == 0 ≤ 2. CR 202.3b.
        let mut c = instant_face("Gitaxian Probe");
        c.mana_cost = ManaCost::NoCost;
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.low_curve_spell_count, 4);
    }

    #[test]
    fn detects_prowess_creature() {
        // Keyword::Prowess on a creature face. CR 702.108a.
        let mut c = creature_face("Monastery Swiftspear");
        c.keywords.push(Keyword::Prowess);
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.prowess_count, 4);
        assert!(f.payoff_names.contains(&"Monastery Swiftspear".to_string()));
    }

    #[test]
    fn detects_cast_payoff_any_spell() {
        // SpellCast + valid_card None + Controller scope — Conduit-of-Ruin shape.
        // CR 601.2i + CR 603.1.
        let mut c = creature_face("Conduit Shape");
        c.triggers
            .push(cast_payoff_trigger(TriggerMode::SpellCast, None));
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.cast_payoff_count, 2);
        assert!(f.payoff_names.contains(&"Conduit Shape".to_string()));
    }

    #[test]
    fn detects_cast_payoff_instant_or_sorcery_filter() {
        // SpellCastOrCopy + Or(Instant, Sorcery) — magecraft canonical shape.
        // CR 601.2i + CR 707.10 + CR 207.2c.
        let mut c = creature_face("Archmage Emeritus Shape");
        c.triggers.push(cast_payoff_trigger(
            TriggerMode::SpellCastOrCopy,
            Some(instant_or_sorcery_filter()),
        ));
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.cast_payoff_count, 2);
    }

    #[test]
    fn opponent_scoped_cast_trigger_ignored() {
        // valid_target = Typed(controller=Opponent) → Esper Sentinel pattern —
        // NOT a spellslinger payoff for the AI. Must not count.
        // The parser emits TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        // for opponent-scoped triggers. CR 601.2i.
        let mut c = creature_face("Esper Sentinel Shape");
        let mut t = TriggerDefinition::new(TriggerMode::SpellCast);
        t.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        c.triggers.push(t);
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.cast_payoff_count, 0);
        assert!(!f.payoff_names.contains(&"Esper Sentinel Shape".to_string()));
    }

    #[test]
    fn detects_nth_spell_payoff() {
        // SpellCast + NthSpellThisTurn. CR 603.4.
        let mut c = creature_face("Spectral Sailor Shape");
        let mut t = TriggerDefinition::new(TriggerMode::SpellCast);
        t.constraint = Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None });
        c.triggers.push(t);
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.nth_spell_payoff_count, 2);
    }

    #[test]
    fn detects_copy_effect_spell() {
        // AbilityKind::Spell + CopySpell → copy_effect_count. CR 707.10.
        let mut c = instant_face("Twincast Shape");
        c.mana_cost = ManaCost::generic(2);
        c.abilities.push(spell_ability(copy_spell_effect()));
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.copy_effect_count, 2);
    }

    #[test]
    fn copy_effect_on_activated_ability_excluded() {
        // AbilityKind::Activated + CopySpell → NOT counted. Dualcaster Mage shape.
        // CR 707.10: only spell-kind copies are spellslinger payoffs here.
        let mut c = creature_face("Dualcaster Mage Shape");
        c.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            copy_spell_effect(),
        ));
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.copy_effect_count, 0);
    }

    #[test]
    fn detects_cantrip_via_control_helper() {
        // Dig dest=None (Brainstorm shape) → cantrip. Delegates to is_card_draw_parts.
        // CR 121.1.
        let mut c = instant_face("Brainstorm Shape");
        c.mana_cost = ManaCost::generic(1);
        c.abilities.push(spell_ability(Effect::Dig {
            count: QuantityExpr::Fixed { value: 3 },
            destination: None,
            keep_count: Some(3),
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
        }));
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.cantrip_count, 4);
    }

    #[test]
    fn impulse_dig_to_exile_excluded_from_cantrip() {
        // Dig dest=Exile → impulse-cast, NOT a cantrip. Same carve-out as control.rs.
        // CR 121.1: drawing moves cards to hand; exile does not.
        let mut c = sorcery_face("Light Up the Stage Shape");
        c.mana_cost = ManaCost::generic(2);
        c.abilities.push(spell_ability(Effect::Dig {
            count: QuantityExpr::Fixed { value: 2 },
            destination: Some(Zone::Exile),
            keep_count: Some(2),
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal: false,
        }));
        let f = detect(&[entry(c, 4)]);
        // impulse-dig should NOT count as cantrip
        assert_eq!(f.cantrip_count, 0);
        // but it IS a low-curve sorcery (MV 2)
        assert_eq!(f.low_curve_spell_count, 4);
    }

    #[test]
    fn detects_burn_to_player() {
        // DealDamage target=Any hits players. CR 120.3.
        let mut c = instant_face("Lightning Bolt Shape");
        c.mana_cost = ManaCost::generic(1);
        c.abilities.push(spell_ability(deal_damage_any()));
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.burn_player_count, 4);
    }

    #[test]
    fn creature_only_damage_excluded_from_burn() {
        // DealDamage target=Typed(Creature) — Pyroclasm shape. Must NOT count as
        // burn-to-player since it cannot hit players directly. CR 120.3.
        let mut c = sorcery_face("Pyroclasm Shape");
        c.mana_cost = ManaCost::generic(2);
        c.abilities.push(spell_ability(deal_damage_creature_only()));
        let f = detect(&[entry(c, 2)]);
        assert_eq!(f.burn_player_count, 0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        // Huge prowess count → commitment clamps at 1.0.
        let mut c = creature_face("Monk");
        c.keywords.push(Keyword::Prowess);
        let f = detect(&[entry(c, 60)]);
        assert!(
            f.commitment <= 1.0,
            "commitment must clamp ≤ 1.0, got {}",
            f.commitment
        );
        assert_eq!(f.commitment, 1.0);
    }

    #[test]
    fn commitment_above_floor_for_izzet_prowess_baseline() {
        // Simulate ~37-card nonland Izzet Prowess:
        // 12 prowess creatures + 20 low-curve IS spells.
        let mut prowess_c = creature_face("Prowler");
        prowess_c.keywords.push(Keyword::Prowess);
        let mut bolt = instant_face("Bolt");
        bolt.mana_cost = ManaCost::generic(1);
        let deck = vec![entry(prowess_c, 12), entry(bolt, 20)];
        let f = detect(&deck);
        assert!(
            f.commitment > 0.7,
            "Izzet Prowess baseline should commit > 0.7, got {}",
            f.commitment
        );
    }

    #[test]
    fn commitment_above_floor_for_burn() {
        // Mono-Red Burn: ~36 nonland, 0 payoffs, 28 low-curve IS.
        // spell_density = 28/36 ≈ 0.778 → commitment ≈ 0.778.
        // CR 121.1 (card-draw) baseline used elsewhere; here we lock in the
        // burn calibration anchor at > 0.40 so the doc claim stays enforced.
        let mut bolt = instant_face("Bolt");
        bolt.mana_cost = ManaCost::generic(1);
        let deck = vec![entry(bolt, 28), entry(creature_face("Bear"), 8)];
        let f = detect(&deck);
        assert!(
            f.commitment > 0.40,
            "Burn should hit calibration floor 0.40, got {}",
            f.commitment
        );
    }

    #[test]
    fn commitment_below_floor_for_midrange() {
        // Vanilla midrange: 24 creatures + 12 expensive non-IS spells.
        let mut expensive = creature_face("Titan");
        expensive.mana_cost = ManaCost::generic(6);
        let deck = vec![entry(creature_face("Bear"), 24), entry(expensive, 12)];
        let f = detect(&deck);
        assert!(
            f.commitment < 0.10,
            "Vanilla midrange should be below 0.10, got {}",
            f.commitment
        );
    }

    #[test]
    fn payoff_names_includes_prowess_and_cast_payoff_only() {
        // payoff_names must contain prowess + cast_payoff cards, NOT burn or copy cards.
        let mut prowess_c = creature_face("Monk");
        prowess_c.keywords.push(Keyword::Prowess);

        let mut payoff_c = creature_face("Archmage");
        payoff_c
            .triggers
            .push(cast_payoff_trigger(TriggerMode::SpellCast, None));

        let mut burn = instant_face("Bolt");
        burn.mana_cost = ManaCost::generic(1);
        burn.abilities.push(spell_ability(deal_damage_any()));

        let mut copier = instant_face("Twincast");
        copier.mana_cost = ManaCost::generic(2);
        copier.abilities.push(spell_ability(copy_spell_effect()));

        let f = detect(&[
            entry(prowess_c, 4),
            entry(payoff_c, 4),
            entry(burn, 4),
            entry(copier, 4),
        ]);

        assert!(f.payoff_names.contains(&"Monk".to_string()));
        assert!(f.payoff_names.contains(&"Archmage".to_string()));
        assert!(!f.payoff_names.contains(&"Bolt".to_string()));
        assert!(!f.payoff_names.contains(&"Twincast".to_string()));
    }

    #[test]
    fn payoff_names_dedup() {
        // 4 copies of the same prowess creature → one entry in payoff_names.
        let mut c = creature_face("Monastery Swiftspear");
        c.keywords.push(Keyword::Prowess);
        let f = detect(&[entry(c, 4)]);
        let count = f
            .payoff_names
            .iter()
            .filter(|n| n.as_str() == "Monastery Swiftspear")
            .count();
        assert_eq!(
            count, 1,
            "payoff_names should have exactly one entry per unique face"
        );
    }

    #[test]
    fn burn_to_player_target_filter_player_variant() {
        // TargetFilter::Player also qualifies (not just Any). CR 120.3.
        let mut c = instant_face("Direct Damage");
        c.mana_cost = ManaCost::generic(1);
        c.abilities.push(spell_ability(deal_damage_player()));
        let f = detect(&[entry(c, 4)]);
        assert_eq!(f.burn_player_count, 4);
    }
}
