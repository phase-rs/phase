//! MSH Wave 3 — Wolverine, Fierce Fighter: heal-prior-damage replacement.
//!
//! Oracle: "If damage would be dealt to Wolverine, instead that damage is dealt,
//! but all other damage already dealt to him is healed." (CR 614.1a replacement;
//! CR 120.6 marked damage; CR 510.2 combat-damage simultaneity.)
//!
//! The new damage instance IS dealt (no prevention); only the receiver's PRIOR
//! marked damage clears. The heal runs in Phase B (`dealt_damage_applier`,
//! before the Phase-C `damage_marked` increment), so same-batch combat
//! instances (CR 510.2) are preserved while only pre-batch damage heals.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::rules::run_combat;

const WOLVERINE_HEAL_LINE: &str = "If damage would be dealt to Wolverine, instead that damage is \
dealt, but all other damage already dealt to him is healed.";

fn damage_ability(source_id: ObjectId, target: ObjectId, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![TargetRef::Object(target)],
        source_id,
        P1,
    )
}

/// Noncombat: a SEPARATE damage instance heals the PRIOR marked damage but the
/// new instance is fully dealt. After two distinct 2-damage instances, exactly
/// the second remains (marked == 2), and Wolverine is alive.
/// Revert-fail: without the heal, marked == 4.
#[test]
fn wolverine_noncombat_separate_instance_heals_prior_damage() {
    let mut scenario = GameScenario::new();
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Wolverine, Fierce Fighter", 3, 5, WOLVERINE_HEAL_LINE)
        .id();
    let source = scenario.add_creature(P1, "Pinger", 1, 1).id();
    let mut runner = scenario.build();

    // First instance: 2 damage → marked 2.
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, wolverine, 2),
        &mut events,
    )
    .expect("first damage resolves");
    assert_eq!(
        runner.state().objects[&wolverine].damage_marked,
        2,
        "first instance marks 2"
    );

    // Second, separate instance: 2 damage → prior 2 heals, new 2 marked.
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, wolverine, 2),
        &mut events,
    )
    .expect("second damage resolves");
    assert_eq!(
        runner.state().objects[&wolverine].damage_marked,
        2,
        "CR 120.6: prior 2 healed, only the new 2 remains (not 4)"
    );
    assert_eq!(
        runner.state().objects[&wolverine].zone,
        Zone::Battlefield,
        "Wolverine survives two separate 2-damage instances"
    );
}

/// Discriminator: a single instance large enough to be lethal is NOT prevented —
/// the new damage is dealt in full (marked == 5 ≥ toughness). Proves the
/// replacement heals prior damage rather than shielding the incoming hit.
#[test]
fn wolverine_single_large_instance_is_not_prevented() {
    let mut scenario = GameScenario::new();
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Wolverine, Fierce Fighter", 3, 5, WOLVERINE_HEAL_LINE)
        .id();
    let source = scenario.add_creature(P1, "Pinger", 1, 1).id();
    let mut runner = scenario.build();

    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, wolverine, 5),
        &mut events,
    )
    .expect("damage resolves");
    assert_eq!(
        runner.state().objects[&wolverine].damage_marked,
        5,
        "the new instance is dealt in full (5), not prevented — lethal vs toughness 5"
    );
}

/// COMBAT-BATCH simultaneity (CR 510.2): Wolverine 3/5 with pre-batch marked = 1
/// is gang-blocked by two 2-power blockers in ONE combat damage step. The heal
/// runs in Phase B (clearing only the pre-batch 1) before Phase C marks both
/// same-batch instances. Result: marked == 4 (pre-batch 1 healed; 2 + 2
/// preserved), Wolverine alive.
/// Revert-fail: if the heal interleaved with delivery, marked would be 2.
#[test]
fn wolverine_combat_batch_preserves_same_batch_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Wolverine, Fierce Fighter", 3, 5, WOLVERINE_HEAL_LINE)
        .with_damage_marked(1)
        .id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let mut runner = scenario.build();

    assert_eq!(
        runner.state().objects[&wolverine].damage_marked,
        1,
        "precondition: pre-batch marked damage is 1"
    );

    run_combat(
        &mut runner,
        vec![wolverine],
        vec![(blocker_a, wolverine), (blocker_b, wolverine)],
    );

    let wolverine_obj = &runner.state().objects[&wolverine];
    assert_eq!(
        wolverine_obj.damage_marked, 4,
        "CR 510.2: pre-batch 1 healed, both same-batch combat instances (2+2) preserved"
    );
    assert_eq!(
        wolverine_obj.zone,
        Zone::Battlefield,
        "Wolverine (toughness 5) survives 4 marked damage"
    );
}

/// Parser unit: the heal line parses to a DealtDamage replacement carrying
/// Effect::RemoveAllDamage in `execute`, with no prevention shield.
#[test]
fn wolverine_heal_line_parses_remove_all_damage_replacement() {
    use engine::parser::parse_oracle_text;

    let parsed = parse_oracle_text(
        WOLVERINE_HEAL_LINE,
        "Wolverine, Fierce Fighter",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let repl = parsed
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::DealtDamage)
        .expect("expected a DealtDamage replacement");
    let execute = repl.execute.as_deref().expect("execute payload");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::RemoveAllDamage {
                target: TargetFilter::SelfRef
            }
        ),
        "execute must be RemoveAllDamage on self, got {:?}",
        execute.effect
    );
    assert!(
        repl.shield_kind.is_none(),
        "Wolverine's heal must NOT install a prevention/shield (damage IS dealt)"
    );
}
