//! Tribal feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `CardFace.card_type.subtypes: Vec<String>` at
//!   `crates/engine/src/types/card_type.rs:109` (CR 205.3: subtypes).
//! - `CardFace.card_type.core_types: Vec<CoreType>`; `CoreType::Creature`,
//!   `CoreType::Kindred`, `CoreType::Tribal` present at `card_type.rs:74-77`
//!   (CR 308: Kindred cards share creature subtypes).
//! - `CardFace.triggers: Vec<TriggerDefinition>`; `TriggerMode::ChangesZone`
//!   for ETB triggers at `types/triggers.rs:24-27` (CR 603.6a).
//! - `CardFace.static_abilities: Vec<StaticDefinition>` with
//!   `affected: Option<TargetFilter>` at `ability.rs:4681` and
//!   `modifications: Vec<ContinuousModification>` at `ability.rs:4683`.
//! - `TypeFilter::Subtype(String)` at `ability.rs:794`;
//!   `TypedFilter::get_subtype()` at `ability.rs:1061`.
//! - `Effect::Token { types: Vec<String>, .. }` at `ability.rs:2131`.
//! - `StaticMode::ReduceCost { spell_filter: Option<TargetFilter>, .. }`
//!   at `statics.rs:136`.
//! - `TriggerDefinition.execute: Option<Box<AbilityDefinition>>`
//!   at `ability.rs:4520`.
//! - `ContinuousModification::AddAllCreatureTypes` at `ability.rs:5076` —
//!   changeling detection.
//! - `ContinuousModification::GrantTrigger` at `ability.rs:5036` and
//!   `ContinuousModification::AddStaticMode` at `ability.rs:5096` —
//!   ability-granting lords.
//!
//! No parser remediation required — tribal-shaped patterns classify
//! structurally via existing typed AST.

use std::collections::BTreeMap;

