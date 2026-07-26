//! Unit tests for `features::devotion` — structural detection + calibration
//! anchors for the CR 700.5 pip-density axis. No `#[cfg(test)]` in SOURCE
//! files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, DevotionColors, Effect,
    ManaContribution, ManaProduction, QuantityExpr, QuantityRef, StaticCondition, StaticDefinition,
    TargetFilter, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::triggers::TriggerMode;

use crate::features::devotion::*;

fn card(name: &str, core: CoreType, pips: &[ManaCostShard], generic: u32) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![core],
            subtypes: Vec::new(),
        },
        ..Default::default()
    };
    face.mana_cost = ManaCost::Cost {
        shards: pips.to_vec(),
        generic,
    };
    face
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

fn devotion(colors: &[ManaColor]) -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Devotion {
            colors: DevotionColors::Fixed(colors.to_vec()),
        },
    }
}

/// Erebos shape: a static gated on `Not { DevotionGE { color, threshold } }`,
/// carrying a colored pip in its own cost.
fn god(name: &str, color: ManaColor, threshold: u32, pip: ManaCostShard) -> CardFace {
    let mut face = card(name, CoreType::Enchantment, &[pip], 1);
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::RemoveType {
            core_type: CoreType::Creature,
        }])
        .condition(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE {
                colors: vec![color],
                threshold,
            }),
        })];
    face
}

/// Athreos / Xenagos shape: a two-color god gated on COMBINED devotion to both
/// colors (`DevotionGE { colors: [c1, c2], threshold }`), carrying one pip of
/// each color in its own cost.
fn dual_god(
    name: &str,
    c1: ManaColor,
    c2: ManaColor,
    threshold: u32,
    pips: &[ManaCostShard],
) -> CardFace {
    let mut face = card(name, CoreType::Enchantment, pips, 1);
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::RemoveType {
            core_type: CoreType::Creature,
        }])
        .condition(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE {
                colors: vec![c1, c2],
                threshold,
            }),
        })];
    face
}

/// Gray Merchant shape: an ETB trigger draining life equal to devotion, plus
/// two colored pips in its own cost.
fn drain(name: &str, color: ManaColor, pips: &[ManaCostShard]) -> CardFace {
    let mut face = card(name, CoreType::Creature, pips, 3);
    face.triggers =
        vec![
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: devotion(&[color]),
                    target: Some(TargetFilter::Opponent),
                },
            )),
        ];
    face
}

/// Anax shape: a static setting P/T equal to devotion (no threshold gate).
fn scaling_pt(name: &str, color: ManaColor, pips: &[ManaCostShard]) -> CardFace {
    let mut face = card(name, CoreType::Creature, pips, 1);
    face.static_abilities = vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::SetDynamicPower {
            value: devotion(&[color]),
        }])];
    face
}

/// A vanilla permanent contributing pips but paying off nothing.
fn pip_body(name: &str, pips: &[ManaCostShard]) -> CardFace {
    card(name, CoreType::Creature, pips, 1)
}

const BB: &[ManaCostShard] = &[ManaCostShard::Black, ManaCostShard::Black];

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.payoff_count, 0);
    assert!(f.primary_colors.is_empty());
    assert_eq!(f.commitment, 0.0);
    assert!(f.payoff_names.is_empty());
}

#[test]
fn vanilla_deck_not_registered() {
    let f = detect(&[entry(pip_body("Bear", BB), 4)]);
    // Pips but no payoff → not a devotion deck.
    assert_eq!(f.payoff_count, 0);
    assert!(f.primary_colors.is_empty());
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn detects_god_gate_and_threshold() {
    let f = detect(&[entry(
        god("Erebos", ManaColor::Black, 5, ManaCostShard::Black),
        1,
    )]);
    assert_eq!(f.payoff_count, 1);
    assert_eq!(f.primary_colors, vec![ManaColor::Black]);
    assert_eq!(
        f.gates,
        vec![DevotionGate {
            colors: vec![ManaColor::Black],
            threshold: 5,
        }]
    );
}

#[test]
fn detects_scaling_drain_payoff() {
    let f = detect(&[entry(drain("Gray Merchant", ManaColor::Black, BB), 4)]);
    assert_eq!(f.payoff_count, 4);
    assert_eq!(f.primary_colors, vec![ManaColor::Black]);
    // A drain has no god threshold.
    assert!(f.gates.is_empty());
}

#[test]
fn detects_dynamic_pt_payoff() {
    let f = detect(&[entry(
        scaling_pt("Anax", ManaColor::Red, &[ManaCostShard::Red]),
        4,
    )]);
    assert_eq!(f.payoff_count, 4);
    assert_eq!(f.primary_colors, vec![ManaColor::Red]);
}

/// CR 700.5 counts permanents only — an instant reading devotion is a payoff,
/// but its own pips never contribute to the deck's devotion.
#[test]
fn instant_pips_do_not_count_toward_devotion() {
    let mut spell = card(
        "Aspect of Hydra",
        CoreType::Instant,
        &[ManaCostShard::Green],
        0,
    );
    spell.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: devotion(&[ManaColor::Green]),
            player: TargetFilter::Controller,
        },
    )];
    // Add a green permanent so green is the primary color and has pips.
    let f = detect(&[
        entry(spell, 4),
        entry(pip_body("Green Body", &[ManaCostShard::Green]), 1),
    ]);
    assert_eq!(f.primary_colors, vec![ManaColor::Green]);
    // The 4 instants contribute 0 pips; only the single permanent's green pip counts.
    assert_eq!(f.pip_count, 1);
}

