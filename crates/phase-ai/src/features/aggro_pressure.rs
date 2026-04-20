//! Aggro pressure feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `CardFace.card_type.core_types: Vec<CoreType>`; `CoreType::Creature` at
//!   `crates/engine/src/types/card_type.rs:74-77` (CR 302).
//! - `CardFace.mana_cost: ManaCost` at `crates/engine/src/types/card.rs:51`;
//!   `ManaCost::mana_value()` at `mana.rs:468` (CR 202.3).
//! - `CardFace.keywords: Vec<Keyword>` at `card.rs:60` (CR 702.1: keywords
//!   are named ability shorthands; reminder text summarizes their rules).
//! - Evasion keyword variants: `Keyword::Flying` (CR 702.9), `Keyword::Haste`
//!   (CR 702.10), `Keyword::Menace` (CR 702.111), `Keyword::Trample` (CR 702.19),
//!   `Keyword::Skulk` (CR 702.118), `Keyword::Shadow` (CR 702.28),
//!   `Keyword::Intimidate` (CR 702.13), `Keyword::Fear` (CR 702.36),
//!   `Keyword::Horsemanship` (CR 702.31) at `keywords.rs:277-302`.
//! - `ContinuousModification::AddKeyword { keyword }` at `ability.rs:5023-5025`
//!   (CR 613.1f).
//! - `StaticDefinition.affected: Option<TargetFilter>` at `ability.rs:4681`;
//!   `StaticDefinition.modifications: Vec<ContinuousModification>` at `ability.rs:4683`.
//! - `AbilityKind::Spell` at `ability.rs:3749` (CR 112.1).
//! - `Effect::DealDamage { target, .. }` at `ability.rs:2085-2093` (CR 120.3).
//! - `Effect::Pump { power, toughness, target }` at `ability.rs:2098-2105`
//!   (CR 120.3 / CR 613.4c).
//! - `StaticMode::Continuous` + `FilterProp::Attacking` (anthem-attacker static)
//!   at `statics.rs:116`, `ability.rs:834`.
//! - `ControllerRef::You` at `ability.rs:813-818` (CR 109.5).
//!
//! No parser remediation required — aggro-shaped abilities classify structurally
//! via the existing typed AST.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, Effect, FilterProp, StaticDefinition,
    TargetFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;

use crate::ability_chain::collect_chain_effects;

/// Maximum mana value to qualify as "low curve" — CR 202.3.
pub const LOW_CURVE_CAP: u32 = 2;

/// Tactical-policy floor for aggro pressure commitment.
pub const AGGRO_COMMITMENT_FLOOR: f32 = 0.45;

/// Mulligan-policy floor for aggro keepables.
pub const MULLIGAN_FLOOR: f32 = 0.55;

/// Tempo-class floor — at or above this commitment the deck reads as
/// `TempoClass::Aggro` in `plan/curves.rs`.
pub const AGGRO_TEMPO_FLOOR: f32 = 0.55;

/// Evasion keywords — excludes Reach (defensive, not evasion). CR 702.9 /
/// CR 702.111 / CR 702.19 / CR 702.118 / CR 702.28 / CR 702.13 / CR 702.36 /
/// CR 702.31.
const EVASION_KEYWORDS: &[Keyword] = &[
    Keyword::Flying,
    Keyword::Menace,
    Keyword::Trample,
    Keyword::TrampleOverPlaneswalkers,
    Keyword::Skulk,
    Keyword::Shadow,
    Keyword::Intimidate,
    Keyword::Fear,
    Keyword::Horsemanship,
];