use engine::game::DeckEntry;
use engine::parser::oracle_util::canonicalize_subtype_name;
use engine::types::ability::{
    ContinuousModification, ControllerRef, Effect, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;

/// Minimum dominant-tribe commitment required to set `dominant_tribe`.
/// CR 205.3: subtypes determine tribal membership; below this floor the
/// tribe is incidental rather than a deck-defining axis.
const DOMINANCE_FLOOR: f32 = 0.25;

/// Maximum number of `TribeEntry` records retained (ranked by commitment desc).
const MAX_TRIBES: usize = 4;

/// Tactical lord-priority floor — `TribalLordPriorityPolicy` engages at or
/// above this `commitment`. Below it, lord re-ordering is unwarranted because
/// the deck has only incidental tribal subtypes. CR 205.3.
pub const LORD_PRIORITY_FLOOR: f32 = 0.3;

/// Mulligan-and-planning floor — `TribalDensityMulligan` engages at or above
/// this commitment, and `expected_threats_for` front-loads creature
/// deployment on turns 2–4 to capture early lord anthem value.
pub const MULLIGAN_FLOOR: f32 = 0.4;

/// Tempo-class floor — at or above this commitment the deck is classified as
/// `TempoClass::Aggro`. Tribal anthems compress threat density into early
/// turns, so the game plan reads as aggro regardless of coarse `archetype`.
pub const AGGRO_TEMPO_FLOOR: f32 = 0.55;

/// Per-tribe structural data — computed once per game from the deck list.
#[derive(Debug, Clone, Default)]
pub struct TribeEntry {
    /// Canonical subtype name (e.g. "Elf", "Goblin").
    pub subtype: String,
    /// Number of tribal members in the deck (creature/kindred cards with this subtype).
    pub member_count: u32,
    /// Lords — static abilities that boost other tribe members.
    /// CR 613.4c: lord anthems apply power/toughness in layer 7c.
    pub lord_count: u32,
    /// ETB payoffs — triggers that fire when a tribe member enters the battlefield.
    pub etb_payoff_count: u32,
    /// Cost reducers — statics reducing the cost of tribal spells.
    pub cost_reducer_count: u32,
    /// Token generators — abilities producing tokens of this subtype.
    pub token_gen_count: u32,
    /// Per-tribe commitment `0.0..=1.0`.
    pub commitment: f32,
}

/// CR 205.3 + CR 308: structural tribal classification for a single deck.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.card_type.subtypes`, `CardFace.triggers`, `CardFace.static_abilities`,
/// and `CardFace.abilities` — never by card name. Policies consume this feature
/// to weight lord prioritization and mulligan decisions.
#[derive(Debug, Clone, Default)]
pub struct TribalFeature {
    /// Canonical subtype name of the dominant tribe, or `None` when commitment
    /// falls below `DOMINANCE_FLOOR`.
    pub dominant_tribe: Option<String>,
    /// All detected tribes ranked by commitment desc, capped at `MAX_TRIBES`.
    pub tribes: Vec<TribeEntry>,
    /// Commitment of the dominant tribe (`0.0` when there is none).
    pub commitment: f32,
}

/// Structural detection — two-pass walk over every `DeckEntry`.
pub fn detect(deck: &[DeckEntry]) -> TribalFeature {
    if deck.is_empty() {
        return TribalFeature::default();
    }

    // ---- Pass 1: membership census ----
    // For each creature/kindred card, add its count to every listed subtype.
    let mut tribe_map: BTreeMap<String, TribeEntry> = BTreeMap::new();

    for entry in deck {
        let face = &entry.card;
        if !is_tribal_member_type(face) {
            continue;
        }
        for raw_sub in &face.card_type.subtypes {
            let sub = canonicalize_subtype_name(raw_sub);
            if sub.is_empty() {
                continue;
            }
            let te = tribe_map.entry(sub.clone()).or_insert_with(|| TribeEntry {
                subtype: sub,
                ..Default::default()
            });
            te.member_count = te.member_count.saturating_add(entry.count);
        }
    }

    // Changelings boost every *already-detected* tribe (not create new ones).
    // CR 205.3m: a changeling has every creature type.
    for entry in deck {
        let face = &entry.card;
        if is_changeling(face) {
            for te in tribe_map.values_mut() {
                te.member_count = te.member_count.saturating_add(entry.count);
            }
        }
    }

    // If no tribes detected, return default.
    if tribe_map.is_empty() {
        return TribalFeature::default();
    }

    // ---- Pass 2: payoff census ----
    // Single-traversal: borrow each `TribeEntry` mutably and check every face
    // against every tribe in one walk. Avoids the prior O(entries × tribes)
    // key-clone allocation and the get_mut+Index double-lookup pattern.
    for entry in deck {
        let face = &entry.card;
        for (tribe_key, te) in tribe_map.iter_mut() {
            if is_lord(face, tribe_key) {
                te.lord_count = te.lord_count.saturating_add(entry.count);
            }
            if is_etb_payoff(face, tribe_key) {
                te.etb_payoff_count = te.etb_payoff_count.saturating_add(entry.count);
            }
            if is_cost_reducer(face, tribe_key) {
                te.cost_reducer_count = te.cost_reducer_count.saturating_add(entry.count);
            }
            if is_token_generator(face, tribe_key) {
                te.token_gen_count = te.token_gen_count.saturating_add(entry.count);
            }
        }
    }

    // ---- Commitment formula per tribe ----
    // CR 205.3 + CR 613.4c: weighted combination of tribal indicators.
    for te in tribe_map.values_mut() {
        let raw = 0.03 * te.member_count as f32
            + 0.15 * te.lord_count as f32
            + 0.10 * te.etb_payoff_count as f32
            + 0.10 * te.cost_reducer_count as f32
            + 0.06 * te.token_gen_count as f32;
        te.commitment = raw.min(1.0);
    }

    // ---- Dominant tribe selection ----
    // Sort by (commitment desc, member_count desc, subtype asc) for determinism.
    let mut sorted: Vec<TribeEntry> = tribe_map.into_values().collect();
    sorted.sort_by(|a, b| {
        b.commitment
            .partial_cmp(&a.commitment)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.member_count.cmp(&a.member_count))
            .then_with(|| a.subtype.cmp(&b.subtype))
    });

    let dominant_tribe = sorted.first().and_then(|te| {
        if te.commitment >= DOMINANCE_FLOOR {
            Some(te.subtype.clone())
        } else {
            None
        }
    });
    let commitment = sorted.first().map_or(0.0, |te| {
        if te.commitment >= DOMINANCE_FLOOR {
            te.commitment
        } else {
            0.0
        }
    });

    sorted.truncate(MAX_TRIBES);

    TribalFeature {
        dominant_tribe,
        tribes: sorted,
        commitment,
    }
}

