//! Unit tests for `features::cost_reduction` — CR 601.2f "spells you cast cost
//! less" detection. No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    Comparator, ControllerRef, FilterProp, QuantityExpr, StaticDefinition, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::statics::{CostModifyMode, StaticMode};

use crate::features::cost_reduction::*;

fn face(name: &str, core: CoreType) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![core],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

fn generic(amount: u32) -> ManaCost {
    ManaCost::Cost {
        shards: Vec::new(),
        generic: amount,
    }
}

/// A CR 601.2f board-wide reducer: "spells you cast cost {amount} less",
/// narrowed by `spell_filter`.
fn reducer(
    name: &str,
    amount: u32,
    mode: CostModifyMode,
    spell_filter: Option<TargetFilter>,
) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode,
        amount: generic(amount),
        spell_filter,
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::You),
        ..Default::default()
    }));
    f.static_abilities = vec![def];
    f
}

/// The Goblin Electromancer shape: instant/sorcery spells you cast cost {1} less.
fn spell_reducer(name: &str) -> CardFace {
    reducer(
        name,
        1,
        CostModifyMode::Reduce,
        Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::AnyOf(vec![
                TypeFilter::Instant,
                TypeFilter::Sorcery,
            ])],
            controller: Some(ControllerRef::You),
            ..Default::default()
        })),
    )
}

/// Filler the reducers above actually discount.
fn discounted_spell(name: &str) -> CardFace {
    face(name, CoreType::Instant)
}

/// Filler no instant/sorcery-scoped reducer discounts.
fn undiscounted_spell(name: &str) -> CardFace {
    face(name, CoreType::Creature)
}

