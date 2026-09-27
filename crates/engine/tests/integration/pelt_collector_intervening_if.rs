//! Pelt Collector — "Whenever another creature you control enters or dies, if
//! that creature's power is greater than this creature's, put a +1/+1 counter on
//! this creature."
//!
//! The intervening-if (CR 603.4) used to be swallowed on BOTH split halves, so
//! the counter landed on every entering or dying creature regardless of power.
//! The enters half compares the entrant's live power (CR 603.6a); the dies half
//! compares the dying creature's last battlefield power (CR 603.10a), both
//! against Pelt Collector's current power.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    Comparator, FilterProp, ObjectScope, PtStat, PtValueScope, QuantityExpr, QuantityRef,
    TargetFilter, TriggerCondition,
};
use engine::types::counter::CounterType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PELT_COLLECTOR: &str = "Whenever another creature you control enters or dies, if that \
    creature's power is greater than this creature's, put a +1/+1 counter on this creature.\n\
    As long as this creature has three or more +1/+1 counters on it, it has trample.";

const DESTROY: &str = "Destroy target creature.";

fn power_gt_source() -> FilterProp {
    FilterProp::PtComparison {
        stat: PtStat::Power,
        scope: PtValueScope::Current,
        comparator: Comparator::GT,
        value: QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Source,
            },
        },
    }
}

#[test]
fn pelt_collector_both_halves_carry_the_power_condition() {
    let parsed = parse_oracle_text(
        PELT_COLLECTOR,
        "Pelt Collector",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 2, "enters and dies halves");
    let mut shapes = Vec::new();
    for trigger in &parsed.triggers {
        match trigger.condition.as_ref() {
            Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin,
                destination,
                filter: TargetFilter::Typed(typed),
            }) => {
                assert_eq!(typed.properties, vec![power_gt_source()]);
                shapes.push((*origin, *destination));
            }
            other => panic!("expected power-vs-source condition, got {other:?}"),
        }
    }
    shapes.sort_by_key(|(_, d)| format!("{d:?}"));
    assert_eq!(
        shapes,
        vec![
            (None, Zone::Battlefield),
            (Some(Zone::Battlefield), Zone::Graveyard),
        ]
    );
    assert!(
        !parsed.parse_warnings.iter().any(|w| matches!(
            w,
            OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
        )),
        "the intervening-if must no longer be swallowed"
    );
}

/// Review of phase-rs#9240: the possessive comparison must follow the proven
/// trigger head. On a non-zone head the entry-only condition could never match
/// the tap event (it only accepts `ZoneChanged`), so the trigger would never
/// fire; the clause stays unhoisted instead. Paired with the enters/dies
/// positive shape above, so the negative is not vacuous.
#[test]
fn a_non_zone_head_does_not_get_an_entry_condition() {
    let parsed = parse_oracle_text(
        "Whenever a creature you control becomes tapped, if that creature's power is \
         greater than this creature's, put a +1/+1 counter on this creature.",
        "Tapped Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        1,
        "reach guard: the tap trigger parses"
    );
    assert!(
        !matches!(
            parsed.triggers[0].condition,
            Some(TriggerCondition::ZoneChangeObjectMatchesFilter { .. })
        ),
        "a becomes-tapped head must not carry a zone-change condition, got {:?}",
        parsed.triggers[0].condition
    );
}

#[test]
fn trailing_power_comparison_is_not_an_intervening_if() {
    let leading = parse_oracle_text(
        "Whenever another creature you control enters, if that creature's power is \
         greater than this creature's, put a +1/+1 counter on this creature.",
        "Comparison Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(leading.triggers.len(), 1);
    assert!(matches!(
        leading.triggers[0].condition,
        Some(TriggerCondition::ZoneChangeObjectMatchesFilter { .. })
    ));

    let trailing = parse_oracle_text(
        "Whenever another creature you control enters, put a +1/+1 counter on this \
         creature if that creature's power is greater than this creature's.",
        "Comparison Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(
        trailing.triggers.len(),
        1,
        "reach guard: entry trigger parses"
    );
    assert!(
        trailing.triggers[0].condition.is_none(),
        "trailing comparison must remain an effect instruction"
    );
}

fn enters(power: i32) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pelt = scenario
        .add_creature_from_oracle(P0, "Pelt Collector", 1, 1, PELT_COLLECTOR)
        .id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Newcomer", power, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let out = runner.cast(entrant).resolve();
    out.counters(pelt, CounterType::Plus1Plus1)
}

#[test]
fn bigger_entrant_grants_counter() {
    assert_eq!(enters(2), 1, "2 > 1 → +1/+1");
}

#[test]
fn equal_or_smaller_entrant_grants_nothing() {
    assert_eq!(enters(1), 0, "1 is not greater than 1");
    assert_eq!(enters(0), 0, "0 is not greater than 1");
}

fn dies(power: i32) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pelt = scenario
        .add_creature_from_oracle(P0, "Pelt Collector", 1, 1, PELT_COLLECTOR)
        .id();
    let victim = scenario.add_creature(P0, "Victim", power, 3).id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    out.counters(pelt, CounterType::Plus1Plus1)
}

#[test]
fn bigger_creature_dying_grants_counter() {
    assert_eq!(dies(3), 1, "3 > 1 → +1/+1");
}

#[test]
fn equal_or_smaller_creature_dying_grants_nothing() {
    assert_eq!(dies(1), 0, "1 is not greater than 1");
}
