//! Cavern-Hoard Dragon's spell-cost reduction uses the greatest artifact count
//! among opponents, not the caster's artifact count or the total artifact count.

use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);

const ORACLE: &str = "This spell costs {X} less to cast, where X is the greatest number of artifacts an opponent controls.\n\
Flying, trample, haste\n\
Whenever this creature deals combat damage to a player, you create a Treasure token for each artifact that player controls.";

const INVESTIGATOR_ORACLE: &str = "This artifact enters with a number of suspect counters on it equal to the greatest number of creatures a player controls.\n\
{2}, {T}, Remove a suspect counter from this artifact: Draw a card.\n\
{2}, Sacrifice this artifact: Draw a card.";

fn printed_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 7,
    }
}

fn reduced_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 4,
    }
}

fn exact_payment() -> Vec<ManaUnit> {
    [
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Red,
        ManaType::Red,
    ]
    .into_iter()
    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
    .collect()
}

fn add_artifacts(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for index in 0..count {
        scenario
            .add_creature(player, &format!("Test Artifact {index}"), 1, 1)
            .as_artifact();
    }
}

#[test]
fn cavern_hoard_dragon_uses_greatest_opponent_artifact_count_when_cast() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P0=5, P1=1, P2=3. The correct per-opponent maximum is 3. Summing the
    // opponents would yield 4; including the caster would yield 5.
    add_artifacts(&mut scenario, P0, 5);
    add_artifacts(&mut scenario, P1, 1);
    add_artifacts(&mut scenario, P2, 3);
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Cavern-Hoard Dragon", 6, 6, ORACLE)
        .with_mana_cost(printed_cost())
        .id();
    scenario.with_mana_pool(P0, exact_payment());

    let mut runner = scenario.build();
    assert_eq!(
        display_spell_cost(runner.state(), P0, dragon),
        Some(reduced_cost()),
        "the cost must be {{4}}{{R}}{{R}}: reduce {{7}}{{R}}{{R}} by the opponent's three artifacts"
    );

    let outcome = runner.cast(dragon).resolve();
    assert_eq!(
        outcome.zone_of(dragon),
        Zone::Battlefield,
        "the full Oracle card must be castable with exactly the reduced cost"
    );
}

#[test]
fn cavern_hoard_dragon_has_no_reduction_when_opponents_control_no_artifacts() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifacts(&mut scenario, P0, 5);
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Cavern-Hoard Dragon", 6, 6, ORACLE)
        .with_mana_cost(printed_cost())
        .id();

    let runner = scenario.build();
    assert_eq!(
        display_spell_cost(runner.state(), P0, dragon),
        Some(printed_cost()),
        "the caster's five artifacts must not reduce the cost when both opponents control none"
    );
}

#[test]
fn investigators_journal_enters_with_greatest_per_player_creature_count() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for (player, count) in [(P0, 2), (P1, 1), (P2, 3)] {
        for index in 0..count {
            scenario.add_creature(player, &format!("Test Creature {player:?} {index}"), 1, 1);
        }
    }
    let journal = scenario
        .add_artifact_to_hand_from_oracle(P0, "Investigator's Journal", INVESTIGATOR_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(journal).resolve();
    assert_eq!(outcome.zone_of(journal), Zone::Battlefield);
    assert_eq!(
        outcome.state().objects[&journal]
            .counters
            .get(&CounterType::Generic("suspect".to_string()))
            .copied(),
        Some(3),
        "the 2/1/3 creature distribution must yield max 3, not sum 6 or fixed 1"
    );
}