/// An off-color god is not the primary color when the deck's pips are elsewhere.
#[test]
fn primary_color_follows_pip_density_among_payoff_colors() {
    // A black god, but the deck is packed with black pips → black is primary.
    let deck = vec![
        entry(god("Erebos", ManaColor::Black, 5, ManaCostShard::Black), 1),
        entry(drain("Gray Merchant", ManaColor::Black, BB), 4),
        entry(pip_body("Black Body", BB), 10),
    ];
    let f = detect(&deck);
    assert_eq!(f.primary_colors, vec![ManaColor::Black]);
    assert!(
        f.pip_count >= 20,
        "expected heavy black pip count, got {}",
        f.pip_count
    );
}

/// Calibration: a Mono-Black Devotion shell clears the floor comfortably.
#[test]
fn mono_black_devotion_hits_calibration_floor() {
    let deck = vec![
        entry(drain("Gray Merchant", ManaColor::Black, BB), 4),
        entry(god("Erebos", ManaColor::Black, 5, ManaCostShard::Black), 1),
        entry(pip_body("Double Black Body", BB), 16),
        entry(pip_body("Filler", &[ManaCostShard::Black]), 16),
    ];
    let f = detect(&deck);
    assert_eq!(f.primary_colors, vec![ManaColor::Black]);
    assert!(
        f.commitment > 0.85,
        "mono-black devotion must clear 0.85, got {}",
        f.commitment
    );
}

/// Anti-calibration: a single payoff splashed into an otherwise colorless
/// deck is not a devotion deck — both the payoff and pip pillars are thin, so
/// the geometric mean stays below the floor. (Four double-black drains, by
/// contrast, carry eight black pips themselves and legitimately read as
/// committed — the pillars are only thin when the deck genuinely lacks them.)
#[test]
fn lone_payoff_in_offcolor_deck_below_floor() {
    let deck = vec![
        entry(drain("Gray Merchant", ManaColor::Black, BB), 1),
        entry(pip_body("Colorless Body", &[]), 36),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment < DEVOTION_FLOOR,
        "a lone splashed payoff must stay below floor, got {}",
        f.commitment
    );
}

/// Geometric-mean collapse: heavy pips but no payoff is just a mono deck.
#[test]
fn pips_without_payoff_collapse() {
    let deck = vec![entry(pip_body("Black Body", BB), 30)];
    assert_eq!(detect(&deck).commitment, 0.0);
}

/// A Nykthos-style `ChosenColor` payoff makes every color eligible, so the
/// deck's own densest color becomes primary.
#[test]
fn chosen_color_payoff_uses_deck_densest_color() {
    let mut nykthos = card("Nyx Lotus Proxy", CoreType::Artifact, &[], 4);
    // A ChosenColor devotion read via a draw count (stand-in for the ramp shape).
    nykthos.abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Devotion {
                    colors: DevotionColors::ChosenColor,
                },
            },
            target: TargetFilter::Controller,
        },
    )];
    let deck = vec![
        entry(nykthos, 1),
        entry(
            pip_body("Green Body", &[ManaCostShard::Green, ManaCostShard::Green]),
            8,
        ),
        entry(pip_body("Red Body", &[ManaCostShard::Red]), 2),
    ];
    let f = detect(&deck);
    assert_eq!(f.primary_colors, vec![ManaColor::Green]);
}

/// Two gods at DIFFERENT thresholds in the same color must both be retained —
/// keeping only the maximum would hide the lower god from the policy.
#[test]
fn distinct_thresholds_are_all_retained() {
    let deck = vec![
        entry(
            god("Small God", ManaColor::Black, 3, ManaCostShard::Black),
            1,
        ),
        entry(god("Big God", ManaColor::Black, 5, ManaCostShard::Black), 1),
        entry(pip_body("Black Body", BB), 8),
    ];
    let f = detect(&deck);
    assert_eq!(f.primary_colors, vec![ManaColor::Black]);
    assert_eq!(
        f.gates,
        vec![
            DevotionGate {
                colors: vec![ManaColor::Black],
                threshold: 3,
            },
            DevotionGate {
                colors: vec![ManaColor::Black],
                threshold: 5,
            },
        ],
        "both distinct thresholds retained"
    );
}

