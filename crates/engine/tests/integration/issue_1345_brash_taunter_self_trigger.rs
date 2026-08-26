//! Issue #1345: Brash Taunter must not re-trigger when its outgoing damage
//! hits an opponent (same class as Stuffy Doll / Body of Knowledge #1353).
//!
//! CR 120.3: A self-scoped `DamageReceived` trigger ("~ is dealt damage") must
//! not fire when the triggered ability deals damage to a player.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::triggers::TriggerMode;

const BRASH_TAUNTER_ORACLE: &str = "Indestructible\n\
Whenever this creature is dealt damage, it deals that much damage to target opponent.\n\
{2}{R}, {T}: This creature fights another target creature.";

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
    target: TargetRef,
    amount: u32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

fn brash_taunter_triggers_on_stack(runner: &GameRunner) -> usize {
    runner
        .stack_names()
        .iter()
        .filter(|name| name.contains("Brash Taunter"))
        .count()
}

#[test]
fn brash_taunter_does_not_retrigger_when_damage_hits_opponent() {
    let mut scenario = GameScenario::new();
    let taunter = scenario
        .add_creature_from_oracle(P0, "Brash Taunter", 1, 1, BRASH_TAUNTER_ORACLE)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    let trigger = &runner.state().objects[&taunter].trigger_definitions[0];
    assert_eq!(trigger.definition.mode, TriggerMode::DamageReceived);
    assert_eq!(trigger.definition.valid_card, Some(TargetFilter::SelfRef));

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(taunter), 2),
        &mut events,
    )
    .expect("damage to Brash Taunter resolves");
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        brash_taunter_triggers_on_stack(&runner),
        1,
        "exactly one Brash Taunter trigger should be queued after it is dealt damage"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        brash_taunter_triggers_on_stack(&runner),
        0,
        "Brash Taunter must not re-trigger when its ability deals damage to an opponent (#1345)"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "trigger should deal 2 damage to the target opponent"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack must be empty after the single trigger resolves"
    );
}
