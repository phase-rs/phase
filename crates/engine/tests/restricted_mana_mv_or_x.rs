//! Helga, Skittish Seer / Troyan, Gutsy Explorer — "Spend this mana only to cast
//! [creature] spells with mana value N or greater **or** [creature] spells with
//! {X} in their mana costs."
//!
//! CR 106.6 (restricted mana spend) + CR 107.3 (X placeholder in costs) +
//! CR 202.3 (mana value; 202.3e — X contributes 0 to mana value off the stack).
//!
//! These tests drive the runtime spend-eligibility decision two ways:
//!   1. `ManaRestriction::allows_spell` — the single authority every payment site
//!      flows through (`PaymentContext::Spell` → `allows_spell`).
//!   2. `ManaPool::spend_for` with `PaymentContext::Spell` — the real mana-payment
//!      route, proving a restricted unit is consumed for an eligible spell and
//!      withheld for an ineligible one.
//!
//! Revert-proof: the MV-vs-X cases give DIFFERENT allow results. An MV-1 creature
//! spell WITH {X} in its cost is allowed only because the `HasXInCost` disjunct is
//! evaluated; an MV-3 creature spell WITHOUT {X} is rejected. If the
//! `OnlyForSpellMatchingCostCriteria` arm (or `ManaCost::has_x` / `SpellMeta`
//! plumbing) were reverted, the MV-1-with-X assertion flips to `false` and the
//! tests fail. The MV-threshold-only cards would also lose their disjunction.

use engine::types::ability::Comparator;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaCost, ManaCostShard, ManaPool, ManaRestriction, ManaType, ManaUnit, PaymentContext,
    SpellCostCriterion, SpellMeta,
};

/// Build a `SpellMeta` whose `has_x_in_cost` is DERIVED from a real `ManaCost`
/// via `ManaCost::has_x` — exactly as production `build_spell_meta` does — so the
/// test exercises the same X-detection path the engine uses at cast time.
fn spell_meta(types: &[&str], cost: &ManaCost) -> SpellMeta {
    SpellMeta {
        types: types.iter().map(|t| t.to_string()).collect(),
        subtypes: Vec::new(),
        keyword_kinds: Vec::new(),
        cast_from_zone: None,
        mana_value: Some(cost.mana_value()),
        color_count: None,
        has_x_in_cost: cost.has_x(),
    }
}

/// Cost with `generic` generic mana and no X (mana value == generic).
fn generic_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: Vec::new(),
        generic,
    }
}

/// Cost containing an {X} symbol plus `generic` generic mana. Mana value off the
/// stack is `generic` (CR 202.3e — X contributes 0), but `has_x()` is true.
fn x_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X],
        generic,
    }
}

/// Helga, Skittish Seer: creature-narrowed, MV >= 4 OR {X} in cost.
fn helga_restriction() -> ManaRestriction {
    ManaRestriction::OnlyForSpellMatchingCostCriteria {
        spell_type: Some("Creature".to_string()),
        criteria: vec![
            SpellCostCriterion::ManaValue {
                comparator: Comparator::GE,
                value: 4,
            },
            SpellCostCriterion::HasXInCost,
        ],
    }
}

/// Troyan, Gutsy Explorer: any type, MV >= 5 OR {X} in cost.
fn troyan_restriction() -> ManaRestriction {
    ManaRestriction::OnlyForSpellMatchingCostCriteria {
        spell_type: None,
        criteria: vec![
            SpellCostCriterion::ManaValue {
                comparator: Comparator::GE,
                value: 5,
            },
            SpellCostCriterion::HasXInCost,
        ],
    }
}

#[test]
fn has_x_detects_x_symbol_only() {
    // CR 107.3 + CR 202.3e: structural X detection is independent of mana value.
    assert!(!generic_cost(3).has_x());
    assert!(!generic_cost(0).has_x());
    assert!(x_cost(0).has_x());
    assert!(x_cost(2).has_x());
    // An {X} cost still reports its X separately from its (X=0) mana value.
    assert_eq!(x_cost(1).mana_value(), 1);
    assert!(!ManaCost::NoCost.has_x());
}

#[test]
fn helga_allows_mv4_creature() {
    // MV-4 creature satisfies the ManaValue(GE, 4) disjunct.
    let spell = spell_meta(&["Creature"], &generic_cost(4));
    assert!(helga_restriction().allows_spell(&spell));
}

#[test]
fn helga_rejects_mv3_nonx_creature() {
    // MV-3 creature with no {X}: neither disjunct matches.
    let spell = spell_meta(&["Creature"], &generic_cost(3));
    assert!(!helga_restriction().allows_spell(&spell));
}

#[test]
fn helga_allows_mv1_creature_with_x_in_cost() {
    // Load-bearing: MV-1 creature WITH {X} (cost {X}{1}, MV 1 off-stack) is
    // allowed ONLY via the HasXInCost disjunct. Flips to false on revert.
    let cost = x_cost(1);
    assert_eq!(cost.mana_value(), 1, "MV must be below the 4 threshold");
    let spell = spell_meta(&["Creature"], &cost);
    assert!(helga_restriction().allows_spell(&spell));
}