/// [HIGH] Nykthos-style ramp: a mana ability that produces mana equal to your
/// devotion lives in `Effect::Mana`'s `ManaProduction`, which `count_expr` does
/// not reach — the dedicated mana-production scan must still detect it as a
/// devotion payoff.
#[test]
fn nykthos_mana_production_is_a_payoff() {
    let mut nykthos = card("Nykthos", CoreType::Artifact, &[], 0);
    nykthos.abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::ChosenColor {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Devotion {
                        colors: DevotionColors::ChosenColor,
                    },
                },
                contribution: ManaContribution::Base,
                fixed_alternative: None,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )];
    // A ChosenColor payoff makes every color eligible, so the deck's densest
    // color becomes primary and Nykthos is counted as a payoff.
    let deck = vec![
        entry(nykthos, 1),
        entry(
            pip_body("Green Body", &[ManaCostShard::Green, ManaCostShard::Green]),
            8,
        ),
    ];
    let f = detect(&deck);
    assert_eq!(
        f.payoff_count, 1,
        "Nykthos must register as a devotion payoff"
    );
    assert_eq!(f.primary_colors, vec![ManaColor::Green]);
}

/// [HIGH] Athreos (W+B): a two-color god's gate is against COMBINED devotion to
/// both colors, and the deck's primary demand is the whole W+B set — not either
/// color alone. The gate must retain both colors so the policy can count the
/// combined board devotion, hybrids once (CR 700.5).
#[test]
fn dual_color_god_retains_the_whole_color_set() {
    let deck = vec![
        entry(
            dual_god(
                "Athreos",
                ManaColor::White,
                ManaColor::Black,
                5,
                &[ManaCostShard::White, ManaCostShard::Black],
            ),
            1,
        ),
        entry(
            pip_body("WB Body", &[ManaCostShard::White, ManaCostShard::Black]),
            8,
        ),
    ];
    let f = detect(&deck);
    // WUBRG-normalized: White precedes Black.
    assert_eq!(f.primary_colors, vec![ManaColor::White, ManaColor::Black]);
    assert_eq!(
        f.gates,
        vec![DevotionGate {
            colors: vec![ManaColor::White, ManaColor::Black],
            threshold: 5,
        }],
        "a two-color god's gate keeps both colors"
    );
}

/// Xenagos (R+G): the same combined-set retention for a different color pair,
/// confirming the axis is not White/Black-specific.
#[test]
fn dual_color_god_xenagos_red_green() {
    let deck = vec![
        entry(
            dual_god(
                "Xenagos",
                ManaColor::Red,
                ManaColor::Green,
                5,
                &[ManaCostShard::Red, ManaCostShard::Green],
            ),
            1,
        ),
        entry(
            pip_body("RG Body", &[ManaCostShard::Red, ManaCostShard::Green]),
            8,
        ),
    ];
    let f = detect(&deck);
    // WUBRG-normalized: Red precedes Green.
    assert_eq!(f.primary_colors, vec![ManaColor::Red, ManaColor::Green]);
    assert_eq!(
        f.gates,
        vec![DevotionGate {
            colors: vec![ManaColor::Red, ManaColor::Green],
            threshold: 5,
        }]
    );
}

/// [MED] CR 700.5: a hybrid `{W/B}` symbol contributes exactly ONCE to a W+B
/// candidate set, not once to white and once to black. `pip_count` for the
/// combined primary set must be computed set-wise, not as a sum of per-color
/// buckets (which would double-count every hybrid shard).
#[test]
fn hybrid_pips_count_once_for_a_combined_set() {
    const WB: ManaCostShard = ManaCostShard::WhiteBlack;
    let deck = vec![
        // Athreos makes W+B the demanded (and only) candidate set. Its own cost
        // is two unambiguous pips (one white, one black) → 2 toward {W,B}.
        entry(
            dual_god(
                "Athreos",
                ManaColor::White,
                ManaColor::Black,
                5,
                &[ManaCostShard::White, ManaCostShard::Black],
            ),
            1,
        ),
        // Four pure-hybrid `{W/B}{W/B}` bodies: 2 shards each, each counting once
        // for {W,B} → 2 per copy, 8 total. A per-color-bucket sum would see each
        // shard in BOTH buckets and report 16.
        entry(pip_body("WB Hybrid Body", &[WB, WB]), 4),
    ];
    let f = detect(&deck);
    assert_eq!(f.primary_colors, vec![ManaColor::White, ManaColor::Black]);
    // 2 (god) + 4 × 2 (hybrid bodies, each shard once) = 10. Double-counting
    // hybrids would yield 2 + 4 × 4 = 18.
    assert_eq!(
        f.pip_count, 10,
        "each hybrid shard must count once for the W+B set, got {}",
        f.pip_count
    );
}
