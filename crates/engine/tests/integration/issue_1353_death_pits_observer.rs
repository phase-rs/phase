//! Issue #1353 class — observer-style DamageReceived triggers must fire when a
//! filtered object is dealt damage, not only when the trigger source itself is
//! damaged. Death Pits of Rath: "Whenever a creature is dealt damage, destroy it."

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const DEATH_PITS_ORACLE: &str =
    "Whenever a creature is dealt damage, destroy it. It can't be regenerated.";

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
    target: TargetRef,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

#[test]
fn death_pits_destroys_creature_damaged_while_observer_unharmed() {
    let mut scenario = GameScenario::new();
    // Nonzero toughness so the observer survives setup (Death Pits is an enchantment
    // on the real card; the harness needs a creature shell with legal stats).
    let death_pits = scenario
        .add_creature_from_oracle(P0, "Death Pits of Rath", 1, 3, DEATH_PITS_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Victim", 2, 2).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();

    let mut runner = scenario.build();

    let trigger = &runner.state().objects[&death_pits].trigger_definitions[0];
    assert_eq!(trigger.mode, TriggerMode::DamageReceived);
    assert!(
        matches!(trigger.valid_card, Some(TargetFilter::Typed(_))),
        "Death Pits must watch creatures, not SelfRef, got {:?}",
        trigger.valid_card
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(victim)),
        &mut events,
    )
    .expect("damage to victim resolves");
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Death Pits must trigger when another creature is dealt damage"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "Death Pits must destroy the creature that was dealt damage"
    );
    assert_eq!(
        runner.state().objects[&death_pits].zone,
        Zone::Battlefield,
        "Death Pits itself must remain on the battlefield"
    );
}
