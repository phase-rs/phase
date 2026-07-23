//! Unit tests for `features::graveyard_types` — structural detection +
//! calibration anchors for the delirium / descend / Goyf axis. No
//! `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CardTypeSetSource, Comparator, ContinuousModification,
    ControllerRef, CountScope, Effect, QuantityExpr, QuantityRef, StaticCondition,
    StaticDefinition, TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter, ZoneRef,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::graveyard_types::*;

fn creature(name: &str) -> CardFace {
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

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// CR 205.2a: distinct card types among cards in the controller's graveyard.
fn own_graveyard_types() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::Zone {
                zone: ZoneRef::Graveyard,
                scope: CountScope::Controller,
            },
        },
    }
}

fn opponent_graveyard_types() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::Zone {
                zone: ZoneRef::Graveyard,
                scope: CountScope::Opponents,
            },
        },
    }
}

/// Backwoods Survivalists shape: a static gated on "four or more card types".
fn threshold_payoff(name: &str, threshold: i32, lhs: QuantityExpr) -> CardFace {
    let mut face = creature(name);
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::QuantityComparison {
            lhs,
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: threshold },
        })];
    face
}

/// Autumnal Gloom shape: the delirium clause rides the TRIGGER, not the static.
fn trigger_threshold_payoff(name: &str, threshold: i32) -> CardFace {
    let mut face = creature(name);
    face.triggers = vec![TriggerDefinition::new(TriggerMode::Phase).condition(
        TriggerCondition::QuantityComparison {
            lhs: own_graveyard_types(),
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: threshold },
        },
    )];
    face
}

/// Consuming Blob / Tarmogoyf shape: scales continuously, no threshold.
fn scaling_payoff(name: &str) -> CardFace {
    let mut face = creature(name);
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::SetDynamicPower {
            value: own_graveyard_types(),
        }])];
    face
}

fn self_mill_enabler(name: &str) -> CardFace {
    let mut face = creature(name);
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
    )];
    face
}

#[test]
fn empty_deck_produces_defaults() {
    let feature = detect(&[]);
    assert_eq!(feature.threshold_payoff_count, 0);
    assert_eq!(feature.commitment, 0.0);
    assert!(feature.payoff_names.is_empty());
}

#[test]
fn vanilla_creature_not_registered() {
    let feature = detect(&[entry(creature("Grizzly Bears"), 4)]);
    assert_eq!(feature.threshold_payoff_count, 0);
    assert_eq!(feature.scaling_payoff_count, 0);
    assert_eq!(feature.commitment, 0.0);
}

#[test]
fn detects_static_threshold_payoff() {
    let feature = detect(&[entry(
        threshold_payoff("Backwoods Survivalists", 4, own_graveyard_types()),
        4,
    )]);
    assert_eq!(feature.threshold_payoff_count, 4);
    assert_eq!(feature.highest_threshold, Some(4));
}

#[test]
fn detects_trigger_threshold_payoff() {
    let feature = detect(&[entry(trigger_threshold_payoff("Autumnal Gloom", 4), 4)]);
    assert_eq!(feature.threshold_payoff_count, 4);
}

#[test]
fn detects_scaling_payoff() {
    let feature = detect(&[entry(scaling_payoff("Consuming Blob"), 2)]);
    assert_eq!(feature.scaling_payoff_count, 2);
    assert_eq!(feature.threshold_payoff_count, 0);
}

/// A delirium card must land on the threshold axis only — counting it as a
/// scaling payoff too would double-weight it in the commitment formula.
#[test]
fn threshold_payoff_not_double_counted_as_scaling() {
    let mut face = threshold_payoff("Hybrid", 4, own_graveyard_types());
    face.static_abilities[0].modifications = vec![ContinuousModification::SetDynamicPower {
        value: own_graveyard_types(),
    }];
    let feature = detect(&[entry(face, 4)]);
    assert_eq!(feature.threshold_payoff_count, 4);
    assert_eq!(feature.scaling_payoff_count, 0);
}

/// A card punishing an OPPONENT's diverse graveyard is not a payoff for this
/// deck's own plan.
#[test]
fn opponent_scoped_graveyard_count_ignored() {
    let feature = detect(&[entry(
        threshold_payoff("Punisher", 4, opponent_graveyard_types()),
        4,
    )]);
    assert_eq!(feature.threshold_payoff_count, 0);
}

#[test]
fn descend_eight_tracks_highest_threshold() {
    let deck = vec![
        entry(
            threshold_payoff("Delirium Four", 4, own_graveyard_types()),
            2,
        ),
        entry(
            threshold_payoff("Descend Eight", 8, own_graveyard_types()),
            2,
        ),
    ];
    assert_eq!(detect(&deck).highest_threshold, Some(8));
}

