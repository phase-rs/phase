//! Mornsong Aria, driven through the real trigger and casting pipeline.
//!
//! Oracle:
//!   "Players can't draw cards or gain life.
//!    At the beginning of each player's draw step, that player loses 3 life,
//!    searches their library for a card, puts it into their hand, then
//!    shuffles."
//!
//! The draw-step trigger is a phase trigger scoped to the active player, so
//! "that player" is `TargetFilter::ScopedPlayer` on the `LoseLife`, the
//! `SearchLibrary` and the `Shuffle`. CR 608.2c: "that player" is the drawing
//! player on every link, so on an opponent's draw step the OPPONENT searches
//! their own library and the card goes to their hand. Before the fix the
//! search resolved its library owner and searcher to the Aria's controller.
//!
//! The static line parses to two prohibitions on the same subject: CR 121.3
//! (can't draw) and CR 119.7 (can't gain life). Before the fix only the draw
//! prohibition was emitted.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_has_cant_gain_life;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MORNSONG_ARIA: &str = "Players can't draw cards or gain life.\n\
     At the beginning of each player's draw step, that player loses 3 life, \
     searches their library for a card, puts it into their hand, then shuffles.";

const SALVE: &str = "You gain 4 life.";

/// Pass priority (declaring no attackers or blockers) until the engine stops on
/// something other than a priority pass or a combat declaration.
fn advance_to_decision(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// CR 608.2c + CR 701.23a: Mornsong Aria controlled by P0. On P1's draw step,
/// P1 loses 3 life and P1 searches P1's library; the found card goes to P1's
/// hand. P0's library and life are untouched.
#[test]
fn mornsong_aria_on_opponents_draw_step_the_drawing_player_searches_their_library() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B", "P0 Card C"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C"]);
    scenario.add_enchantment_from_oracle(P0, "Mornsong Aria", MORNSONG_ARIA);

    let mut runner = scenario.build();
    let p0_library_before: Vec<ObjectId> = runner.state().library_of(P0).iter().copied().collect();
    let p1_library_before: Vec<ObjectId> = runner.state().library_of(P1).iter().copied().collect();

    // The first draw step after P0's main phase is P1's.
    advance_to_decision(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Draw);

    let (searcher, offered) = match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => (*player, cards.clone()),
        other => panic!("expected the Aria's SearchChoice on P1's draw step, got {other:?}"),
    };
    assert_eq!(
        searcher, P1,
        "the drawing player searches, not the Aria's controller"
    );
    assert!(!offered.is_empty());
    assert!(
        offered.iter().all(|id| p1_library_before.contains(id)),
        "every offered card is in P1's library: {offered:?}"
    );
    assert_eq!(runner.life(P1), 17, "that player loses 3 life");
    assert_eq!(runner.life(P0), 20, "the Aria's controller loses no life");

    let found = offered[0];
    runner
        .act(GameAction::SelectCards { cards: vec![found] })
        .expect("P1 picks a card from their own library");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&found].zone, Zone::Hand);
    assert!(
        runner.state().players[P1.0 as usize].hand.contains(&found),
        "the found card goes to P1's hand"
    );
    assert_eq!(
        runner.state().library_of(P1).len(),
        p1_library_before.len() - 1
    );
    let mut p0_library_after: Vec<ObjectId> =
        runner.state().library_of(P0).iter().copied().collect();
    let mut p0_library_expected = p0_library_before.clone();
    p0_library_after.sort();
    p0_library_expected.sort();
    assert_eq!(
        p0_library_after, p0_library_expected,
        "the Aria's controller's library is not searched"
    );
}

fn cast_salve(with_aria: bool) -> i32 {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if with_aria {
        scenario.add_enchantment_from_oracle(P1, "Mornsong Aria", MORNSONG_ARIA);
    }
    let salve = scenario
        .add_spell_to_hand_from_oracle(P0, "Salve", false, SALVE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&salve].card_id;
    assert_eq!(player_has_cant_gain_life(runner.state(), P0), with_aria);
    runner
        .act(GameAction::CastSpell {
            object_id: salve,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the life-gain sorcery succeeds");
    runner.advance_until_stack_empty();
    runner.life(P0)
}

/// CR 119.7: "Players can't draw cards or gain life" also stops life gain —
/// including for a player who does not control the Aria. Control: the same
/// spell without the Aria gains 4.
#[test]
fn mornsong_aria_players_cant_gain_life() {
    assert_eq!(cast_salve(false), 24, "control: the sorcery gains 4 life");
    assert_eq!(
        cast_salve(true),
        20,
        "under Mornsong Aria no player gains life"
    );
}
