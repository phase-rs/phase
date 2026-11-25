//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 10: "target opponent
//! creates ..." token instructions (Hungry Lynx, Wanted Scoundrels) whose typed opponent owner already
//! binds the chosen player, the same route Forbidden Orchard uses. Every card is the printed card
//! loaded through `add_real_card` and driven through public `GameRunner` actions in a three-player
//! game; the only synthetic setup is a mana pool. Each group picks the second opponent (so a
//! first-opponent default would fail), checks no other seat gets a token, and has a negative where
//! the chosen opponent leaves the game after the trigger is on the stack with that target, so the
//! instruction creates nothing (CR 608.2b).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

fn card(scenario: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(
        player,
        name,
        zone,
        shared_card_db().expect("printed fixture"),
    )
}

// CR 115.1d + CR 603.3d: The controller chooses one legal opponent as the trigger goes on the stack.
fn choose_second_opponent(runner: &mut GameRunner, source: ObjectId) {
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected P0's trigger target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    let legal = &target_slots[0].legal_targets;
    assert_eq!(legal.len(), 2);
    assert!(legal.contains(&TargetRef::Player(P1)));
    assert!(legal.contains(&TargetRef::Player(P2)));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P2)],
        })
        .expect("choose the second opponent");
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1);
    let trigger = &stack[0];
    assert_eq!(trigger.source_id, source);
    assert_eq!(
        trigger.ability().expect("trigger ability").targets,
        [TargetRef::Player(P2)]
    );
}

// CR 117.4 + CR 117.3b: Pass through the pending trigger and return to priority.
fn finish_trigger(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..3 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.pass_holding_team().expect("pass priority"));
    }
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    events
}

fn tokens(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.zone == Zone::Battlefield && object.is_token && object.name == name
        })
        .map(|(&id, _)| id)
        .collect()
}

// CR 111.2 + CR 115.1d: Every token on the battlefield belongs to the selected creator.
fn assert_tokens_for_second_opponent(
    runner: &GameRunner,
    name: &str,
    count: usize,
) -> Vec<ObjectId> {
    let ids = tokens(runner, name);
    assert_eq!(ids.len(), count);
    for id in &ids {
        let token = &runner.state().objects[id];
        assert_eq!((token.owner, token.controller), (P2, P2));
    }
    ids
}

// CR 104.3a + CR 608.2b: Concession leaves the committed trigger with no legal target.
fn concede_second_opponent(runner: &mut GameRunner) {
    let trigger = runner.state().stack[0].id;
    let departure = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(runner.state().players[P2.0 as usize].is_eliminated);
    assert_eq!(runner.state().stack[0].id, trigger);
    let resolution = finish_trigger(runner);
    assert!(resolution.iter().any(|event| matches!(
        event, GameEvent::StackResolved { object_id } if *object_id == trigger
    )));
    assert!(departure
        .events
        .iter()
        .chain(&resolution)
        .all(|event| { !matches!(event, GameEvent::TokenCreated { .. }) }));
}

// CR 106.4: Supply mana for Murder.
fn fund_murder(scenario: &mut GameScenario) {
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]); 3],
    );
}

fn lynx_trigger(runner: &mut GameRunner, lynx: ObjectId) {
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    // CR 117.4 + CR 603.2: Passing out of second main enters the end step and fires the printed trigger.
    for _ in 0..3 {
        runner.pass_holding_team().expect("advance toward end step");
    }
    assert_eq!(runner.state().phase, Phase::End);
    assert!(tokens(runner, "Rat").is_empty());
    choose_second_opponent(runner, lynx);
}

#[test]
fn hungry_lynx_creates_one_deathtouch_rat_for_second_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    let lynx = card(&mut scenario, P0, "Hungry Lynx", Zone::Battlefield);
    let mut runner = scenario.build();

    lynx_trigger(&mut runner, lynx);
    finish_trigger(&mut runner);
    let rats = assert_tokens_for_second_opponent(&runner, "Rat", 1);
    // CR 111.3 + CR 702.2a: The end-step instruction defines a black 1/1 Rat with deathtouch.
    let rat = &runner.state().objects[&rats[0]];
    assert_eq!((rat.power, rat.toughness), (Some(1), Some(1)));
    assert_eq!(rat.card_types.core_types, [CoreType::Creature]);
    assert_eq!(rat.card_types.subtypes, ["Rat"]);
    assert_eq!(rat.color, [ManaColor::Black]);
    assert!(rat.keywords.contains(&Keyword::Deathtouch));
}

