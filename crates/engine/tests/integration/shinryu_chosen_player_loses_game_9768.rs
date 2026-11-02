//! Shinryu, Transcendent Rival (#9768) — "When the chosen player loses the
//! game, you win the game." through the real parser and trigger pipeline.
//!
//! Verbatim Oracle text (Scryfall, back face of Zenos yae Galvus):
//!
//! ```text
//! Flying
//! As this creature transforms into Shinryu, choose an opponent.
//! Burning Chains — When the chosen player loses the game, you win the game.
//! ```
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 607.2d: "the chosen player" refers only to the linked choice.
//!   - CR 104.3b: a player at 0 or less life loses the game (an SBA).
//!   - CR 104.2b: an effect may state that a player wins the game.
//!   - CR 704.3: losses found in one SBA check are a single event.
//!   - CR 800.4d: a triggered ability controlled by a player who has left the
//!     game isn't put on the stack.
//!
//! The runtime rows build the back face from the Oracle text above and attach
//! it to a vanilla front face (the `cr733_resolved_transform.rs` recipe), then
//! transform through a real `Transform` spell, so the choice is made by the
//! parsed `As … transforms into …` ability and persisted by the parser's
//! linked-choice relation — not written by the test.

use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ChoiceType, ChosenAttribute, Effect, EffectScope, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const SHINRYU_NAME: &str = "Shinryu, Transcendent Rival";
const SHINRYU: &str = "Flying\nAs this creature transforms into Shinryu, choose an opponent.\nBurning Chains — When the chosen player loses the game, you win the game.";

/// CR 607.2d: the loss trigger reads the persisted choice, and that reader is
/// what makes the linked `As … transforms into …` choice persist on the source.
/// Base: the loss line is an `Unknown` trigger and the choice is not persisted.
#[test]
fn shinryu_oracle_links_the_transform_choice_to_the_loss_trigger() {
    let parsed = parse_oracle_text(SHINRYU, SHINRYU_NAME, &[], &["Creature".to_string()], &[]);
    assert_eq!(parsed.triggers.len(), 2, "got {:?}", parsed.triggers);

    let chooser = &parsed.triggers[0];
    assert_eq!(chooser.mode, TriggerMode::Transformed);
    let execute = chooser.execute.as_deref().expect("choice body");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::Choose {
                choice_type: ChoiceType::Opponent { .. },
                persist: true,
                ..
            }
        ),
        "the linked opponent choice must persist on the source, got {:?}",
        execute.effect
    );

    let loss = &parsed.triggers[1];
    assert_eq!(loss.mode, TriggerMode::LosesGame, "got {:?}", loss.mode);
    assert_eq!(loss.valid_target, Some(TargetFilter::SourceChosenPlayer));
    let execute = loss.execute.as_deref().expect("loss body");
    assert!(
        matches!(execute.effect.as_ref(), Effect::WinTheGame { .. }),
        "got {:?}",
        execute.effect
    );
}

/// A `players`-player game in P0's main phase: a vanilla front face for P0
/// carrying Shinryu as its back face, and a zero-cost Transform sorcery.
struct Board {
    runner: GameRunner,
    dfc: ObjectId,
    transform_spell: ObjectId,
}

fn board(players: u8) -> Board {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let dfc = scenario.add_creature(P0, "Zenos yae Galvus", 4, 4).id();
    let donor = scenario
        .add_creature_from_oracle(P0, SHINRYU_NAME, 8, 8, SHINRYU)
        .id();
    let mut spell = scenario.add_spell_to_hand(P0, "Rouse", true);
    spell.with_ability(Effect::Transform {
        target: TargetFilter::Typed(TypedFilter::creature()),
        scope: EffectScope::Single,
    });
    let transform_spell = spell.id();

    let mut runner = scenario.build();
    let back_face = snapshot_object_face(&runner.state().objects[&donor]);
    runner
        .state_mut()
        .objects
        .get_mut(&dfc)
        .expect("the double-faced permanent exists")
        .back_face = Some(back_face);
    // The donor only supplied the face; take it off the battlefield so its own
    // copy of the abilities can't observe anything.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), donor, Zone::Exile, &mut events);

    Board {
        runner,
        dfc,
        transform_spell,
    }
}

/// Transform the front face into Shinryu and answer the opponent choice.
fn transform_and_choose(board: &mut Board, chosen: PlayerId) {
    let Board {
        runner,
        dfc,
        transform_spell,
    } = board;
    runner.cast(*transform_spell).target_object(*dfc).resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[dfc].transformed,
        "reach: the Transform spell turned the permanent to Shinryu"
    );
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the transform must ask for an opponent, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice_type, ChoiceType::Opponent { .. }),
        "got {choice_type:?}"
    );
    assert!(
        options.contains(&chosen.0.to_string()),
        "the chosen player must be offered, got {options:?}"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.0.to_string(),
        })
        .expect("choose the opponent");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[dfc].chosen_attributes,
        vec![ChosenAttribute::Player(chosen)],
        "reach: the choice is persisted on Shinryu"
    );
}