/// A deck of `reducers` copies of `reducer_face` plus `discounted` discounted
/// spells and `other` undiscounted ones, padded to `nonland` nonland cards.
fn deck(reducer_face: CardFace, reducers: u32, discounted: u32, other: u32) -> Vec<DeckEntry> {
    vec![
        entry(reducer_face, reducers),
        entry(discounted_spell("Discounted"), discounted),
        entry(undiscounted_spell("Other"), other),
    ]
}

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.reducer_count, 0);
    assert_eq!(f.total_discount, 0);
    assert_eq!(f.discounted_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn vanilla_creature_not_registered() {
    let f = detect(&[entry(undiscounted_spell("Bear"), 4)]);
    assert_eq!(f.reducer_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn detects_board_wide_reducer() {
    // 4 reducers, 20 discounted instants, 12 other → 36 nonland.
    let f = detect(&deck(spell_reducer("Electromancer"), 4, 20, 12));
    assert_eq!(f.reducer_count, 4);
    assert_eq!(f.total_discount, 4);
    // The reducer itself is a creature, so only the 20 instants are discounted.
    assert_eq!(f.discounted_count, 20);
}

#[test]
fn unfiltered_reducer_discounts_every_nonland_card() {
    // `spell_filter: None` — "spells you cast cost {1} less" (Aang / Medallion
    // shape) admits every nonland card in the deck, itself included.
    let f = detect(&deck(
        reducer("Unfiltered", 1, CostModifyMode::Reduce, None),
        4,
        20,
        12,
    ));
    assert_eq!(f.discounted_count, 36);
}

#[test]
fn discount_magnitude_is_the_generic_component() {
    // CR 118.7a: only the generic component of a cost is reduced.
    let f = detect(&deck(
        reducer("Big", 2, CostModifyMode::Reduce, None),
        3,
        20,
        13,
    ));
    assert_eq!(f.reducer_count, 3);
    assert_eq!(f.total_discount, 6);
}

#[test]
fn raise_mode_does_not_count() {
    // Thalia, Guardian of Thraben taxes — it is not a discount engine.
    let f = detect(&deck(
        reducer("Thalia", 1, CostModifyMode::Raise, None),
        4,
        20,
        12,
    ));
    assert_eq!(f.reducer_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn minimum_mode_does_not_count() {
    // Trinisphere floors a cost (CR 601.2f last step); it discounts nothing.
    let f = detect(&deck(
        reducer("Trinisphere", 3, CostModifyMode::Minimum, None),
        4,
        20,
        12,
    ));
    assert_eq!(f.reducer_count, 0);
}

#[test]
fn self_cost_reduction_does_not_count() {
    // CR 113.6: "this spell costs {1} less" is a property of one card, resolved
    // by `apply_self_spell_cost_modifiers` — never a board-wide engine.
    let mut f_card = face("Affinity Thing", CoreType::Artifact);
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(1),
        spell_filter: None,
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::SelfRef);
    f_card.static_abilities = vec![def];

    let f = detect(&deck(f_card, 4, 20, 12));
    assert_eq!(f.reducer_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn opponent_scoped_reducer_does_not_count() {
    // A discount handed to the other side is not your engine.
    let mut f_card = face("Opponent Helper", CoreType::Enchantment);
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(1),
        spell_filter: None,
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::Opponent),
        ..Default::default()
    }));
    f_card.static_abilities = vec![def];

    let f = detect(&deck(f_card, 4, 20, 12));
    assert_eq!(f.reducer_count, 0);
}

#[test]
fn zero_generic_reduction_does_not_count() {
    // A purely colored `amount` moves no generic cost (CR 118.7a).
    let f = detect(&deck(
        reducer("Colorless Only", 0, CostModifyMode::Reduce, None),
        4,
        20,
        12,
    ));
    assert_eq!(f.reducer_count, 0);
}

#[test]
fn unverifiable_filter_property_yields_no_coverage() {
    // A `properties` predicate needs live game state, so coverage fails OFF
    // rather than claiming a discount the deck may not get.
    let f = detect(&deck(
        reducer(
            "Stateful",
            1,
            CostModifyMode::Reduce,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Instant],
                properties: vec![FilterProp::Tapped],
                ..Default::default()
            })),
        ),
        4,
        20,
        12,
    ));
    assert_eq!(f.reducer_count, 4);
    assert_eq!(f.discounted_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn reducer_matching_nothing_collapses_commitment() {
    // The lone-Semblance-Anvil case: a real reducer whose filter admits nothing
    // this deck plays. Geometric mean collapses on the zero coverage pillar.
    let f = detect(&[
        entry(spell_reducer("Electromancer"), 4),
        entry(undiscounted_spell("Creature"), 32),
    ]);
    assert_eq!(f.reducer_count, 4);
    assert_eq!(f.discounted_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn spells_without_a_reducer_collapse_commitment() {
    let f = detect(&[entry(discounted_spell("Bolt"), 36)]);
    assert_eq!(f.reducer_count, 0);
    assert_eq!(f.discounted_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn izzet_spells_shell_hits_calibration_anchor() {
    // Docstring anchor: 4 two-mana reducers, 20 of 36 nonland discounted → ≈0.61.
    let f = detect(&deck(spell_reducer("Electromancer"), 4, 20, 12));
    assert!(
        f.commitment > 0.58 && f.commitment < 0.64,
        "expected ≈0.61, got {}",
        f.commitment
    );
    assert!(f.commitment >= COST_REDUCTION_FLOOR);
}

#[test]
fn two_reducers_stay_below_floor() {
    // Anti-calibration: two reducers over the same 36 nonland → ≈0.43.
    let f = detect(&deck(spell_reducer("Electromancer"), 2, 20, 14));
    assert!(
        f.commitment < COST_REDUCTION_FLOOR,
        "expected below floor, got {}",
        f.commitment
    );
}

#[test]
fn commitment_is_format_size_neutral() {
    // Same density over a 99-card Commander shell reads the same as over 60.
    let sixty = detect(&deck(spell_reducer("Electromancer"), 4, 20, 12));
    let commander = detect(&deck(spell_reducer("Electromancer"), 7, 35, 21));
    assert!(
        (sixty.commitment - commander.commitment).abs() < 0.03,
        "{} vs {}",
        sixty.commitment,
        commander.commitment
    );
}

#[test]
fn commitment_clamps_to_one() {
    // Every nonland card is both a reducer and discounted by it.
    let f = detect(&[entry(
        reducer("Everything", 3, CostModifyMode::Reduce, None),
        40,
    )]);
    assert!(f.commitment <= 1.0);
    assert!(
        f.commitment > 0.99,
        "expected saturation, got {}",
        f.commitment
    );
}

#[test]
fn lands_are_excluded_from_coverage() {
    // CR 305.1: playing a land is not casting a spell, so lands never count as
    // discounted cards nor toward the nonland denominator.
    let with_lands = detect(&[
        entry(spell_reducer("Electromancer"), 4),
        entry(discounted_spell("Discounted"), 20),
        entry(undiscounted_spell("Other"), 12),
        entry(face("Island", CoreType::Land), 24),
    ]);
    let without_lands = detect(&deck(spell_reducer("Electromancer"), 4, 20, 12));
    assert_eq!(with_lands.commitment, without_lands.commitment);
    assert_eq!(with_lands.discounted_count, 20);
}

#[test]
fn parts_predicate_sums_multiple_reducing_statics() {
    // One face carrying two reducers reports their combined discount.
    let mut f_card = face("Double", CoreType::Artifact);
    let mut first = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(1),
        spell_filter: None,
        dynamic_count: None,
    });
    first.affected = Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::You),
        ..Default::default()
    }));
    let second = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(2),
        spell_filter: None,
        dynamic_count: None,
    });
    f_card.static_abilities = vec![first, second];

    assert_eq!(your_spell_discount_parts(&f_card.static_abilities), 3);
}

