//! Unit tests for `features::energy` — structural detection + calibration
//! anchors for the producer × sink energy economy. No `#[cfg(test)]` in SOURCE
//! files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::energy::{
    ability_tree_pays_energy, detect, effect_is_energy_gain, is_energy_producer, is_energy_sink,
    COMMITMENT_FLOOR,
};

fn face(core: Vec<CoreType>) -> CardFace {
    CardFace {
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: core,
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn land_face() -> CardFace {
    face(vec![CoreType::Land])
}

/// A sorcery that grants energy (Attune with Aether shape) — producer only.
fn producer_spell_face() -> CardFace {
    let mut f = face(vec![CoreType::Sorcery]);
    f.abilities.push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        },
    ));
    f
}

/// A creature whose activation pays energy (Bristling Hydra activation shape)
/// — sink only. The effect is a non-energy effect so the face is not also a
/// producer.
fn sink_creature_face() -> CardFace {
    let mut f = face(vec![CoreType::Creature]);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::PayEnergy {
        amount: QuantityExpr::Fixed { value: 1 },
    });
    f.abilities.push(ability);
    f
}

/// A creature that both grants energy on a trigger and pays it on an activated
/// ability (Longtusk Cub shape) — the true energy-engine payoff body.
fn payoff_creature_face() -> CardFace {
    let mut f = face(vec![CoreType::Creature]);
    // Activated sink.
    let mut sink = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    sink.cost = Some(AbilityCost::PayEnergy {
        amount: QuantityExpr::Fixed { value: 2 },
    });
    f.abilities.push(sink);
    // Triggered producer.
    let producer = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        },
    );
    f.triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(producer));
    f
}

/// A Rogue Refiner shape: the ETB trigger's execute chain grants energy only
/// via a sub-ability (Draw → sub GainEnergy). A flat single-level walk misses
/// this; `collect_chain_effects` must descend the sub-ability chain.
fn triggered_sub_chain_producer_face() -> CardFace {
    let mut f = face(vec![CoreType::Creature]);
    let mut execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    execute.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        },
    )));
    f.triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(execute));
    f
}

/// An Aether Chaser shape: the attack trigger's execute chain pays energy.
fn triggered_energy_sink_face() -> CardFace {
    let mut f = face(vec![CoreType::Creature]);
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
    );
    f.triggers
        .push(TriggerDefinition::new(TriggerMode::Attacks).execute(execute));
    f
}

/// A Harnessed Lightning shape: the spell grants energy, then pays any amount
/// of energy during resolution.
fn spell_resolution_energy_sink_face() -> CardFace {
    let mut f = producer_spell_face();
    f.abilities[0].sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Ref {
                    qty: engine::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
    )));
    f
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// A generic non-energy, non-land spell — dilutes the density denominator so a
/// splash test reflects a realistic 36-nonland deck rather than a tiny all-energy
/// list (which `density_per_60` would normalize to saturation).
fn filler_face() -> CardFace {
    let mut f = face(vec![CoreType::Sorcery]);
    f.abilities.push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    f
}

// ─── effect_is_energy_gain ─────────────────────────────────────────────────

#[test]
fn gain_energy_effect_is_energy_gain() {
    let e = Effect::GainEnergy {
        amount: QuantityExpr::Fixed { value: 3 },
    };
    assert!(effect_is_energy_gain(&e));
}

#[test]
fn mill_effect_is_not_energy_gain() {
    let e = Effect::Mill {
        count: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Player,
        destination: Zone::Graveyard,
    };
    assert!(!effect_is_energy_gain(&e));
}

// ─── is_energy_producer / is_energy_sink ───────────────────────────────────

#[test]
fn gain_energy_spell_is_producer() {
    assert!(is_energy_producer(&producer_spell_face()));
    assert!(!is_energy_sink(&producer_spell_face()));
}

#[test]
fn pay_energy_creature_is_sink() {
    assert!(is_energy_sink(&sink_creature_face()));
    assert!(!is_energy_producer(&sink_creature_face()));
}

#[test]
fn payoff_creature_is_both() {
    let f = payoff_creature_face();
    assert!(is_energy_producer(&f));
    assert!(is_energy_sink(&f));
}

/// The defining correctness case: energy granted only via a trigger's execute
/// sub-ability must still register as a producer.
#[test]
fn triggered_sub_chain_grants_energy_is_producer() {
    assert!(is_energy_producer(&triggered_sub_chain_producer_face()));
}

