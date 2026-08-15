use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const ORACLE: &str = "Crackle with Power deals five times X damage to each of up to X targets.";

fn crackle_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::X,
            ManaCostShard::X,
            ManaCostShard::Red,
            ManaCostShard::Red,
        ],
        generic: 0,
    }
}

fn red_pool_for_x(x: usize) -> Vec<ManaUnit> {
    (0..(x * 3 + 2))
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

fn crackle_scenario(x: usize) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_pool_for_x(x));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Crackle with Power", false, ORACLE)
        .with_mana_cost(crackle_cost())
        .id();
    (scenario, spell)
}

#[test]
fn crackle_with_power_x2_hits_a_player_and_creature_for_ten_each() {
    let (mut scenario, spell) = crackle_scenario(2);
    let creature = scenario.add_creature(P1, "Durable Target", 12, 12).id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .x(2)
        .target_player(P1)
        .target_object(creature)
        .resolve();

    // CR 107.3a + CR 107.3i + CR 120.2b: the announced X fixes both the
    // triple-X cost and five-times-X effect amount as the spell deals damage.
    outcome.assert_life_delta(P1, -10);
    assert_eq!(
        outcome.state().objects[&creature].damage_marked,
        10,
        "X=2 must deal five times X damage to the targeted creature"
    );
}

#[test]
fn crackle_with_power_x2_allows_fewer_targets_without_hitting_bystanders() {
    let (mut scenario, spell) = crackle_scenario(2);
    let target = scenario.add_creature(P1, "Durable Target", 12, 12).id();
    let bystander = scenario
        .add_creature(P1, "Untargeted Bystander", 12, 12)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(2).target_object(target).resolve();

    assert_eq!(outcome.state().objects[&target].damage_marked, 10);
    // CR 601.2c + CR 115.6: the caster chooses fewer than the maximum targets.
    assert_eq!(
        outcome.state().objects[&bystander].damage_marked,
        0,
        "up to X targets must not damage an unselected creature"
    );
}

#[test]
fn crackle_with_power_x0_resolves_without_target_selection() {
    let (scenario, spell) = crackle_scenario(0);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(0).resolve();

    // CR 115.6: an up-to-X targeted spell permits zero targets when X is zero.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "X=0 with zero targets must resolve back to priority"
    );
}
