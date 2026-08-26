use engine::game::mana_payment::{can_pay, compute_hand_color_demand};
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PARADOX_SURVEYOR_ORACLE: &str = "Reach\nWhen this creature enters, look at the top five \
cards of your library. You may reveal a land card or a card with {X} in its mana cost from among \
them and put it into your hand. Put the rest on the bottom of your library in a random order.";

fn mana(color: ManaType, source: u64) -> ManaUnit {
    ManaUnit::new(color, ObjectId(source), false, Vec::new())
}

#[test]
fn paradox_surveyor_demand_fallback_casts_and_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let surveyor_cost = ManaCost::Cost {
        shards: vec![
            ManaCostShard::Green,
            ManaCostShard::GreenBlue,
            ManaCostShard::Blue,
        ],
        generic: 0,
    };
    let surveyor = scenario
        .add_creature_to_hand_from_oracle(P0, "Paradox Surveyor", 3, 3, PARADOX_SURVEYOR_ORACLE)
        .with_mana_cost(surveyor_cost.clone())
        .id();

    scenario
        .add_creature_to_hand(P0, "Demanding Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            generic: 0,
        });
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Green, 10_001),
            mana(ManaType::Green, 10_002),
            mana(ManaType::Blue, 10_003),
        ],
    );
    for name in [
        "Card One",
        "Card Two",
        "Card Three",
        "Card Four",
        "Card Five",
    ] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    assert_eq!(
        compute_hand_color_demand(runner.state(), P0, surveyor),
        [0, 1, 0, 0, 3]
    );
    let player = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 exists");
    assert!(can_pay(&player.mana_pool, &surveyor_cost));

    runner.cast(surveyor).resolve();

    assert_eq!(runner.state().objects[&surveyor].zone, Zone::Battlefield);
    let player = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 exists");
    assert!(player.mana_pool.mana.is_empty());
}