#[test]
fn triggered_pay_energy_execute_is_sink() {
    assert!(is_energy_sink(&triggered_energy_sink_face()));
}

#[test]
fn spell_resolution_pay_energy_effect_is_sink() {
    let f = spell_resolution_energy_sink_face();
    assert!(is_energy_producer(&f));
    assert!(is_energy_sink(&f));
}

#[test]
fn non_energy_spell_is_neither() {
    let mut f = face(vec![CoreType::Instant]);
    f.abilities.push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    ));
    assert!(!is_energy_producer(&f));
    assert!(!is_energy_sink(&f));
}

#[test]
fn ability_tree_pays_energy_detects_top_level_cost() {
    let f = sink_creature_face();
    assert!(f.abilities.iter().any(ability_tree_pays_energy));
}

// ─── detect + calibration ──────────────────────────────────────────────────

#[test]
fn empty_deck_produces_zero_commitment() {
    let f = detect(&[]);
    assert_eq!(f.producer_count, 0);
    assert_eq!(f.sink_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn all_lands_deck_produces_zero_commitment() {
    let deck: Vec<DeckEntry> = (0..24).map(|_| entry(land_face(), 1)).collect();
    let f = detect(&deck);
    assert_eq!(f.commitment, 0.0);
}

/// Calibration anchor — a dedicated energy engine (producers + sinks in density)
/// must clear `COMMITMENT_FLOOR` so `EnergyPayoffPolicy` activates.
#[test]
fn dedicated_energy_deck_clears_commitment_floor() {
    let deck = vec![
        entry(producer_spell_face(), 18),
        entry(sink_creature_face(), 18),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "dedicated energy deck must clear COMMITMENT_FLOOR {COMMITMENT_FLOOR}; got {}",
        f.commitment
    );
    assert!(f.producer_count >= 1);
    assert!(f.sink_count >= 1);
}

/// Anti-calibration — a producers-only deck (Attune-for-fixing with no sinks)
/// scores zero: energy is a two-part economy, and the geometric mean collapses
/// without the sink side.
#[test]
fn producers_only_deck_has_zero_commitment() {
    let deck = vec![entry(producer_spell_face(), 24), entry(land_face(), 12)];
    let f = detect(&deck);
    assert!(f.producer_count > 0);
    assert_eq!(f.sink_count, 0, "no sinks in a producers-only deck");
    assert_eq!(
        f.commitment, 0.0,
        "producers-only must not register as energy-committed"
    );
}

/// Anti-calibration — a sinks-only deck (nothing to spend) also scores zero.
#[test]
fn sinks_only_deck_has_zero_commitment() {
    let deck = vec![entry(sink_creature_face(), 24), entry(land_face(), 12)];
    let f = detect(&deck);
    assert!(f.sink_count > 0);
    assert_eq!(f.producer_count, 0);
    assert_eq!(f.commitment, 0.0);
}

/// Anti-calibration — a light splash (4 producers + 2 sinks / 36 nonland)
/// stays below the floor.
#[test]
fn splash_energy_stays_below_floor() {
    // 4 producers + 2 sinks + 30 filler = 36 nonland → density 6.7 / 3.3 per 60
    // → normalized 0.22 × 0.17 → commitment ≈ 0.19.
    let deck = vec![
        entry(producer_spell_face(), 4),
        entry(sink_creature_face(), 2),
        entry(filler_face(), 30),
        entry(land_face(), 24),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "splash energy (4 prod / 2 sink / 36 nonland) must stay below \
         COMMITMENT_FLOOR {COMMITMENT_FLOOR}; got {}",
        f.commitment
    );
}

/// Commitment clamps at 1.0 even with extreme producer × sink density.
#[test]
fn commitment_clamps_at_one() {
    let deck = vec![entry(payoff_creature_face(), 36), entry(land_face(), 24)];
    let f = detect(&deck);
    assert_eq!(f.commitment, 1.0);
}

/// `payoff_count` counts faces that are both a producer and a sink.
#[test]
fn payoff_count_counts_both_faces() {
    let deck = vec![
        entry(payoff_creature_face(), 4),
        entry(producer_spell_face(), 4),
        entry(land_face(), 8),
    ];
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 4, "only the payoff creatures are both");
    assert_eq!(f.producer_count, 8);
    assert_eq!(f.sink_count, 4);
}
