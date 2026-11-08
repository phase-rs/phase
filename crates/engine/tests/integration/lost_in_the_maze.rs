use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOST_IN_THE_MAZE_ORACLE: &str = "Flash\nWhen this enchantment enters, tap X target creatures. Put a stun counter on each of those creatures you don't control. (If a permanent with a stun counter would become untapped, remove one from it instead.)\nTapped creatures you control have hexproof.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn lost_in_the_maze_stuns_only_targeted_creatures_not_controlled_by_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own creature", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 2, 2).id();
    let stolen = scenario
        .add_creature(P1, "Stolen creature", 2, 2)
        .controlled_by(P0)
        .id();
    let untargeted = scenario.add_creature(P1, "Untargeted creature", 2, 2).id();
    let spell = {
        let mut card = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Lost in the Maze",
            false,
            LOST_IN_THE_MAZE_ORACLE,
        );
        card.as_enchantment();
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        });
        card.id()
    };
    scenario.with_mana_pool(P0, floating_mana(5, ManaType::Blue));
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .x(3)
        .target_objects(&[own, opponent, stolen])
        .resolve();

    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for target in [own, opponent, stolen] {
        assert!(
            runner.state().objects[&target].tapped,
            "target {target:?} should be tapped"
        );
    }
    assert_eq!(
        runner.state().objects[&opponent]
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or_default(),
        1
    );
    assert_eq!(
        runner.state().objects[&own]
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        runner.state().objects[&stolen]
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or_default(),
        0
    );
    assert!(!runner.state().objects[&untargeted].tapped);
    assert_eq!(
        runner.state().objects[&untargeted]
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or_default(),
        0
    );
}
