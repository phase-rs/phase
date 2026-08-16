//! Battle of Wits production-pipeline regression (CR 401.3 + CR 603.4 + CR 104.2b).
//!
//! CR 401.3 makes each player's remaining library-card count countable. The
//! controller's live library count gates the upkeep trigger both when it would
//! fire and when it resolves. The fixtures deliberately give the opponent the
//! opposite threshold so a controller/scope regression is observable.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BATTLE_OF_WITS_ORACLE: &str =
    "At the beginning of your upkeep, if you have 200 or more cards in your library, you win the game.";

fn add_library_cards(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for index in 0..count {
        scenario.add_card_to_library_top(player, &format!("{player:?} Library Card {index}"));
    }
}

fn assert_no_unimplemented(ability: &AbilityDefinition) {
    assert!(
        !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
        "Battle of Wits parsed an Unimplemented effect: {ability:?}"
    );
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        assert_no_unimplemented(sub_ability);
    }
}

fn setup(controller_cards: usize, opponent_cards: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    add_library_cards(&mut scenario, P0, controller_cards);
    add_library_cards(&mut scenario, P1, opponent_cards);

    let source = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Battle of Wits", 0, 0, BATTLE_OF_WITS_ORACLE);
        builder.as_enchantment();
        builder.id()
    };
    let draw_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw Spell", true, "Draw a card.")
        .id();

    let runner = scenario.build();
    let source_object = runner
        .state()
        .objects
        .get(&source)
        .expect("Battle of Wits exists on the battlefield");
    assert_eq!(
        source_object.trigger_definitions.len(),
        1,
        "the exact Oracle text must parse one trigger"
    );
    let trigger = source_object
        .trigger_definitions
        .first()
        .expect("Battle of Wits trigger exists");
    assert!(
        trigger.definition.condition.is_some(),
        "the parsed trigger must retain its intervening-if condition"
    );
    assert_no_unimplemented(
        trigger
            .definition
            .execute
            .as_deref()
            .expect("the parsed trigger must retain its win effect"),
    );

    (runner, source, draw_spell)
}

fn triggers_from_source(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn assert_no_winner(runner: &GameRunner) {
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ),
        "no player should have won: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .players
            .iter()
            .all(|player| !player.is_eliminated),
        "both players must remain in the game"
    );
}

/// CR 603.4: with only 199 cards in its controller's library, Battle of Wits
/// does not trigger. The opponent's 200-card library is a hostile scope guard.
#[test]
fn battle_of_wits_does_not_trigger_with_199_cards() {
    let (mut runner, source, _) = setup(199, 200);

    runner.advance_to_upkeep();

    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the upkeep event was reached"
    );
    assert_eq!(
        triggers_from_source(&runner, source),
        0,
        "the opponent's 200-card library must not satisfy the controller condition"
    );
    assert_no_winner(&runner);
}

/// CR 603.4 + CR 104.2b: with 200 cards in its controller's library, Battle of
/// Wits triggers and its ordinary win effect ends the game for that controller.
#[test]
fn battle_of_wits_wins_with_200_cards() {
    let (mut runner, source, _) = setup(200, 199);

    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the upkeep event was reached"
    );
    assert_eq!(
        triggers_from_source(&runner, source),
        1,
        "the qualifying Battle of Wits trigger must be enqueued before resolution"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "the trigger controller must win after normal stack resolution"
    );
    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("opponent exists")
            .is_eliminated,
        "the sole opponent must be eliminated by the win effect"
    );
}

/// CR 603.4: the intervening-if is checked again on resolution. Moving one
/// library card through the production zone mover makes the live count 199, so
/// the already-enqueued trigger resolves without applying its win effect.
#[test]
fn battle_of_wits_rechecks_library_count_on_resolution() {
    let (mut runner, source, draw_spell) = setup(200, 199);

    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the upkeep event was reached"
    );
    assert_eq!(
        triggers_from_source(&runner, source),
        1,
        "precondition: the 200-card condition enqueued the trigger"
    );

    runner.cast(draw_spell).resolve();
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P0)
            .expect("controller exists")
            .library
            .len(),
        199,
        "production zone movement must make the resolution-time condition false"
    );

    runner.advance_until_stack_empty();

    assert!(
        runner.state().stack.is_empty(),
        "the intervening-if trigger must leave the stack"
    );
    assert_no_winner(&runner);
}