/// True if this face is a tribal member — Creature, Kindred, or (legacy)
/// Tribal typed card. CR 308: Kindred cards share creature subtypes.
fn is_tribal_member_type(face: &CardFace) -> bool {
    face.card_type
        .core_types
        .iter()
        .any(|t| matches!(t, CoreType::Creature | CoreType::Kindred | CoreType::Tribal))
}

/// True if this face has `ContinuousModification::AddAllCreatureTypes` in any
/// static ability modification — i.e. it is a changeling. CR 205.3m.
fn is_changeling(face: &CardFace) -> bool {
    face.static_abilities.iter().any(|s| {
        s.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddAllCreatureTypes))
    })
}

/// True if this face has a static ability that:
/// 1. `affected` references creatures of `tribe` you control (or unset — wildcard), and
/// 2. `modifications` contains at least one lord-class modification.
///
/// CR 613.4c: P/T anthems apply in layer 7c.
/// CR 613.1f: Ability-adding effects apply in layer 6.
pub(crate) fn is_lord(face: &CardFace, tribe: &str) -> bool {
    statics_are_lord_for(&face.static_abilities, tribe)
}

/// Check whether a slice of static abilities constitutes a lord for `tribe`.
/// Re-exported so runtime policies can run the check against `GameObject.static_abilities`
/// without requiring a `CardFace` conversion.
pub(crate) fn statics_are_lord_for(statics: &[StaticDefinition], tribe: &str) -> bool {
    statics.iter().any(|s| static_is_lord_for(s, tribe))
}

fn static_is_lord_for(s: &StaticDefinition, tribe: &str) -> bool {
    let affected_matches = s
        .affected
        .as_ref()
        .is_some_and(|f| filter_references_tribe_you_control(f, tribe));
    if !affected_matches {
        return false;
    }
    s.modifications.iter().any(is_lord_modification)
}

fn is_lord_modification(m: &ContinuousModification) -> bool {
    matches!(
        m,
        ContinuousModification::AddPower { .. }
            | ContinuousModification::AddToughness { .. }
            | ContinuousModification::AddKeyword { .. }
            | ContinuousModification::GrantAbility { .. }
            | ContinuousModification::GrantTrigger { .. }
            | ContinuousModification::AddStaticMode { .. }
    )
}

/// True if this face has a `ChangesZone` trigger targeting a creature of `tribe`
/// you control entering the battlefield (not from the battlefield).
///
/// CR 603.6a: triggered abilities fire when the specified event occurs.
fn is_etb_payoff(face: &CardFace, tribe: &str) -> bool {
    face.triggers
        .iter()
        .any(|t| trigger_is_etb_payoff_for(t, tribe))
}

fn trigger_is_etb_payoff_for(t: &engine::types::ability::TriggerDefinition, tribe: &str) -> bool {
    if t.mode != TriggerMode::ChangesZone {
        return false;
    }
    if t.destination != Some(Zone::Battlefield) {
        return false;
    }
    // Origin = battlefield means it's a "leaves battlefield" trigger.
    if matches!(t.origin, Some(Zone::Battlefield)) {
        return false;
    }
    let Some(filter) = t.valid_card.as_ref() else {
        return false;
    };
    filter_references_tribe_you_control(filter, tribe)
}

/// True if this face has a `ReduceCost` static whose `spell_filter` references
/// the tribe's subtype.
fn is_cost_reducer(face: &CardFace, tribe: &str) -> bool {
    face.static_abilities.iter().any(|s| {
        if let StaticMode::ReduceCost {
            spell_filter: Some(filter),
            ..
        } = &s.mode
        {
            return filter_references_subtype(filter, tribe);
        }
        false
    })
}

