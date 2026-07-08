//! Issue #5246: Panther Habit's damage-prevention shield must scope to the
//! equipped creature through the RUNTIME replacement pipeline — not just parse
//! into the right shape. This is a discriminating engine scenario (per PR #5390
//! review): Panther Habit is attached to one creature; damage is dealt to that
//! equipped creature AND to an unrelated creature. Only the equipped creature's
//! damage is prevented, and it receives +1/+1 counters equal to the prevented
//! amount; the unrelated creature is untouched by the shield.
//!
//! CR 301.5f (Equipment attaches to a creature) + CR 615.1a (prevention shield
//! recipient scope) + CR 615.5 (prevented-amount rider).

use engine::game::effects::deal_damage;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    Effect, FilterProp, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;

// Real Oracle text, verified against `client/public/card-data.json`.
const PANTHER_HABIT: &str = "If equipped creature would be dealt damage, prevent that damage \
and put that many +1/+1 counters on it.\nEquip {2}";

const DAMAGE: i32 = 3;

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
    target: TargetRef,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: DAMAGE },
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
fn panther_habit_prevents_only_equipped_creature_damage_and_adds_counters() {
    let mut scenario = GameScenario::new();
    // Panther Habit installs its shield from Oracle text. Added with toughness 1
    // so it survives build-time SBAs before we convert it into an Equipment.
    let habit = scenario
        .add_creature_from_oracle(P0, "Panther Habit", 0, 1, PANTHER_HABIT)
        .id();
    // Two creatures with enough toughness to survive DAMAGE unkilled, so the
    // post-resolve assertions can read their marked damage.
    let equipped = scenario.add_creature(P0, "Equipped Bear", 2, 5).id();
    let unrelated = scenario.add_creature(P0, "Unrelated Bear", 2, 5).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    // Convert Panther Habit into an Equipment attached to `equipped`. The
    // `EquippedBy` filter resolves off `attached_to` (CR 301.5f), so this is the
    // real runtime attachment signal — no parser shortcut.
    {
        let obj = runner.state_mut().objects.get_mut(&habit).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.attached_to = Some(AttachTarget::Object(equipped));
    }

    // The Oracle-installed shield scopes to the equipped creature, not board-wide.
    assert_eq!(
        runner.state().objects[&habit].replacement_definitions[0].valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy])
        )),
        "Panther Habit's shield must scope to the equipped creature"
    );

    // Damage to the UNRELATED creature is NOT prevented by Panther Habit's shield.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(unrelated)),
        &mut events,
    )
    .expect("damage to unrelated creature resolves");
    assert_eq!(
        runner.state().objects[&unrelated].damage_marked as i32,
        DAMAGE,
        "damage to an unrelated creature must NOT be prevented (shield is per-equipped-creature)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "no prevention event should fire for the unrelated creature"
    );
    assert_eq!(
        runner.state().objects[&unrelated]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        None,
        "unrelated creature must not receive +1/+1 counters"
    );

    // Damage to the EQUIPPED creature IS prevented, and it gains +1/+1 counters
    // equal to the prevented amount (the rider "put that many counters on it").
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(equipped)),
        &mut events,
    )
    .expect("damage to equipped creature resolves");
    let equipped_obj = &runner.state().objects[&equipped];
    assert_eq!(
        equipped_obj.damage_marked, 0,
        "damage to the equipped creature must be prevented"
    );
    assert_eq!(
        equipped_obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(DAMAGE as u32),
        "equipped creature must gain +1/+1 counters equal to the prevented amount"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "prevention event must fire for the equipped creature"
    );
}
