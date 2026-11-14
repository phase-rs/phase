use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::ability::{PlayerScope, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BLOOD_SCRIVENER: &str = "If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.";
const ALMS_COLLECTOR: &str = "Flash
If an opponent would draw two or more cards, instead you and that player each draw a card.";

fn cast_draw_with<T>(
    instruction: &str,
    setup: impl FnOnce(&mut GameScenario) -> T,
) -> (GameRunner, Outcome, T) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..10 {
            scenario.add_spell_to_library_top(player, &format!("Filler {index}"), true);
        }
    }
    let setup_result = setup(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", false, instruction)
        .id();
    let mana: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    (runner, outcome, setup_result)
}

fn counts(
    events: &[GameEvent],
    state: &engine::types::game_state::GameState,
    player: PlayerId,
) -> (usize, usize, usize) {
    let card_drawn = events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player),
        )
        .count();
    let draw_actions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::Draw,
                    ..
                } if *player_id == player
            )
        })
        .count();
    let ledger = state
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (player, PlayerActionKind::Draw))
        .count();
    (card_drawn, draw_actions, ledger)
}

fn draw_action_count(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::Draw,
                    ..
                } if *player_id == player
            )
        })
        .count()
}

/// CR 121.1 + 121.2: ordinary one-card and multi-card spell instructions each
/// publish one draw action, independent of how many CardDrawn events they contain.
/// CR 121.6b: Scrivener's nested replacement draw and the remaining original draw
/// are separate completed instructions; each publishes and records once.
#[test]
fn normal_spell_draws_record_each_completed_instruction_once() {
    let (_, one, ()) = cast_draw_with("Draw a card.", |_| ());
    assert_eq!(counts(one.events(), one.state(), P0), (1, 1, 1));

    let draw_count = QuantityExpr::Ref {
        qty: QuantityRef::PlayerActionsThisTurn {
            player: PlayerScope::Controller,
            action: PlayerActionKind::Draw,
        },
    };
    let (_, two, ()) = cast_draw_with("Draw two cards.", |_| ());
    assert_eq!(counts(two.events(), two.state(), P0), (2, 1, 1));
    assert_eq!(
        resolve_quantity(two.state(), &draw_count, P0, ObjectId(0)),
        1
    );

    let (_, scrivener, ()) = cast_draw_with("Draw two cards.", |scenario| {
        scenario.add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER);
    });
    assert_eq!(counts(scrivener.events(), scrivener.state(), P0), (3, 2, 2));
    assert_eq!(
        resolve_quantity(scrivener.state(), &draw_count, P0, ObjectId(0)),
        2
    );
}

/// CR 121.2c + 616.1: Alms Collector creates independent P1 and P0 draw
/// instructions. The per-player action and turn ledgers must keep those subjects.
#[test]
fn alms_generated_draws_record_the_actual_players_once() {
    let (_, outcome, ()) = cast_draw_with("Draw two cards.", |scenario| {
        scenario.add_creature_from_oracle(P1, "Alms Collector", 3, 3, ALMS_COLLECTOR);
    });
    assert_eq!(counts(outcome.events(), outcome.state(), P0), (1, 1, 1));
    assert_eq!(counts(outcome.events(), outcome.state(), P1), (1, 1, 1));
    let draw_count = QuantityExpr::Ref {
        qty: QuantityRef::PlayerActionsThisTurn {
            player: PlayerScope::Controller,
            action: PlayerActionKind::Draw,
        },
    };
    assert_eq!(
        resolve_quantity(outcome.state(), &draw_count, P0, ObjectId(0)),
        1
    );
    assert_eq!(
        resolve_quantity(outcome.state(), &draw_count, P1, ObjectId(0)),
        1
    );
}

/// CR 121.6b + 608.2c: an accepted Dredge pause resumes the same spell
/// instruction. Its replacement delivery and remaining normal draw yield one
/// completion event and one turn-ledger entry after the choice.
#[test]
fn dredge_choice_resume_records_completed_draw_once() {
    let (mut runner, initial, dredger) = cast_draw_with("Draw two cards.", |scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Test Dredger", 1, 1)
            .from_oracle_text_with_keywords(&["Dredge"], "Dredge 2")
            .id()
    });
    assert!(matches!(
        initial.final_waiting_for(),
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(draw_action_count(initial.events(), P0), 0);
    assert_eq!(counts(&[], runner.state(), P0).2, 0);

    let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidates,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected the first Dredge choice")
    };
    assert_eq!(player, P0);
    assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == dredger)
        .expect("the Dredge replacement is offered");
    assert_eq!(
        candidates
            .get(index + 1)
            .map(|candidate| candidate.description.as_str()),
        Some("Decline")
    );
    let resumed = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("accepting Dredge resumes the draw");
    assert_eq!(counts(&resumed.events, runner.state(), P0), (1, 1, 1));
    runner.advance_until_stack_empty();
    assert_eq!(counts(&[], runner.state(), P0).2, 1);
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(runner.state().resolution_stack.is_empty());
}