/// True if this face produces tokens of the given tribe subtype via any
/// `Effect::Token { types, .. }` in its spell/activated ability chains or
/// trigger execute chains.
fn is_token_generator(face: &CardFace, tribe: &str) -> bool {
    let in_abilities = face.abilities.iter().any(|ability| {
        collect_chain_effects(ability)
            .iter()
            .any(|e| token_effect_has_tribe(e, tribe))
    });
    if in_abilities {
        return true;
    }
    face.triggers.iter().any(|t| {
        t.execute.as_ref().is_some_and(|exec| {
            collect_chain_effects(exec)
                .iter()
                .any(|e| token_effect_has_tribe(e, tribe))
        })
    })
}

fn token_effect_has_tribe(e: &Effect, tribe: &str) -> bool {
    if let Effect::Token { types, .. } = e {
        return types.iter().any(|s| canonicalize_subtype_name(s) == tribe);
    }
    false
}

/// True if `filter` references the given tribe with controller = You or unset (wildcard).
/// Opponent-scoped references are rejected.
fn filter_references_tribe_you_control(filter: &TargetFilter, tribe: &str) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_references_tribe_you_control(typed, tribe),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| filter_references_tribe_you_control(f, tribe)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|f| filter_references_tribe_you_control(f, tribe)),
        _ => false,
    }
}

fn typed_filter_references_tribe_you_control(typed: &TypedFilter, tribe: &str) -> bool {
    // Reject opponent-scoped filters.
    if matches!(typed.controller, Some(ControllerRef::Opponent)) {
        return false;
    }
    typed
        .type_filters
        .iter()
        .any(|tf| type_filter_has_subtype(tf, tribe))
}

/// True if `filter` references the given tribe's subtype at any nesting level.
fn filter_references_subtype(filter: &TargetFilter, tribe: &str) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .type_filters
            .iter()
            .any(|tf| type_filter_has_subtype(tf, tribe)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(|f| filter_references_subtype(f, tribe))
        }
        _ => false,
    }
}

