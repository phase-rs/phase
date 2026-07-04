//! Issue #1353: Body of Knowledge's "Whenever this creature is dealt damage,
//! draw that many cards" must fire only when Body of Knowledge itself is dealt
//! damage — not whenever any permanent or player takes damage.
//!
//! CR 603.2 + CR 120.3: A self-scoped damage-received trigger watches the
//! trigger source object as the damage recipient.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

fn hand_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

const BODY_OF_KNOWLEDGE_ORACLE: &str = "\
Body of Knowledge's power and toughness are each equal to the number of cards in your hand.\n\
You have no maximum hand size.\n\
Whenever this creature is dealt damage, draw that many cards.";

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

#[test]
fn body_of_knowledge_draws_only_when_itself_is_dealt_damage() {
    let mut scenario = GameScenario::new();
    scenario.with_library_top(
        P0,
        &[
            "Library Card 1",
            "Library Card 2",
            "Library Card 3",
            "Library Card 4",
            "Library Card 5",
            "Library Card 6",
            "Library Card 7",
            "Library Card 8",
            "Library Card 9",
            "Library Card 10",
            "Library Card 11",
            "Library Card 12",
            "Library Card 13",
            "Library Card 14",
            "Library Card 15",
            "Library Card 16",
            "Library Card 17",
            "Library Card 18",
            "Library Card 19",
            "Library Card 20",
        ],
    );

    let body = scenario
        .add_creature_from_oracle(P0, "Body of Knowledge", 0, 0, BODY_OF_KNOWLEDGE_ORACLE)
        .id();
    let other = scenario.add_creature(P0, "Other Creature", 2, 2).id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();

    let mut runner = scenario.build();

    let trigger = &runner.state().objects[&body].trigger_definitions[0];
    assert_eq!(
        trigger.mode,
        TriggerMode::DamageReceived,
        "precondition: Body of Knowledge installs a DamageReceived trigger"
    );
    assert_eq!(
        trigger.valid_card,
        Some(TargetFilter::SelfRef),
        "precondition: the trigger must be self-scoped"
    );

    let hand_before = hand_count(&runner, P0);

    // Damage to an unrelated creature must not queue Body of Knowledge's draw.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(other), 2),
        &mut events,
    )
    .expect("damage to other creature resolves");
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        hand_count(&runner, P0),
        hand_before,
        "damage to another creature must not draw cards via Body of Knowledge"
    );
    assert!(
        !runner
            .stack_names()
            .iter()
            .any(|name| name.contains("Body of Knowledge")),
        "Body of Knowledge's trigger must not go on the stack for unrelated damage"
    );

    // Damage to Body of Knowledge itself must draw that many cards.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, TargetRef::Object(body), 3),
        &mut events,
    )
    .expect("damage to Body of Knowledge resolves");
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Body of Knowledge's draw trigger must be on the stack after it is dealt damage"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        hand_count(&runner, P0),
        hand_before + 3,
        "CR 121.2: Body of Knowledge must draw cards equal to the damage dealt to it"
    );
}
