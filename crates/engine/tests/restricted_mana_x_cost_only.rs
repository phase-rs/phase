//! Rosheen Meanderer / Elementalist's Palette / Nexos — "Spend this mana only
//! on costs that contain {X}."
//!
//! CR 106.6 (restricted mana spend) + CR 107.3 ({X} placeholder in costs) +
//! CR 202.3e (X contributes 0 to mana value off the stack).
//!
//! These cards previously surfaced as honest coverage red because
//! `ManaSpendRestriction::XCostOnly` was dead in `is_coverage_supported()` and
//! `ManaRestriction::OnlyForXCosts` always rejected spell spends. The PR wires
//! the direct restriction to `SpellMeta.has_x_in_cost` (derived from the cast
//! cost via `ManaCost::has_x`, same as `build_spell_meta`).
//!
//! This file exercises the **direct** `OnlyForXCosts` path. It is distinct from
//! `restricted_mana_mv_or_x.rs`, which covers Helga/Troyan's
//! `OnlyForSpellMatchingCostCriteria { HasXInCost, ManaValue(GE, …) }`
//! disjunction — a different restriction shape that happens to mention {X}.
//!
//! Revert-proof: an MV-2 sorcery WITH {X} is allowed only because
//! `meta.has_x_in_cost` is true; an MV-2 sorcery WITHOUT {X} is rejected. If
//! `OnlyForXCosts => meta.has_x_in_cost` is reverted to `false`, the allowed
//! case flips and these tests fail.

use engine::types::ability::ManaSpendRestriction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaCost, ManaCostShard, ManaPool, ManaRestriction, ManaType, ManaUnit, PaymentContext,
    SpellMeta,
};

const X_COST_ONLY_RESTRICTION: ManaRestriction = ManaRestriction::OnlyForXCosts;

fn spell_meta(types: &[&str], cost: &ManaCost) -> SpellMeta {
    SpellMeta {
        types: types.iter().map(|t| t.to_string()).collect(),
        subtypes: Vec::new(),
        keyword_kinds: Vec::new(),
        cast_from_zone: None,
        mana_value: Some(cost.mana_value()),
        color_count: None,
        has_x_in_cost: cost.has_x(),
        is_face_down: false,
    }
}

fn generic_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: Vec::new(),
        generic,
    }
}

fn x_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X],
        generic,
    }
}

#[test]
fn x_cost_only_is_coverage_supported() {
    // Rosheen Meanderer, Elementalist's Palette, Nexos, Rosheen, Roaring Prophet
    // parse to XCostOnly (see oracle.rs mana_spend_restriction_x_cost_only).
    assert!(
        ManaSpendRestriction::XCostOnly.is_coverage_supported(),
        "XCostOnly must be live once OnlyForXCosts gates on has_x_in_cost"
    );
}

#[test]
fn only_for_x_costs_allows_spell_with_x_rejects_comparable_non_x() {
    // Load-bearing pair: same spell type, comparable off-stack mana value, only
    // the {X} symbol distinguishes eligibility.
    let x_spell = spell_meta(&["Sorcery"], &x_cost(1));
    assert_eq!(x_spell.mana_value, Some(1));
    assert!(x_spell.has_x_in_cost);

    let non_x_spell = spell_meta(&["Sorcery"], &generic_cost(2));
    assert_eq!(non_x_spell.mana_value, Some(2));
    assert!(!non_x_spell.has_x_in_cost);

    assert!(X_COST_ONLY_RESTRICTION.allows_spell(&x_spell));
    assert!(!X_COST_ONLY_RESTRICTION.allows_spell(&non_x_spell));
}

#[test]
fn only_for_x_costs_rejects_no_cost_spell() {
    let spell = spell_meta(&["Instant"], &ManaCost::NoCost);
    assert!(!spell.has_x_in_cost);
    assert!(!X_COST_ONLY_RESTRICTION.allows_spell(&spell));
}

#[test]
fn only_for_x_costs_never_allows_activation() {
    // Ability payments do not yet thread activation-cost X detection through
    // PaymentContext::Activation; the spell half is what this PR makes live.
    assert!(!X_COST_ONLY_RESTRICTION.allows_activation(&["Creature".to_string()], &[], None,));
}

#[test]
fn spend_for_consumes_restricted_mana_only_for_x_spell() {
    let source = ObjectId(42);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            source,
            false,
            vec![X_COST_ONLY_RESTRICTION],
        ));
        pool
    };

    // Rosheen-style: pay an {X} spell (e.g. Fireball class).
    let eligible = spell_meta(&["Sorcery"], &x_cost(0));
    let mut pool = make_pool();
    assert!(
        pool.spend_for(ManaType::Colorless, &PaymentContext::Spell(&eligible))
            .is_some(),
        "restricted mana must pay an X-cost spell"
    );
    assert_eq!(pool.total(), 0);

    // Comparable non-{X} spell: withheld.
    let ineligible = spell_meta(&["Sorcery"], &generic_cost(3));
    let mut pool = make_pool();
    assert!(
        pool.spend_for(ManaType::Colorless, &PaymentContext::Spell(&ineligible))
            .is_none(),
        "restricted mana must not pay a non-X-cost spell"
    );
    assert_eq!(pool.total(), 1);
}