fn type_filter_has_subtype(tf: &TypeFilter, tribe: &str) -> bool {
    match tf {
        TypeFilter::Subtype(s) => canonicalize_subtype_name(s) == tribe,
        TypeFilter::AnyOf(inner) => inner.iter().any(|t| type_filter_has_subtype(t, tribe)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, PtValue, QuantityExpr,
        StaticDefinition, TargetFilter, TriggerDefinition, TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::statics::StaticMode;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    fn creature_face(name: &str, subtypes: Vec<&str>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    fn entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    fn draw_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )
    }

    fn etb_trigger_for(tribe: &str) -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype(tribe.to_string()))
                    .controller(ControllerRef::You),
            ))
            .destination(Zone::Battlefield)
            .execute(draw_ability())
    }

    fn lord_static_for(tribe: &str) -> StaticDefinition {
        StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype(tribe.to_string()))
                    .controller(ControllerRef::You),
            ))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ])
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert!(feature.dominant_tribe.is_none());
        assert!(feature.tribes.is_empty());
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn detects_single_tribe_membership() {
        let mut elves: Vec<DeckEntry> = Vec::new();
        for i in 0..8u32 {
            elves.push(entry(creature_face(&format!("Elf {i}"), vec!["Elf"]), 4));
        }
        let feature = detect(&elves);
        assert_eq!(feature.dominant_tribe.as_deref(), Some("Elf"));
        assert!(feature.commitment > 0.0);
        let elf_tribe = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        // 8 distinct cards × 4 copies = 32 members
        assert_eq!(elf_tribe.member_count, 32);
    }

    #[test]
    fn detects_lord_via_static_modifications() {
        let mut lord_face = creature_face("Elf Lord", vec!["Elf"]);
        lord_face.static_abilities.push(lord_static_for("Elf"));

        let plain_elf = creature_face("Plain Elf", vec!["Elf"]);
        let deck = vec![entry(lord_face, 4), entry(plain_elf, 4)];

        let feature = detect(&deck);
        let elf_tribe = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert!(elf_tribe.lord_count > 0, "lord_count should be non-zero");
    }

    #[test]
    fn detects_lord_via_grant_trigger() {
        // CR 604.1: lords that grant triggered abilities use GrantTrigger modification.
        let grant_trigger_mod = ContinuousModification::GrantTrigger {
            trigger: Box::new(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .execute(draw_ability()),
            ),
        };
        let mut lord_face = creature_face("Trigger Lord", vec!["Goblin"]);
        lord_face.static_abilities.push(
            StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Subtype("Goblin".to_string()))
                        .controller(ControllerRef::You),
                ))
                .modifications(vec![grant_trigger_mod]),
        );

        let goblin = creature_face("Goblin Warrior", vec!["Goblin"]);
        let deck = vec![entry(lord_face, 2), entry(goblin, 4)];

        let feature = detect(&deck);
        let goblin_tribe = feature
            .tribes
            .iter()
            .find(|t| t.subtype == "Goblin")
            .unwrap();
        assert!(
            goblin_tribe.lord_count > 0,
            "GrantTrigger lord should be detected"
        );
    }

    #[test]
    fn detects_subtype_scoped_etb_trigger() {
        let mut payoff = creature_face("Elf Payoff", vec!["Elf"]);
        payoff.triggers.push(etb_trigger_for("Elf"));

        let plain_elf = creature_face("Plain Elf", vec!["Elf"]);
        let deck = vec![entry(payoff, 4), entry(plain_elf, 4)];

        let feature = detect(&deck);
        let elf_tribe = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert!(
            elf_tribe.etb_payoff_count > 0,
            "ETB payoff should be detected"
        );
    }

    #[test]
    fn detects_cost_reducer() {
        use engine::types::mana::ManaCost;

        let mut reducer = creature_face("Elf Cost Reducer", vec!["Elf"]);
        reducer
            .static_abilities
            .push(StaticDefinition::new(StaticMode::ReduceCost {
                amount: ManaCost::generic(1),
                spell_filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                    "Elf".to_string(),
                )))),
                dynamic_count: None,
            }));

        let plain_elf = creature_face("Plain Elf", vec!["Elf"]);
        let deck = vec![entry(reducer, 2), entry(plain_elf, 4)];

        let feature = detect(&deck);
        let elf_tribe = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert!(
            elf_tribe.cost_reducer_count > 0,
            "cost reducer should be detected"
        );
    }

    #[test]
    fn detects_token_generator() {
        let mut token_gen = creature_face("Goblin Token Gen", vec!["Goblin"]);
        token_gen.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Token {
                name: "Goblin".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Goblin".to_string()],
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
        ));

        let goblin = creature_face("Goblin Warrior", vec!["Goblin"]);
        let deck = vec![entry(token_gen, 4), entry(goblin, 4)];

        let feature = detect(&deck);
        let goblin_tribe = feature
            .tribes
            .iter()
            .find(|t| t.subtype == "Goblin")
            .unwrap();
        assert!(
            goblin_tribe.token_gen_count > 0,
            "token generator should be detected"
        );
    }

    #[test]
    fn multi_subtype_creature_counts_both() {
        // Elf Warrior: counted in both Elf and Warrior tribes.
        let warrior_elf = creature_face("Warrior Elf", vec!["Elf", "Warrior"]);
        let deck = vec![entry(warrior_elf, 4)];

        let feature = detect(&deck);
        let elf = feature.tribes.iter().find(|t| t.subtype == "Elf");
        let warrior = feature.tribes.iter().find(|t| t.subtype == "Warrior");
        assert!(elf.is_some(), "Elf tribe should be detected");
        assert!(warrior.is_some(), "Warrior tribe should be detected");
        assert_eq!(elf.unwrap().member_count, 4);
        assert_eq!(warrior.unwrap().member_count, 4);
    }

    #[test]
    fn changeling_boosts_existing_tribes_only() {
        // Only Elf tribe exists from a real elf. Changeling boosts Elf count
        // but must NOT create new tribes via itself.
        let real_elf = creature_face("Real Elf", vec!["Elf"]);

        let mut changeling = creature_face("Changeling", vec!["Shapeshifter"]);
        changeling.static_abilities.push(
            StaticDefinition::new(StaticMode::Continuous)
                .modifications(vec![ContinuousModification::AddAllCreatureTypes]),
        );

        let deck = vec![entry(real_elf, 4), entry(changeling, 4)];

        let feature = detect(&deck);
        // Elf tribe should be boosted by changeling copies.
        let elf = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert_eq!(elf.member_count, 8, "changeling should boost Elf by 4");
        // No "Changeling" tribe manufactured by the changeling boost pass.
        assert!(
            feature.tribes.iter().all(|t| t.subtype != "Changeling"),
            "Changeling should not appear as a tribe name"
        );
    }

    #[test]
    fn opponent_scope_lord_ignored() {
        let mut face = creature_face("Anti-Lord", vec!["Elf"]);
        face.static_abilities.push(
            StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Subtype("Elf".to_string()))
                        .controller(ControllerRef::Opponent),
                ))
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );

        let plain_elf = creature_face("Plain Elf", vec!["Elf"]);
        let deck = vec![entry(face, 4), entry(plain_elf, 4)];

        let feature = detect(&deck);
        let elf = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert_eq!(elf.lord_count, 0, "opponent-scoped lord must be ignored");
    }

    #[test]
    fn off_tribe_payoff_does_not_inflate_dominant() {
        // Goblin lord should not inflate Elf tribe lord_count.
        let mut goblin_lord = creature_face("Goblin Lord", vec!["Goblin"]);
        goblin_lord.static_abilities.push(lord_static_for("Goblin"));

        let elf = creature_face("Elf", vec!["Elf"]);
        let deck = vec![entry(goblin_lord, 4), entry(elf, 4)];

        let feature = detect(&deck);
        let elf_tribe = feature.tribes.iter().find(|t| t.subtype == "Elf").unwrap();
        assert_eq!(elf_tribe.lord_count, 0, "Goblin lord must not inflate Elf");
    }

    #[test]
    fn dominance_floor_rejects_incidental_tribes() {
        // A single 1-of creature: 0.03 * 1 = 0.03 < DOMINANCE_FLOOR 0.25
        let face = creature_face("Rare Merfolk", vec!["Merfolk"]);
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert!(
            feature.dominant_tribe.is_none(),
            "single 1-of should not pass dominance floor"
        );
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        // Many lord elves — raw > 1.0 must clamp.
        let mut lord_elf = creature_face("Lord Elf", vec!["Elf"]);
        lord_elf.static_abilities.push(lord_static_for("Elf"));
        let deck = vec![entry(lord_elf, 40)];

        let feature = detect(&deck);
        assert!(
            feature.commitment <= 1.0,
            "commitment must clamp to 1.0, got {}",
            feature.commitment
        );
    }

    #[test]
    fn tie_break_is_deterministic() {
        // Two tribes with equal commitment — alphabetically earlier wins.
        // 9 copies × 0.03 = 0.27 > DOMINANCE_FLOOR (0.25) so both tribes
        // pass the floor and the only tie-break is alphabetical order.
        // Input order A: Elf first.
        let deck_a = vec![
            entry(creature_face("Elf", vec!["Elf"]), 9),
            entry(creature_face("Goblin", vec!["Goblin"]), 9),
        ];
        // Input order B: Goblin first.
        let deck_b = vec![
            entry(creature_face("Goblin2", vec!["Goblin"]), 9),
            entry(creature_face("Elf2", vec!["Elf"]), 9),
        ];

        let fa = detect(&deck_a);
        let fb = detect(&deck_b);
        // Both have equal member_count (9) and commitment — Elf < Goblin alphabetically.
        assert_eq!(
            fa.dominant_tribe, fb.dominant_tribe,
            "tie-break must be deterministic regardless of input order"
        );
        assert_eq!(
            fa.dominant_tribe.as_deref(),
            Some("Elf"),
            "alphabetical winner should be Elf"
        );
    }

    #[test]
    fn kindred_card_type_counted() {
        // A card with CoreType::Kindred (without CoreType::Creature) should still
        // contribute to tribe membership. CR 308.
        let kindred_face = CardFace {
            name: "Kindred Spell".to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Kindred, CoreType::Instant],
                subtypes: vec!["Elf".to_string()],
            },
            ..Default::default()
        };

        let deck = vec![entry(kindred_face, 4)];
        let feature = detect(&deck);
        let elf = feature.tribes.iter().find(|t| t.subtype == "Elf");
        assert!(
            elf.is_some(),
            "Kindred card should count toward tribe membership"
        );
        assert_eq!(elf.unwrap().member_count, 4);
    }
}