/// CR 104.3b: put each player in `losers` to 0 life and let state-based
/// actions eliminate them all in one check. Returns the players still in the
/// game right after that check, then resolves the stack.
fn lose_by_life(runner: &mut GameRunner, losers: &[PlayerId]) -> Vec<PlayerId> {
    for loser in losers {
        runner.state_mut().players[loser.0 as usize].life = 0;
    }
    runner.act(GameAction::PassPriority).expect("pass");
    for loser in losers {
        assert!(
            runner.state().players[loser.0 as usize].is_eliminated,
            "reach: {loser:?} lost the game"
        );
    }
    let remaining = runner
        .state()
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();
    runner.advance_until_stack_empty();
    remaining
}

fn winner(runner: &GameRunner) -> Option<Option<PlayerId>> {
    match &runner.state().waiting_for {
        WaitingFor::GameOver { winner } => Some(*winner),
        _ => None,
    }
}

/// CR 607.2d + CR 104.2b: in a four-player game, an unchosen opponent losing
/// does nothing; the chosen opponent losing makes Shinryu's controller win.
/// Base: the trigger never fires, so the game goes on after P2 loses.
#[test]
fn chosen_player_losing_wins_the_game_but_another_player_losing_does_not() {
    let mut board = board(4);
    transform_and_choose(&mut board, P2);
    let runner = &mut board.runner;

    let remaining = lose_by_life(runner, &[P1]);
    assert_eq!(remaining, vec![P0, P2, P3], "reach: only P1 left");
    assert_eq!(
        winner(runner),
        None,
        "an unchosen opponent losing must not end the game"
    );

    let remaining = lose_by_life(runner, &[P2]);
    assert_eq!(
        remaining,
        vec![P0, P3],
        "reach: P3 is still in the game, so only the trigger can end it"
    );
    assert_eq!(
        winner(runner),
        Some(Some(P0)),
        "CR 104.2b: the chosen player lost, so Shinryu's controller wins"
    );
}

/// CR 704.3: the chosen player losing alongside another opponent
/// in the same SBA check still fires the trigger; P3 remains in the game, so
/// only the trigger can end it.
#[test]
fn chosen_player_losing_simultaneously_with_another_still_wins() {
    let mut board = board(4);
    transform_and_choose(&mut board, P2);
    let runner = &mut board.runner;

    let remaining = lose_by_life(runner, &[P1, P2]);
    assert_eq!(remaining, vec![P0, P3], "reach: P3 is still in the game");
    assert_eq!(winner(runner), Some(Some(P0)));
}

/// CR 800.4d: when Shinryu's controller and the chosen player lose in the
/// same check, the trigger would be controlled by a player who has left the
/// game, so it isn't put on the stack and nobody wins from it.
#[test]
fn controller_losing_with_the_chosen_player_does_not_win() {
    let mut board = board(4);
    transform_and_choose(&mut board, P2);
    let runner = &mut board.runner;

    let remaining = lose_by_life(runner, &[P0, P2]);
    assert_eq!(remaining, vec![P1, P3], "reach: P1 and P3 are still in");
    assert_eq!(
        winner(runner),
        None,
        "P1 and P3 are still in the game; an eliminated player can't win"
    );
}

/// The #9768 triage repro: the choice is written straight onto the source,
/// isolating the trigger from the transform choice. Base: the loss line is an
/// `Unknown` trigger, so P1 leaves and the game goes on (P0 and P2 remain).
#[test]
fn triage_shinryu_chosen_player_loses_you_win() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let shinryu = scenario
        .add_creature_from_oracle(P0, SHINRYU_NAME, 8, 8, SHINRYU)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&shinryu)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Player(P1));

    let remaining = lose_by_life(&mut runner, &[P1]);
    assert_eq!(remaining, vec![P0, P2], "reach: P2 is still in the game");
    assert_eq!(
        winner(&runner),
        Some(Some(P0)),
        "P0 wins when the chosen player loses"
    );
}

/// CR 607.2d: with no linked choice (Shinryu on the battlefield without having
/// transformed — the card's own ruling), "the chosen player" is nobody.
#[test]
fn shinryu_without_a_choice_never_wins() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let shinryu = scenario
        .add_creature_from_oracle(P0, SHINRYU_NAME, 8, 8, SHINRYU)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&shinryu]
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.definition.mode == TriggerMode::LosesGame),
        "reach: the loss trigger is on Shinryu"
    );

    let remaining = lose_by_life(&mut runner, &[P1]);
    assert_eq!(remaining, vec![P0, P2, P3]);
    assert_eq!(winner(&runner), None);
}

/// Hostile: Shinryu leaves the battlefield after the choice and before the
/// chosen player loses — the ability no longer exists to trigger.
#[test]
fn shinryu_gone_before_the_loss_does_not_win() {
    let mut board = board(4);
    transform_and_choose(&mut board, P2);
    let dfc = board.dfc;
    let runner = &mut board.runner;

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), dfc, Zone::Graveyard, &mut events);
    assert_ne!(runner.state().objects[&dfc].zone, Zone::Battlefield);

    let remaining = lose_by_life(runner, &[P2]);
    assert_eq!(remaining, vec![P0, P1, P3]);
    assert_eq!(winner(runner), None);
}

/// Two-player sanity: the game is over either way, and P0 is the winner.
#[test]
fn two_player_chosen_opponent_losing_ends_with_controller_winning() {
    let mut board = board(2);
    transform_and_choose(&mut board, P1);
    let runner = &mut board.runner;

    runner.state_mut().players[P1.0 as usize].life = 0;
    runner.act(GameAction::PassPriority).expect("pass");
    runner.advance_until_stack_empty();
    assert_eq!(winner(runner), Some(Some(P0)));
}