/// CR 302 + CR 202.3 + CR 702.10: per-deck aggro pressure classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities`, `CardFace.keywords`, and `CardFace.static_abilities` —
/// never by card name. Policies consume this feature to weight attack pressure
/// and mulligan decisions.
#[derive(Debug, Clone, Default)]
pub struct AggroPressureFeature {
    /// Low-curve creatures (mana value ≤ 2). CR 202.3 + CR 302.
    pub low_curve_creature_count: u32,
    /// Creatures with haste (face keyword or self-grant static). CR 702.10.
    pub hasty_creature_count: u32,
    /// Creatures with at least one evasion keyword. CR 702.9 / 702.111 / etc.
    pub evasion_creature_count: u32,
    /// Spells that deal damage targeting any (player-reachable). CR 120.3.
    pub burn_spell_count: u32,
    /// Spell or static pump effects applicable in combat. CR 613.4c.
    pub combat_pump_count: u32,
    /// Total non-land card count (denominator for densities).
    pub total_nonland: u32,
    /// `low_curve_creature_count / max(total_nonland, 1)`.
    pub low_curve_density: f32,
    /// Weighted commitment score `0.0..=1.0`.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across the aggro pressure axes.
pub fn detect(deck: &[DeckEntry]) -> AggroPressureFeature {
    if deck.is_empty() {
        return AggroPressureFeature::default();
    }

    let mut low_curve_creature_count = 0u32;
    let mut hasty_creature_count = 0u32;
    let mut evasion_creature_count = 0u32;
    let mut burn_spell_count = 0u32;
    let mut combat_pump_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        let is_land = face.card_type.core_types.contains(&CoreType::Land);
        if is_land {
            continue;
        }

        for _ in 0..entry.count {
            total_nonland = total_nonland.saturating_add(1);

            if is_low_curve_creature_parts(&face.card_type.core_types, &face.mana_cost) {
                low_curve_creature_count = low_curve_creature_count.saturating_add(1);
            }
            if is_hasty_creature_parts(
                &face.card_type.core_types,
                &face.keywords,
                &face.static_abilities,
            ) {
                hasty_creature_count = hasty_creature_count.saturating_add(1);
            }
            if is_evasion_creature_parts(
                &face.card_type.core_types,
                &face.keywords,
                &face.static_abilities,
            ) {
                evasion_creature_count = evasion_creature_count.saturating_add(1);
            }
            if is_burn_spell_parts(&face.card_type.core_types, &face.abilities) {
                burn_spell_count = burn_spell_count.saturating_add(1);
            }
            if is_combat_pump_parts(&face.abilities, &face.static_abilities) {
                combat_pump_count = combat_pump_count.saturating_add(1);
            }
        }
    }

    let nonland_denom = total_nonland.max(1) as f32;
    let low = low_curve_creature_count as f32 / nonland_denom;
    let hasty = hasty_creature_count as f32 / nonland_denom;
    let evasion = evasion_creature_count as f32 / nonland_denom;
    let burn = burn_spell_count as f32 / nonland_denom;
    let pump = combat_pump_count as f32 / nonland_denom;

    // Weighted sum: low-curve density is the primary axis; evasion/hasty add
    // tempo value; burn and pump contribute pressure at lower weights.
    let raw = 2.0 * low + 1.0 * hasty + 0.8 * evasion + 0.6 * burn + 0.4 * pump;
    let commitment = raw.clamp(0.0, 1.0);

    AggroPressureFeature {
        low_curve_creature_count,
        hasty_creature_count,
        evasion_creature_count,
        burn_spell_count,
        combat_pump_count,
        total_nonland,
        low_curve_density: low,
        commitment,
    }
}

// ─── Parts predicates ────────────────────────────────────────────────────────

/// True if this card face is a creature with mana value ≤ `LOW_CURVE_CAP`.
/// CR 302: creature core type. CR 202.3: mana value.
pub(crate) fn is_low_curve_creature_parts(
    core_types: &[CoreType],
    mana_cost: &engine::types::mana::ManaCost,
) -> bool {
    core_types.contains(&CoreType::Creature) && mana_cost.mana_value() <= LOW_CURVE_CAP
}

/// True if this card face is a creature with haste (via face keyword or
/// self-grant static). CR 702.10a: haste is a static ability.
pub(crate) fn is_hasty_creature_parts(
    core_types: &[CoreType],
    keywords: &[Keyword],
    static_abilities: &[StaticDefinition],
) -> bool {
    if !core_types.contains(&CoreType::Creature) {
        return false;
    }
    // Direct face keyword: `CardFace.keywords` includes Haste. CR 702.10.
    if keywords.contains(&Keyword::Haste) {
        return true;
    }
    // Self-grant haste via continuous static — e.g. "~ has haste" prints as a
    // static with `affected = Some(SelfRef) | None` and `AddKeyword { Haste }`.
    // CR 613.1f: ability-adding continuous effects applied in layer 6.
    static_abilities
        .iter()
        .any(|s| s.mode == StaticMode::Continuous && static_grants_self_keyword(s, &Keyword::Haste))
}

