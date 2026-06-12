//! Issue #2888: Unbreathing Horde's subject-first damage-prevention replacement
//! is self-scoped. Damage to players must not be prevented by its shield, while
//! damage to the Horde itself is prevented and removes a +1/+1 counter.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;

const UNBREATHING_HORDE: &str =
    "If Unbreathing Horde would be dealt damage, prevent that damage and remove a +1/+1 counter from it.";

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
    target: TargetRef,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![target],
        source_id,
        P1,
    )
}

#[test]
fn unbreathing_horde_prevents_only_damage_to_itself() {
    let mut scenario = GameScenario::new();
    let horde = scenario
        .add_creature_from_oracle(P0, "Unbreathing Horde", 0, 0, UNBREATHING_HORDE)
        .with_plus_counters(1)
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    assert_eq!(
        runner.state().objects[&horde].replacement_definitions[0].valid_card,
        Some(TargetFilter::SelfRef),
        "Oracle-text installation must keep the self-recipient valid_card gate"
    );

    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Player(P0)),
        &mut events,
    )
    .expect("damage to player resolves");

    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "self-scoped prevention must not prevent damage dealt to players"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { .. })),
        "damage to player must not be prevented by Unbreathing Horde"
    );

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(horde)),
        &mut events,
    )
    .expect("damage to Horde resolves");

    let horde_obj = &runner.state().objects[&horde];
    assert_eq!(
        horde_obj.damage_marked, 0,
        "damage to Unbreathing Horde itself must be prevented"
    );
    assert_eq!(
        horde_obj.counters.get(&CounterType::Plus1Plus1).copied(),
        None,
        "preventing damage must remove the Horde's +1/+1 counter"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { .. })),
        "damage to Unbreathing Horde must emit a prevention event"
    );
}
