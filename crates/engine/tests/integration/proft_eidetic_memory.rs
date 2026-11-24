//! Issue #612: Proft's Eidetic Memory's combat trigger never fired, because "if you've drawn more
//! than one card this turn" didn't parse.
//! CR 603.4: Proft's intervening-if prevents triggering after only one draw.
//! CR 121.2 + CR 608.2h + CR 122.1: Count individual draws at resolution
//! to determine how many +1/+1 counters the selected creature receives.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const PROFT: &str = "When Proft's Eidetic Memory enters, draw a card.\nYou have no maximum hand size.\nAt the beginning of combat on your turn, if you've drawn more than one card this turn, put X +1/+1 counters on target creature you control, where X is the number of cards you've drawn this turn minus one.";

fn scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut game = GameScenario::new();
    game.at_phase(Phase::PreCombatMain);
    game.add_enchantment_from_oracle(P0, "Proft's Eidetic Memory", PROFT)
        .as_legendary();
    let target = game.add_creature(P0, "First friendly creature", 2, 2).id();
    let other = game.add_creature(P0, "Second friendly creature", 2, 2).id();
    let opposing = game.add_creature(P1, "Opposing creature", 2, 2).id();
    for _ in 0..6 {
        game.add_basic_land(P0, ManaColor::Blue);
    }
    game.with_library_top(
        P0,
        &["First draw", "Second draw", "Third draw", "Fourth draw"],
    );
    (game, target, other, opposing)
}

fn divination(game: &mut GameScenario) -> ObjectId {
    game.add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id()
}

fn begin_combat(runner: &mut GameRunner) {
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);
}

fn select_target(runner: &mut GameRunner, target: ObjectId, other: ObjectId, opposing: ObjectId) {
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Proft's target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    let legal = &target_slots[0].legal_targets;
    assert!(legal.contains(&TargetRef::Object(target)));
    assert!(legal.contains(&TargetRef::Object(other)));
    assert!(!legal.contains(&TargetRef::Object(opposing)));
    assert_eq!(legal.len(), 2);
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("select Proft's target");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn counters(runner: &GameRunner, target: ObjectId) -> u32 {
    runner.state().objects[&target]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_completed(runner: &GameRunner) {
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn proft_puts_one_or_three_counters_after_two_or_four_draws() {
    for (casts, expected) in [(1, 1), (2, 3)] {
        let (mut game, target, other, opposing) = scenario();
        let spells: Vec<_> = (0..casts).map(|_| divination(&mut game)).collect();
        let mut runner = game.build();
        for spell in spells {
            runner.cast(spell).resolve();
        }
        begin_combat(&mut runner);
        select_target(&mut runner, target, other, opposing);
        runner.advance_until_stack_empty();
        assert_completed(&runner);
        assert_eq!(counters(&runner, target), expected);
        assert_eq!(counters(&runner, other), 0);
    }
}

#[test]
fn proft_does_not_trigger_after_exactly_one_draw() {
    let (mut game, target, other, _) = scenario();
    let spell = game
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = game.build();
    runner.cast(spell).resolve();
    assert_eq!(runner.state().players[0].library.len(), 3);
    begin_combat(&mut runner);
    assert_completed(&runner);
    assert_eq!(counters(&runner, target), 0);
    assert_eq!(counters(&runner, other), 0);
}

#[test]
fn proft_counts_cards_drawn_in_response_to_its_trigger() {
    let (mut game, target, other, opposing) = scenario();
    let first = divination(&mut game);
    let response = game
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, "Draw two cards.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let mut runner = game.build();
    runner.cast(first).resolve();
    begin_combat(&mut runner);
    select_target(&mut runner, target, other, opposing);

    let committed = runner.cast(response).commit();
    assert_eq!(committed.state().stack.len(), 2);
    runner.pass_both_players();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(counters(&runner, target), 0);

    runner.advance_until_stack_empty();
    assert_completed(&runner);
    assert_eq!(counters(&runner, target), 3);
    assert_eq!(counters(&runner, other), 0);
}