/// True if this card face is a creature with at least one evasion keyword.
/// CR 702.9 (Flying), CR 702.111 (Menace), CR 702.19 (Trample),
/// CR 702.118 (Skulk), CR 702.28 (Shadow), CR 702.13 (Intimidate),
/// CR 702.36 (Fear), CR 702.31 (Horsemanship).
/// Note: Reach is defensive and is intentionally excluded.
pub(crate) fn is_evasion_creature_parts(
    core_types: &[CoreType],
    keywords: &[Keyword],
    static_abilities: &[StaticDefinition],
) -> bool {
    if !core_types.contains(&CoreType::Creature) {
        return false;
    }
    // Direct face keyword.
    if keywords.iter().any(|k| EVASION_KEYWORDS.contains(k)) {
        return true;
    }
    // Self-grant evasion via continuous static.
    static_abilities.iter().any(|s| {
        s.mode == StaticMode::Continuous
            && EVASION_KEYWORDS
                .iter()
                .any(|k| static_grants_self_keyword(s, k))
    })
}

/// True if this card face is a non-creature spell that deals damage to a target
/// that can include players (`TargetFilter::Any` or `TargetFilter::Player`).
/// CR 120.3: damage to players. Excludes `DamageAll` (sweepers, not burn).
pub(crate) fn is_burn_spell_parts(
    core_types: &[CoreType],
    abilities: &[AbilityDefinition],
) -> bool {
    // Only non-creature spells qualify as burn — creature spells are already
    // counted as creatures above.
    if core_types.contains(&CoreType::Creature) {
        return false;
    }
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability).iter().any(|e| {
                matches!(
                    e,
                    Effect::DealDamage { target, .. }
                    if can_target_player(target)
                )
            })
    })
}

/// True if this card face has a pump spell or a combat-attacker static anthem.
///
/// Two shapes qualify:
/// 1. Spell ability with `Effect::Pump` — instant-speed combat trick. CR 613.4c.
/// 2. Continuous static with `FilterProp::Attacking` and `AddPower`/`AddToughness`
///    — lord-anthem-style boost restricted to attackers. CR 613.4c.
pub(crate) fn is_combat_pump_parts(
    abilities: &[AbilityDefinition],
    static_abilities: &[StaticDefinition],
) -> bool {
    // Shape 1: instant/sorcery pump spell.
    let is_pump_spell = abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability)
                .iter()
                .any(|e| matches!(e, Effect::Pump { .. }))
    });
    if is_pump_spell {
        return true;
    }
    // Shape 2: continuous static that boosts attacking creatures.
    // `FilterProp::Attacking` restricts the anthem to combatants. CR 613.4c.
    static_abilities.iter().any(|s| {
        s.mode == StaticMode::Continuous
            && affected_filter_has_prop(&s.affected, &FilterProp::Attacking)
            && s.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddPower { .. }
                        | ContinuousModification::AddToughness { .. }
                )
            })
    })
}

// ─── Public single-face wrappers ──────────────────────────────────────────────

/// Public wrapper: calls `is_low_curve_creature_parts` against `CardFace`.
pub fn is_low_curve_creature(face: &CardFace) -> bool {
    is_low_curve_creature_parts(&face.card_type.core_types, &face.mana_cost)
}

/// Public wrapper: calls `is_hasty_creature_parts` against `CardFace`.
pub fn is_hasty_creature(face: &CardFace) -> bool {
    is_hasty_creature_parts(
        &face.card_type.core_types,
        &face.keywords,
        &face.static_abilities,
    )
}

/// Public wrapper: calls `is_evasion_creature_parts` against `CardFace`.
pub fn is_evasion_creature(face: &CardFace) -> bool {
    is_evasion_creature_parts(
        &face.card_type.core_types,
        &face.keywords,
        &face.static_abilities,
    )
}

/// Public wrapper: calls `is_burn_spell_parts` against `CardFace`.
pub fn is_burn_spell(face: &CardFace) -> bool {
    is_burn_spell_parts(&face.card_type.core_types, &face.abilities)
}

/// Public wrapper: calls `is_combat_pump_parts` against `CardFace`.
pub fn is_combat_pump(face: &CardFace) -> bool {
    is_combat_pump_parts(&face.abilities, &face.static_abilities)
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// True if the static's `modifications` include `AddKeyword { keyword }` AND
/// the `affected` filter is a self-reference (SelfRef or None — both indicate
/// the ability's source as the affected object). CR 613.1f.
fn static_grants_self_keyword(s: &StaticDefinition, keyword: &Keyword) -> bool {
    let is_self_scoped = matches!(s.affected, None | Some(TargetFilter::SelfRef));
    is_self_scoped
        && s.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddKeyword { keyword: k } if k == keyword))
}

