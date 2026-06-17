//! Issue #1020 — Level Up aura doubles +1/+1 counters when enchanted creature attacks.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const LEVEL_UP_ORACLE: &str = "Enchant creature\n\
When this Aura enters, put a +1/+1 counter on enchanted creature.\n\
Enchanted creature has \"Whenever this creature attacks, double the number of +1/+1 counters on it. Then if it has power 10 or greater, draw a card.\"";

#[test]
fn level_up_doubles_counters_when_enchanted_creature_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    // The attack trigger's draw clause needs a library to avoid a game-loss draw.
    scenario.add_card_to_library_top(P0, "Library Card");
    let aura = scenario
        .add_spell_to_hand(P0, "Level Up", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(LEVEL_UP_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 1,
            shards: vec![engine::types::mana::ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_999), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    let outcome = runner.cast(aura).target_object(bear).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Level Up cast must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[aura], Zone::Battlefield);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Level Up ETB trigger should put the first +1/+1 counter on the enchanted creature"
    );

    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "attack trigger should double +1/+1 counters from 1 to 2"
    );
}
