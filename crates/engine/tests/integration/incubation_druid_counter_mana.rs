//! Incubation Druid: "{T}: Add one mana of any type that a land you control
//! could produce. If this creature has a +1/+1 counter on it, add three mana of
//! that type instead."
//!
//! Issue #695: tapped by hand with a +1/+1 counter, it must add three mana of
//! the chosen type, not one. Auto-payment is covered in
//! `mana_any_one_color_quantity`; these rows drive the manual routes: the
//! offered mana-source action and `ActivateAbility` with priority, and the
//! ability activated inside a manual cast's payment window. The priority
//! `ActivateAbility` row starts with one counter. The other positive rows
//! get three counters from a real Adapt 3 activation.

use engine::game::mana_sources::{
    activatable_land_mana_options, activatable_mana_actions_for_player,
    activatable_mana_source_selections,
};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const INCUBATION_DRUID: &str = "{T}: Add one mana of any type that a land you control could produce. If this creature has a +1/+1 counter on it, add three mana of that type instead.\n{3}{G}{G}: Adapt 3. (If this creature has no +1/+1 counters on it, put three +1/+1 counters on it.)";

fn board() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let druid = scenario
        .add_creature_from_oracle(P0, "Incubation Druid", 0, 2, INCUBATION_DRUID)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    (scenario, druid)
}

fn add_adapt_lands(scenario: &mut GameScenario) -> [ObjectId; 5] {
    [
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ]
}

fn adapt(runner: &mut GameRunner, druid: ObjectId, lands: [ObjectId; 5]) {
    assert!(runner.state().players[0].mana_pool.is_empty());
    for land in lands {
        let selection = activatable_land_mana_options(runner.state(), land, P0)
            .into_iter()
            .next()
            .and_then(|option| option.semantic_selection(runner.state()))
            .expect("each basic land offers its mana ability");
        runner
            .act(GameAction::TapLandForMana { selection })
            .unwrap();
    }
    runner.activate(druid, 1).resolve();

    // CR 701.46a: Adapt 3 puts three +1/+1 counters on a creature with none.
    assert_eq!(
        runner.state().objects[&druid]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&3)
    );
    assert!(lands.iter().all(|land| runner.state().objects[land].tapped));
    assert!(!runner.state().objects[&druid].tapped);
    assert!(runner.state().players[0].mana_pool.is_empty());
    assert_priority_with_empty_stack(runner);
}

fn offered_tap(runner: &GameRunner, druid: ObjectId) -> GameAction {
    let selection = activatable_mana_source_selections(runner.state(), P0)
        .into_iter()
        .find(|selection| selection.source.object_id == druid)
        .expect("the Druid offers its mana ability");
    GameAction::ActivateManaSource { selection }
}

fn tap_and_choose(runner: &mut GameRunner, action: GameAction, color: ManaType) {
    assert!(runner.state().players[0].mana_pool.is_empty());
    runner.act(action).unwrap();
    let WaitingFor::ChooseManaColor {
        player,
        choice: ManaChoicePrompt::SingleColor { options },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected one mana type choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    // CR 106.7: tapped lands still determine the types the Druid can produce.
    assert_eq!(options.len(), 2);
    assert!(options.contains(&ManaType::Blue));
    assert!(options.contains(&ManaType::Green));
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(color),
            count: 1,
        })
        .unwrap();
}

fn assert_druid_mana(runner: &GameRunner, druid: ObjectId, color: ManaType, amount: usize) {
    let pool = &runner.state().players[0].mana_pool;
    // CR 608.2c + CR 614.1a + CR 614.15: with a counter, the ability replaces
    // its own one-mana output with three mana of the chosen type.
    assert_eq!(
        pool.count_color(color),
        amount,
        "Druid's {color:?} mana after the choice"
    );
    assert_eq!(pool.total(), amount);
    assert!(pool.units().all(|unit| unit.source_id == druid));
    assert!(runner.state().objects[&druid].tapped);
}

fn assert_priority_with_empty_stack(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 605.1a + CR 605.3b: the Druid's mana ability resolves without using the stack.
    assert!(runner.state().stack.is_empty());
}

#[test]
fn hand_tap_after_adapt_adds_three_of_the_chosen_type() {
    let (mut scenario, druid) = board();
    let lands = add_adapt_lands(&mut scenario);
    let mut runner = scenario.build();
    adapt(&mut runner, druid, lands);

    // CR 605.3a: the player can activate the mana ability with priority.
    let action = offered_tap(&runner, druid);
    tap_and_choose(&mut runner, action, ManaType::Blue);
    assert_druid_mana(&runner, druid, ManaType::Blue, 3);
    assert_priority_with_empty_stack(&runner);
}

#[test]
fn activate_ability_with_one_counter_adds_three() {
    let (mut scenario, druid) = board();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_counter(druid, CounterType::Plus1Plus1, 1);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&druid]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );

    tap_and_choose(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: druid,
            ability_index: 0,
            payment_mode: CastPaymentMode::Manual,
        },
        ManaType::Green,
    );
    assert_druid_mana(&runner, druid, ManaType::Green, 3);
    assert_priority_with_empty_stack(&runner);
}

#[test]
fn manual_payment_during_a_cast() {
    let (mut scenario, druid) = board();
    let lands = add_adapt_lands(&mut scenario);
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let troll = scenario.add_real_card(P0, "Old-Growth Troll", Zone::Hand, db);
    let mut runner = scenario.build();
    adapt(&mut runner, druid, lands);

    let card_id = runner.state().objects[&troll].card_id;
    assert!(runner.state().players[0].mana_pool.is_empty());
    runner
        .act(GameAction::CastSpell {
            object_id: troll,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    let payment_window = runner.state().waiting_for.clone();
    assert!(
        matches!(payment_window, WaitingFor::ManaPayment { player: P0, .. }),
        "expected P0's mana payment window, got {payment_window:?}"
    );

    // CR 605.3a: the player can also activate the mana ability while casting.
    let stack_len = runner.state().stack.len();
    let action = activatable_mana_actions_for_player(runner.state(), P0)
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::ActivateAbility { source_id, ability_index: 0, .. }
                    if *source_id == druid
            )
        })
        .expect("manual payment offers the Druid's mana ability");
    tap_and_choose(&mut runner, action, ManaType::Green);
    assert_eq!(runner.state().waiting_for, payment_window);
    assert_druid_mana(&runner, druid, ManaType::Green, 3);
    // CR 605.3b: the activation adds no stack entry during payment.
    assert_eq!(runner.state().stack.len(), stack_len);

    runner.act(GameAction::PassPriority).unwrap();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert!(runner.state().players[0].mana_pool.is_empty());
    assert_priority_with_empty_stack(&runner);
}

#[test]
fn no_counter_adds_one() {
    let (mut scenario, druid) = board();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let action = offered_tap(&runner, druid);
    tap_and_choose(&mut runner, action, ManaType::Blue);
    assert_druid_mana(&runner, druid, ManaType::Blue, 1);
    assert_priority_with_empty_stack(&runner);
}