/// A scaling-only deck has NO threshold — `highest_threshold` must stay `None`
/// rather than inventing a four-type ceiling that would make the policy stop
/// rewarding a payoff that keeps scaling.
#[test]
fn scaling_only_deck_has_no_threshold() {
    let feature = detect(&[
        entry(scaling_payoff("Consuming Blob"), 4),
        entry(self_mill_enabler("Stitcher's Supplier"), 4),
    ]);
    assert_eq!(feature.scaling_payoff_count, 4);
    assert_eq!(feature.threshold_payoff_count, 0);
    assert_eq!(feature.highest_threshold, None);
}

// ─── comparator / negation / compound threshold semantics (CR 205.2a) ────────

fn static_threshold(comparator: Comparator, lhs: QuantityExpr, rhs: QuantityExpr) -> CardFace {
    let mut face = creature("Cmp");
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        })];
    face
}

/// `types >= N` and `types > N` are both positive gates; `>` normalizes to the
/// strict boundary (`> 3` needs four types, same as `>= 4`).
#[test]
fn positive_comparators_yield_thresholds() {
    let ge = detect(&[entry(
        static_threshold(
            Comparator::GE,
            own_graveyard_types(),
            QuantityExpr::Fixed { value: 4 },
        ),
        1,
    )]);
    assert_eq!(ge.highest_threshold, Some(4), "types >= 4");

    let gt = detect(&[entry(
        static_threshold(
            Comparator::GT,
            own_graveyard_types(),
            QuantityExpr::Fixed { value: 3 },
        ),
        1,
    )]);
    assert_eq!(gt.highest_threshold, Some(4), "types > 3 ⟺ types >= 4");
}

/// `<`, `<=`, `=`, `!=` reward FEWER or an EXACT number of types — self-mill
/// toward N is not what they want, so they are not threshold payoffs.
#[test]
fn non_positive_comparators_are_not_thresholds() {
    for comparator in [
        Comparator::LT,
        Comparator::LE,
        Comparator::EQ,
        Comparator::NE,
    ] {
        let feature = detect(&[entry(
            static_threshold(
                comparator,
                own_graveyard_types(),
                QuantityExpr::Fixed { value: 4 },
            ),
            1,
        )]);
        assert_eq!(
            feature.threshold_payoff_count, 0,
            "{comparator:?} against the graveyard count is not a delirium gate"
        );
        assert_eq!(feature.highest_threshold, None);
    }
}

/// The mirror orientation `N <= types` / `N < types` reads as the same lower
/// bound once the comparator is flipped across its operands.
#[test]
fn mirrored_orientation_normalizes_to_lower_bound() {
    let le = detect(&[entry(
        static_threshold(
            Comparator::LE,
            QuantityExpr::Fixed { value: 4 },
            own_graveyard_types(),
        ),
        1,
    )]);
    assert_eq!(le.highest_threshold, Some(4), "4 <= types ⟺ types >= 4");

    let lt = detect(&[entry(
        static_threshold(
            Comparator::LT,
            QuantityExpr::Fixed { value: 3 },
            own_graveyard_types(),
        ),
        1,
    )]);
    assert_eq!(lt.highest_threshold, Some(4), "3 < types ⟺ types >= 4");
}

/// CR 205.2a: negating "N or more types" is a "fewer than N" condition — the
/// opposite of a delirium payoff, so it is not counted.
#[test]
fn negated_threshold_is_not_a_payoff() {
    let mut face = creature("Anti-Delirium");
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::Not {
            condition: Box::new(StaticCondition::QuantityComparison {
                lhs: own_graveyard_types(),
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }),
        })];
    let feature = detect(&[entry(face, 1)]);
    assert_eq!(feature.threshold_payoff_count, 0);
    assert_eq!(feature.highest_threshold, None);
}

/// CR 109.3: an `And` gates on every constraint, so a delirium conjunct is
/// mandatory and the highest graveyard threshold present is taken.
#[test]
fn and_takes_the_highest_mandatory_threshold() {
    let mut face = creature("Conjunctive");
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::And {
            conditions: vec![
                StaticCondition::QuantityComparison {
                    lhs: own_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                },
                StaticCondition::QuantityComparison {
                    lhs: own_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 6 },
                },
            ],
        })];
    assert_eq!(detect(&[entry(face, 1)]).highest_threshold, Some(6));
}