/// True if `TargetFilter::Any` or `TargetFilter::Player` — both can reach
/// players. Used to distinguish burn (face-applicable) from removal (creatures only).
fn can_target_player(target: &TargetFilter) -> bool {
    matches!(target, TargetFilter::Any | TargetFilter::Player)
}

/// True if the `affected` `TargetFilter` contains `FilterProp::Attacking`
/// anywhere in its filter tree. Handles `Typed`, `And`, and `Or` wrappers.
fn affected_filter_has_prop(affected: &Option<TargetFilter>, prop: &FilterProp) -> bool {
    let Some(filter) = affected else { return false };
    filter_has_prop(filter, prop)
}

fn filter_has_prop(filter: &TargetFilter, prop: &FilterProp) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.contains(prop),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(|f| filter_has_prop(f, prop))
        }
        _ => false,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Effect, FilterProp, PtValue,
        QuantityExpr, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::statics::StaticMode;

    fn creature_face(mv: u32) -> CardFace {
        CardFace {
            mana_cost: ManaCost::generic(mv),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            },
            power: Some(PtValue::Fixed(mv as i32)),
            ..CardFace::default()
        }
    }

    fn creature_face_with_keyword(mv: u32, kw: Keyword) -> CardFace {
        let mut face = creature_face(mv);
        face.keywords.push(kw);
        face
    }

    fn sorcery_face_with_burn() -> CardFace {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        ability.kind = AbilityKind::Spell;
        CardFace {
            mana_cost: ManaCost::generic(1),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Sorcery],
                subtypes: Vec::new(),
            },
            abilities: vec![ability],
            ..CardFace::default()
        }
    }

    fn land_face() -> CardFace {
        CardFace {
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Land],
                subtypes: Vec::new(),
            },
            ..CardFace::default()
        }
    }

    fn entry(face: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card: face, count }
    }

    // ── Feature tests ──────────────────────────────────────────────────────

    #[test]
    fn empty_deck_produces_defaults() {
        let f = detect(&[]);
        assert_eq!(f.low_curve_creature_count, 0);
        assert_eq!(f.commitment, 0.0);
    }

    #[test]
    fn detects_low_curve_creature_count() {
        let deck = vec![
            entry(creature_face(1), 4),
            entry(creature_face(2), 4),
            entry(creature_face(3), 4), // not low curve
        ];
        let f = detect(&deck);
        assert_eq!(f.low_curve_creature_count, 8);
    }

    #[test]
    fn creature_with_mv_three_not_low_curve() {
        let deck = vec![entry(creature_face(3), 4)];
        let f = detect(&deck);
        assert_eq!(f.low_curve_creature_count, 0);
    }

    #[test]
    fn detects_face_keyword_haste() {
        let deck = vec![entry(creature_face_with_keyword(2, Keyword::Haste), 4)];
        let f = detect(&deck);
        assert_eq!(f.hasty_creature_count, 4);
    }

    #[test]
    fn detects_self_grant_haste_via_static() {
        let static_haste = StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: vec![],
            characteristic_defining: false,
            description: None,
        };
        let mut face = creature_face(2);
        face.static_abilities.push(static_haste);
        let deck = vec![entry(face, 2)];
        let f = detect(&deck);
        assert_eq!(f.hasty_creature_count, 2);
    }

    #[test]
    fn detects_each_evasion_keyword() {
        // Parameterize over the 9 evasion keywords.
        let evasion_kws = [
            Keyword::Flying,
            Keyword::Menace,
            Keyword::Trample,
            Keyword::TrampleOverPlaneswalkers,
            Keyword::Skulk,
            Keyword::Shadow,
            Keyword::Intimidate,
            Keyword::Fear,
            Keyword::Horsemanship,
        ];
        for kw in &evasion_kws {
            let deck = vec![entry(creature_face_with_keyword(2, kw.clone()), 1)];
            let f = detect(&deck);
            assert_eq!(f.evasion_creature_count, 1, "Expected evasion for {kw:?}");
        }
    }

    #[test]
    fn reach_does_not_count_as_evasion() {
        let deck = vec![entry(creature_face_with_keyword(2, Keyword::Reach), 4)];
        let f = detect(&deck);
        assert_eq!(f.evasion_creature_count, 0);
    }

    #[test]
    fn detects_burn_spell_targeting_player() {
        let deck = vec![entry(sorcery_face_with_burn(), 4)];
        let f = detect(&deck);
        assert_eq!(f.burn_spell_count, 4);
    }

    #[test]
    fn damage_only_to_creatures_not_burn_to_player() {
        // DealDamage with Typed creature filter — not a burn spell.
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
        );
        ability.kind = AbilityKind::Spell;
        let face = CardFace {
            mana_cost: ManaCost::generic(2),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Sorcery],
                subtypes: Vec::new(),
            },
            abilities: vec![ability],
            ..CardFace::default()
        };
        let deck = vec![entry(face, 4)];
        let f = detect(&deck);
        assert_eq!(f.burn_spell_count, 0);
    }

    #[test]
    fn detects_combat_pump_static() {
        let attacking_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::Attacking],
        });
        let static_anthem = StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(attacking_filter),
            modifications: vec![ContinuousModification::AddPower { value: 1 }],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: vec![],
            characteristic_defining: false,
            description: None,
        };
        let face = CardFace {
            mana_cost: ManaCost::generic(3),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Enchantment],
                subtypes: Vec::new(),
            },
            static_abilities: vec![static_anthem],
            ..CardFace::default()
        };
        let deck = vec![entry(face, 2)];
        let f = detect(&deck);
        assert_eq!(f.combat_pump_count, 2);
    }

    #[test]
    fn detects_pump_spell() {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        );
        ability.kind = AbilityKind::Spell;
        let face = CardFace {
            mana_cost: ManaCost::generic(1),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Instant],
                subtypes: Vec::new(),
            },
            abilities: vec![ability],
            ..CardFace::default()
        };
        let deck = vec![entry(face, 4)];
        let f = detect(&deck);
        assert_eq!(f.combat_pump_count, 4);
    }

    #[test]
    fn non_creature_low_mv_not_counted() {
        // A 1-mana Sorcery is NOT a low-curve creature.
        let deck = vec![entry(sorcery_face_with_burn(), 4)];
        let f = detect(&deck);
        assert_eq!(f.low_curve_creature_count, 0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        // All slots are low-curve creatures with evasion and haste → raw > 1.
        let deck = vec![entry(
            {
                let mut face = creature_face_with_keyword(1, Keyword::Flying);
                face.keywords.push(Keyword::Haste);
                face
            },
            60,
        )];
        let f = detect(&deck);
        assert!(f.commitment <= 1.0, "commitment must be ≤ 1.0");
        assert!(f.commitment > 0.9, "commitment should be near 1.0");
    }

    #[test]
    fn mono_red_burn_calibration_hits_floor() {
        // Calibration fixture: dense low-curve creatures + burn → commitment > 0.85.
        let deck = [
            entry(creature_face(1), 12),
            entry(creature_face(2), 12),
            entry(sorcery_face_with_burn(), 12),
            entry(creature_face_with_keyword(2, Keyword::Haste), 8),
            entry(land_face(), 20),
        ];
        let f = detect(&deck);
        assert!(
            f.commitment > 0.85,
            "mono-red fixture should exceed 0.85, got {}",
            f.commitment
        );
    }

    #[test]
    fn non_aggro_deck_below_floor() {
        // UW control fixture — high-cost creatures + no burn.
        let counterspell = {
            let mut ability = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_static: None,
                    unless_payment: None,
                },
            );
            ability.kind = AbilityKind::Spell;
            CardFace {
                mana_cost: ManaCost::generic(2),
                card_type: CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Instant],
                    subtypes: Vec::new(),
                },
                abilities: vec![ability],
                ..CardFace::default()
            }
        };
        let deck = [
            entry(creature_face(5), 8),
            entry(creature_face(6), 4),
            entry(counterspell, 12),
            entry(land_face(), 24),
        ];
        let f = detect(&deck);
        assert!(
            f.commitment < 0.2,
            "UW control fixture should be below 0.2, got {}",
            f.commitment
        );
    }

    #[test]
    fn nonland_count_excludes_lands() {
        let deck = [entry(creature_face(2), 20), entry(land_face(), 20)];
        let f = detect(&deck);
        assert_eq!(f.total_nonland, 20);
    }
}