#[test]
fn parts_predicate_reports_zero_for_non_reducer() {
    let f_card = undiscounted_spell("Bear");
    assert_eq!(your_spell_discount_parts(&f_card.static_abilities), 0);
}

// ─── review #6743: context-free `TypedFilter.properties` are now honored ──────
//
// Previously any nonempty `properties` list was discarded, so color-, mana-value-
// and keyword-scoped reducers silently discounted nothing.

/// A face with a real printed mana cost, so color and mana-value props resolve.
fn costed_face(name: &str, core: CoreType, shards: Vec<ManaCostShard>, generic: u32) -> CardFace {
    let mut f = face(name, core);
    f.mana_cost = ManaCost::Cost { shards, generic };
    f
}

fn prop_reducer(name: &str, props: Vec<FilterProp>) -> CardFace {
    reducer(
        name,
        1,
        CostModifyMode::Reduce,
        Some(TargetFilter::Typed(TypedFilter {
            properties: props,
            controller: Some(ControllerRef::You),
            ..Default::default()
        })),
    )
}

#[test]
fn color_scoped_reducer_covers_matching_spells() {
    // "White spells you cast cost {1} less" — CR 105.2 printed color.
    let f = detect(&[
        entry(
            prop_reducer(
                "Medallion",
                vec![FilterProp::HasColor {
                    color: ManaColor::White,
                }],
            ),
            4,
        ),
        entry(
            costed_face(
                "White Spell",
                CoreType::Instant,
                vec![ManaCostShard::White],
                1,
            ),
            20,
        ),
        entry(
            costed_face("Red Spell", CoreType::Instant, vec![ManaCostShard::Red], 1),
            12,
        ),
    ]);
    assert_eq!(f.reducer_count, 4);
    // Only the 20 white spells — the reducer itself is colorless here.
    assert_eq!(f.discounted_count, 20);
    assert!(
        f.commitment > 0.0,
        "color-scoped reducer must produce coverage"
    );
}

#[test]
fn mana_value_scoped_reducer_covers_matching_spells() {
    // "Spells you cast with mana value 3 or greater cost {1} less" — CR 202.3.
    let f = detect(&[
        entry(
            prop_reducer(
                "Big Discount",
                vec![FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 3 },
                }],
            ),
            4,
        ),
        entry(costed_face("Expensive", CoreType::Sorcery, vec![], 4), 20),
        entry(costed_face("Cheap", CoreType::Sorcery, vec![], 1), 12),
    ]);
    assert_eq!(f.reducer_count, 4);
    assert_eq!(f.discounted_count, 20);
}

#[test]
fn negated_color_prop_is_honored() {
    let f = detect(&[
        entry(
            prop_reducer(
                "Anti-White",
                vec![FilterProp::NotColor {
                    color: ManaColor::White,
                }],
            ),
            4,
        ),
        entry(
            costed_face(
                "White Spell",
                CoreType::Instant,
                vec![ManaCostShard::White],
                1,
            ),
            20,
        ),
        entry(
            costed_face("Red Spell", CoreType::Instant, vec![ManaCostShard::Red], 1),
            12,
        ),
    ]);
    // The 12 red spells plus the 4 colorless reducers themselves.
    assert_eq!(f.discounted_count, 16);
}

#[test]
fn live_only_property_still_fails_closed() {
    // A property with no context-free reading must NOT be assumed satisfied —
    // it yields no coverage, so commitment collapses rather than over-claiming.
    let f = detect(&[
        entry(prop_reducer("Stateful", vec![FilterProp::Tapped]), 4),
        entry(costed_face("Spell", CoreType::Instant, vec![], 2), 32),
    ]);
    assert_eq!(f.reducer_count, 4);
    assert_eq!(f.discounted_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn controller_scoped_spell_filter_is_admitted_for_own_deck() {
    // Regression for the root defect: a `ControllerRef::You` spell filter — the
    // shape nearly every real reducer uses — must not be rejected outright.
    let f = detect(&deck(spell_reducer("Electromancer"), 4, 20, 12));
    assert_eq!(f.discounted_count, 20);
}
