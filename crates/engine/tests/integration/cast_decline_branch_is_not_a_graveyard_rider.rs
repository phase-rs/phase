//! A `CastFromZone`'s "If you don't …" sub-ability is a printed decline branch
//! (CR 608.2c), never a CR 614.1a "instead" redirect of the spell that was
//! cast. Discover the Impossible, verbatim:
//!
//! "Look at the top five cards of your library. Exile one of them face down and
//! put the rest on the bottom of your library in a random order. You may cast
//! the exiled card without paying its mana cost if it's an instant spell with
//! mana value 2 or less. If you don't, put that card into your hand."
//!
//! A cast instant resolves into its owner's graveyard (CR 608.2n); only a card
//! that was NOT cast goes to hand.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DISCOVER_THE_IMPOSSIBLE: &str = "Look at the top five cards of your library. Exile one of them face down and put the rest on the bottom of your library in a random order. You may cast the exiled card without paying its mana cost if it's an instant spell with mana value 2 or less. If you don't, put that card into your hand.";
const GAIN_THREE: &str = "You gain 3 life.";
const SHOCK: &str = "Shock deals 2 damage to any target.";

struct Board {
    runner: GameRunner,
    discover: ObjectId,
    pick: ObjectId,
}

/// P0 holds Discover the Impossible; the top of P0's library holds `pick`, an
/// instant with mana value 1, among five looked-at cards.
fn board(pick_name: &str, pick_text: &str, pick_cost: ManaCost) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let discover = scenario
        .add_spell_to_hand_from_oracle(P0, "Discover the Impossible", true, DISCOVER_THE_IMPOSSIBLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    for i in 0..6 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
        scenario.add_card_to_library_top(P1, &format!("Opponent Card {i}"));
    }
    let pick = scenario
        .add_spell_to_library_top(P0, pick_name, true)
        .from_oracle_text(pick_text)
        .with_mana_cost(pick_cost)
        .id();
    Board {
        runner: scenario.build(),
        discover,
        pick,
    }
}

#[derive(Default)]
struct Drive {
    offered: bool,
    zone_after_decision: Option<Zone>,
    target_prompt_seen: bool,
    zones_seen: Vec<Zone>,
}

/// Cast Discover the Impossible, exile `pick`, answer the cast offer with
/// `accept`, answer any target prompt with P1, and settle the stack.
fn cast_discover(board: &mut Board, accept: bool) -> Drive {
    board.runner.cast(board.discover).commit();
    let mut drive = Drive::default();
    for _ in 0..120 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                assert!(
                    cards.contains(&board.pick),
                    "reach-guard: the pick is looked at"
                );
                board
                    .runner
                    .act(GameAction::SelectCards {
                        cards: vec![board.pick],
                    })
                    .expect("exiling the pick");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                drive.offered = true;
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answering the cast offer");
                drive.zone_after_decision = Some(board.runner.state().objects[&board.pick].zone);
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                drive.target_prompt_seen = true;
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choosing P1");
            }
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
        drive
            .zones_seen
            .push(board.runner.state().objects[&board.pick].zone);
    }
    assert!(board.runner.state().stack.is_empty(), "the stack settles");
    drive
}

fn no_free_cast_permission_remains(board: &Board) {
    assert!(
        !board.runner.state().objects.values().any(|obj| obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltCost { .. }))),
        "no free-cast permission outlives the resolution"
    );
}

/// F1-R1 — CR 608.2g + CR 608.2n: the cast instant resolves into the graveyard;
/// "If you don't, put that card into your hand" does not apply to it.
#[test]
fn a_cast_instant_goes_to_the_graveyard_not_the_hand() {
    let mut board = board("Gain Three", GAIN_THREE, ManaCost::generic(1));
    let drive = cast_discover(&mut board, true);
    assert!(drive.offered);
    assert_eq!(
        drive.zone_after_decision,
        Some(Zone::Stack),
        "reach-guard: the instant was cast during the resolution"
    );
    let state = board.runner.state();
    assert_eq!(state.players[0].life, 23);
    assert_eq!(state.objects[&board.pick].zone, Zone::Graveyard);
    no_free_cast_permission_remains(&board);
}

/// F1-R1p — the same with a targeted instant, whose target prompt pauses the
/// resolution: the accept latch survives the pause, so the decline branch never
/// moves the cast spell to hand.
#[test]
fn a_cast_targeted_instant_survives_its_target_pause_into_the_graveyard() {
    let mut board = board("Shock", SHOCK, ManaCost::generic(1));
    let life_before = board.runner.state().players[1].life;
    let drive = cast_discover(&mut board, true);
    assert!(drive.offered);
    assert!(
        drive.target_prompt_seen,
        "reach-guard: the cast paused on Shock's target"
    );
    assert!(
        !drive.zones_seen.contains(&Zone::Hand),
        "Shock is never put into the hand"
    );
    assert_eq!(board.runner.state().players[1].life, life_before - 2);
    assert_eq!(
        board.runner.state().objects[&board.pick].zone,
        Zone::Graveyard
    );
}

/// F1-R2 — CR 608.2c: declining the cast still runs the decline branch.
#[test]
fn a_declined_card_goes_to_the_hand() {
    let mut board = board("Gain Three", GAIN_THREE, ManaCost::generic(1));
    let drive = cast_discover(&mut board, false);
    assert!(drive.offered, "reach-guard: the offer appeared");
    assert_eq!(board.runner.state().objects[&board.pick].zone, Zone::Hand);
    assert_eq!(board.runner.state().players[0].life, 20);
}
