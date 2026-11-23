use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AbilityTag};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AJANI_ORACLE: &str = "Whenever you gain life, put a loyalty counter on Ajani.\n[0]: You gain 1 life.\n[−4]: Create a 2/2 white Cat Soldier creature token named Ajani's Pridemate with \"Whenever you gain life, put a +1/+1 counter on this token.\"\n[−10]: You get an emblem with \"Creatures you control get +2/+2.\"";
const AJANI_DECOY_ORACLE: &str = "[+1]: You gain life equal to the number of creatures you control plus the number of planeswalkers you control.\n[−2]: Create a 2/2 white Cat Soldier creature token named Ajani's Pridemate with \"Whenever you gain life, put a +1/+1 counter on this token.\"\n[0]: If you have at least 15 life more than your starting life total, exile Ajani and each artifact and creature your opponents control.";
const LILIANA_ORACLE: &str = "Whenever another creature or planeswalker you control enters, mill two cards.\nExhaust — {5}{B}: Return target creature or planeswalker card from your graveyard to the battlefield. Put a +1/+1 counter on Liliana. Activate only as a sorcery. (Activate each exhaust ability only once.)";

#[test]
fn zero_ability_adds_one_loyalty_to_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = scenario
        .add_planeswalker_from_oracle(P0, "Ajani Resolute", "Ajani", 2, AJANI_ORACLE)
        .id();
    let decoy = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Ajani, Strength of the Pride",
            "Ajani",
            5,
            AJANI_DECOY_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    let zero = runner.state().objects[&ajani]
        .abilities
        .iter()
        .position(|ability| ability.cost == Some(AbilityCost::Loyalty { amount: 0 }))
        .expect("Ajani Resolute has a zero loyalty ability");

    // CR 119.9 + CR 606.4: the zero cost leaves loyalty alone and the gain
    // creates one qualifying event. No object target is announced (CR 115.1d).
    let outcome = runner.activate(ajani, zero).resolve();
    outcome.assert_life_delta(P0, 1);

    // CR 201.5c + CR 306.5c: only the source gains one loyalty counter.
    assert_eq!(runner.state().objects[&ajani].loyalty, Some(3));
    assert_eq!(runner.state().objects[&decoy].loyalty, Some(5));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn liliana_exhaust_puts_counter_on_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let liliana = scenario
        .add_creature_from_oracle(P0, "Liliana the Repentant", 2, 2, LILIANA_ORACLE)
        .with_subtypes(vec!["Human", "Warlock"])
        .id();
    let chosen = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
        .id();
    let mill_cards = [
        scenario.add_land_to_library_top(P0, "Forest").id(),
        scenario.add_land_to_library_top(P0, "Swamp").id(),
    ];
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Colorless, 5)
            .chain([ManaType::Black])
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let exhaust = runner.state().objects[&liliana]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Exhaust))
        .expect("Liliana the Repentant has an exhaust ability");

    // CR 602.2b + CR 608.2b: the announced graveyard target returns first.
    let outcome = runner
        .activate(liliana, exhaust)
        .target_object(chosen)
        .resolve();
    outcome.assert_zone(&[chosen], Zone::Battlefield);
    outcome.assert_zone(&[unchosen], Zone::Graveyard);

    // CR 201.5c + CR 608.2c: the next instruction places its counter on Liliana.
    let objects = &runner.state().objects;
    assert_eq!(
        objects[&liliana].counters.get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert_eq!(
        objects[&chosen].counters.get(&CounterType::Plus1Plus1),
        None
    );
    // CR 603.6a + CR 701.17a: the returned creature triggers the two-card mill.
    outcome.assert_zone(&mill_cards, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}