#[test]
fn hungry_lynx_creates_no_rat_if_chosen_opponent_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    let lynx = card(&mut scenario, P0, "Hungry Lynx", Zone::Battlefield);
    let mut runner = scenario.build();

    lynx_trigger(&mut runner, lynx);
    concede_second_opponent(&mut runner);
    assert!(tokens(&runner, "Rat").is_empty());
}

#[test]
fn hungry_lynx_protects_its_cats_and_rewards_a_rat_death() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    let lynx = card(&mut scenario, P0, "Hungry Lynx", Zone::Battlefield);
    let lion = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let opposing_lion = card(&mut scenario, P2, "Savannah Lions", Zone::Battlefield);
    let murder = card(&mut scenario, P0, "Murder", Zone::Hand);
    let mut runner = scenario.build();

    lynx_trigger(&mut runner, lynx);
    finish_trigger(&mut runner);
    let rat = assert_tokens_for_second_opponent(&runner, "Rat", 1)[0];

    // CR 702.16a: The static ability grants Rat-subtype protection to the Lynx and its fellow Cats.
    let protection = Keyword::Protection(ProtectionTarget::CardType("rats".into()));
    for cat in [lynx, lion] {
        assert!(runner.state().objects[&cat].keywords.contains(&protection));
    }
    for other in [bear, opposing_lion] {
        assert!(!runner.state().objects[&other]
            .keywords
            .contains(&protection));
    }

    // CR 106.4: Seed mana after entering the end step, after the prior pool would empty.
    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..3 {
        pool.add(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    }
    let outcome = runner.cast(murder).target_object(rat).resolve();
    // CR 700.4 + CR 122.1a: The Rat dies and only Cats controlled by the Lynx's controller grow.
    assert!(tokens(&runner, "Rat").is_empty());
    outcome.assert_counters(lynx, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(lion, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(bear, CounterType::Plus1Plus1, 0);
    outcome.assert_counters(opposing_lion, CounterType::Plus1Plus1, 0);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

fn scoundrels_trigger() -> GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let scoundrels = card(&mut scenario, P0, "Wanted Scoundrels", Zone::Battlefield);
    let murder = card(&mut scenario, P0, "Murder", Zone::Hand);
    fund_murder(&mut scenario);
    let mut runner = scenario.build();

    // CR 701.8a + CR 700.4: Destroy the printed creature to make its dies trigger.
    let cast = runner.cast(murder).target_object(scoundrels).commit();
    assert_eq!(cast.state().stack.len(), 1);
    for _ in 0..3 {
        runner
            .pass_holding_team()
            .expect("pass priority over Murder");
    }
    let graveyard = &runner.state().players[P0.0 as usize].graveyard;
    assert!(graveyard
        .iter()
        .any(|id| runner.state().objects[id].name == "Wanted Scoundrels"));
    assert!(tokens(&runner, "Treasure").is_empty());
    choose_second_opponent(&mut runner, scoundrels);
    runner
}

#[test]
fn wanted_scoundrels_creates_two_treasures_for_second_opponent() {
    let mut runner = scoundrels_trigger();
    finish_trigger(&mut runner);
    // CR 111.10a: The two permanents are Treasure artifacts.
    for id in assert_tokens_for_second_opponent(&runner, "Treasure", 2) {
        let treasure = &runner.state().objects[&id];
        assert_eq!(treasure.card_types.core_types, [CoreType::Artifact]);
        assert_eq!(treasure.card_types.subtypes, ["Treasure"]);
    }
}

#[test]
fn wanted_scoundrels_creates_no_treasure_if_chosen_opponent_leaves() {
    let mut runner = scoundrels_trigger();
    concede_second_opponent(&mut runner);
    assert!(tokens(&runner, "Treasure").is_empty());
}