/// An `Or` makes a graveyard threshold a mandatory gate ONLY when every branch
/// is a graveyard threshold (then the easiest, minimum branch). A single
/// non-graveyard branch means the payoff can fire without delirium.
#[test]
fn or_is_a_gate_only_when_every_branch_is_graveyard() {
    // Every branch is a graveyard threshold → the minimum is the effective gate.
    let mut all_gy = creature("All Graveyard Or");
    all_gy.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::Or {
            conditions: vec![
                StaticCondition::QuantityComparison {
                    lhs: own_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                },
                StaticCondition::QuantityComparison {
                    lhs: own_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 6 },
                },
            ],
        })];
    assert_eq!(detect(&[entry(all_gy, 1)]).highest_threshold, Some(4));

    // One non-graveyard branch → not a mandatory delirium gate.
    let mut mixed = creature("Mixed Or");
    mixed.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
        .condition(StaticCondition::Or {
            conditions: vec![
                StaticCondition::QuantityComparison {
                    lhs: own_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                },
                StaticCondition::QuantityComparison {
                    lhs: opponent_graveyard_types(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                },
            ],
        })];
    assert_eq!(detect(&[entry(mixed, 1)]).threshold_payoff_count, 0);
}

/// CR 108.3: an all-graveyards payoff must NOT be read as an own-graveyard plan
/// — the policy would count only the AI's own graveyard, a different quantity.
#[test]
fn all_graveyards_scope_is_not_own_graveyard() {
    let all_scope = QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::Zone {
                zone: ZoneRef::Graveyard,
                scope: CountScope::All,
            },
        },
    };
    let feature = detect(&[entry(threshold_payoff("All Graveyards", 4, all_scope), 4)]);
    assert_eq!(feature.threshold_payoff_count, 0);
}

#[test]
fn detects_self_mill_enabler() {
    let feature = detect(&[entry(self_mill_enabler("Stitcher's Supplier"), 4)]);
    assert_eq!(feature.enabler_count, 4);
}

/// Filling an OPPONENT's graveyard does nothing for this deck's threshold.
#[test]
fn opponent_mill_not_an_enabler() {
    let mut face = creature("Opponent Mill");
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ),
            destination: Zone::Graveyard,
        },
    )];
    let feature = detect(&[entry(face, 4)]);
    assert_eq!(feature.enabler_count, 0);
}

#[test]
fn payoff_names_dedup_per_face() {
    let feature = detect(&[entry(
        threshold_payoff("Backwoods Survivalists", 4, own_graveyard_types()),
        4,
    )]);
    assert_eq!(
        feature.payoff_names,
        vec!["Backwoods Survivalists".to_string()]
    );
}

/// Calibration anchor: a Modern delirium shell — 8 threshold payoffs +
/// 2 scaling payoffs + 8 enablers over 37 nonland.
#[test]
fn delirium_shell_hits_calibration_floor() {
    let deck = vec![
        entry(
            threshold_payoff("Backwoods Survivalists", 4, own_graveyard_types()),
            4,
        ),
        entry(threshold_payoff("Grim Flayer", 4, own_graveyard_types()), 4),
        entry(scaling_payoff("Tarmogoyf"), 2),
        entry(self_mill_enabler("Stitcher's Supplier"), 4),
        entry(self_mill_enabler("Thought Scour"), 4),
        entry(creature("Filler"), 19),
    ];
    let feature = detect(&deck);
    assert_eq!(feature.threshold_payoff_count, 8);
    assert_eq!(feature.enabler_count, 8);
    assert!(
        feature.commitment > 0.85,
        "delirium shell must clear 0.85, got {}",
        feature.commitment
    );
}

/// Anti-calibration: an incidental Goyf with no enablers is not this archetype.
#[test]
fn lone_goyf_without_enablers_below_floor() {
    let deck = vec![
        entry(scaling_payoff("Tarmogoyf"), 4),
        entry(creature("Filler"), 33),
    ];
    let feature = detect(&deck);
    assert!(
        feature.commitment < GRAVEYARD_TYPES_FLOOR,
        "a lone Goyf is not a delirium deck, got {}",
        feature.commitment
    );
}

/// Geometric mean: payoffs with zero enablers collapse to 0.0 — the payoff
/// never turns on reliably, so the axis is not this deck's plan.
#[test]
fn payoffs_without_enablers_collapse() {
    let deck = vec![
        entry(
            threshold_payoff("Backwoods Survivalists", 4, own_graveyard_types()),
            8,
        ),
        entry(creature("Filler"), 29),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

/// And the mirror: enablers with no payoff are just self-mill.
#[test]
fn enablers_without_payoffs_collapse() {
    let deck = vec![
        entry(self_mill_enabler("Thought Scour"), 8),
        entry(creature("Filler"), 29),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

#[test]
fn control_deck_below_floor() {
    assert_eq!(
        detect(&[entry(creature("Counterspell"), 37)]).commitment,
        0.0
    );
}