#[test]
fn helga_rejects_high_mv_noncreature() {
    // Type narrowing applies to BOTH disjuncts: a non-creature spell is rejected
    // even at MV 6, and even with {X} in its cost.
    let big_instant = spell_meta(&["Instant"], &generic_cost(6));
    assert!(!helga_restriction().allows_spell(&big_instant));
    let x_instant = spell_meta(&["Instant"], &x_cost(0));
    assert!(!helga_restriction().allows_spell(&x_instant));
}

#[test]
fn troyan_allows_mv5_any_type() {
    // No type narrowing: any MV-5 spell qualifies via ManaValue(GE, 5).
    let instant = spell_meta(&["Instant"], &generic_cost(5));
    assert!(troyan_restriction().allows_spell(&instant));
    let creature = spell_meta(&["Creature"], &generic_cost(5));
    assert!(troyan_restriction().allows_spell(&creature));
}

#[test]
fn troyan_rejects_mv4_nonx() {
    // MV 4 is below Troyan's threshold and has no {X}.
    let spell = spell_meta(&["Sorcery"], &generic_cost(4));
    assert!(!troyan_restriction().allows_spell(&spell));
}

#[test]
fn troyan_allows_mv1_with_x_in_cost() {
    // Load-bearing: low-MV {X} spell of ANY type is allowed via HasXInCost.
    let cost = x_cost(1);
    let spell = spell_meta(&["Sorcery"], &cost);
    assert!(troyan_restriction().allows_spell(&spell));
}

#[test]
fn empty_criteria_never_authorizes() {
    // Defensive: a criteria-less restriction can satisfy no disjunct.
    let restriction = ManaRestriction::OnlyForSpellMatchingCostCriteria {
        spell_type: None,
        criteria: Vec::new(),
    };
    let spell = spell_meta(&["Creature"], &x_cost(9));
    assert!(!restriction.allows_spell(&spell));
}

#[test]
fn restriction_never_allows_activation() {
    // CR 106.6: this is a spell-casting spend restriction; ability activation is
    // never permitted.
    assert!(!helga_restriction().allows_activation(&["Creature".to_string()], &[], None));
    assert!(!troyan_restriction().allows_activation(&["Artifact".to_string()], &[], None));
}

/// Drive the REAL mana-payment route: `ManaPool::spend_for` with
/// `PaymentContext::Spell`. A restricted unit must be consumed for an eligible
/// spell and withheld for an ineligible one.
#[test]
fn spend_for_consumes_restricted_mana_for_eligible_spell() {
    let source = ObjectId(1);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Green,
            source,
            false,
            vec![helga_restriction()],
        ));
        pool
    };

    // Eligible: MV-1 creature WITH {X} in its cost — spent via HasXInCost.
    let eligible_cost = x_cost(1);
    let eligible = spell_meta(&["Creature"], &eligible_cost);
    let mut pool = make_pool();
    let ctx = PaymentContext::Spell(&eligible);
    let spent = pool.spend_for(ManaType::Green, &ctx);
    assert!(
        spent.is_some(),
        "restricted mana must pay an eligible X spell"
    );
    assert_eq!(pool.total(), 0, "the unit must be consumed");

    // Ineligible: MV-3 non-X creature — restricted mana withheld, pool intact.
    let ineligible_cost = generic_cost(3);
    let ineligible = spell_meta(&["Creature"], &ineligible_cost);
    let mut pool = make_pool();
    let ctx = PaymentContext::Spell(&ineligible);
    let spent = pool.spend_for(ManaType::Green, &ctx);
    assert!(
        spent.is_none(),
        "restricted mana must not pay an ineligible spell"
    );
    assert_eq!(pool.total(), 1, "the unit must remain unspent");
}

#[test]
fn spend_for_troyan_distinguishes_mv_and_x() {
    let source = ObjectId(2);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Blue,
            source,
            false,
            vec![troyan_restriction()],
        ));
        pool
    };

    // MV-5 instant: eligible via the ManaValue disjunct.
    let mv5 = spell_meta(&["Instant"], &generic_cost(5));
    let mut pool = make_pool();
    assert!(pool
        .spend_for(ManaType::Blue, &PaymentContext::Spell(&mv5))
        .is_some());

    // MV-2 sorcery with {X}: eligible via the HasXInCost disjunct.
    let x_low = spell_meta(&["Sorcery"], &x_cost(2));
    let mut pool = make_pool();
    assert!(pool
        .spend_for(ManaType::Blue, &PaymentContext::Spell(&x_low))
        .is_some());

    // MV-2 sorcery, no {X}: ineligible — withheld.
    let plain = spell_meta(&["Sorcery"], &generic_cost(2));
    let mut pool = make_pool();
    assert!(pool
        .spend_for(ManaType::Blue, &PaymentContext::Spell(&plain))
        .is_none());
}
