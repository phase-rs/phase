//! Your Nightmares Are Delicious: discard-down-to-five, then draw on few discards.
//!
//! Oracle: "When you set this scheme in motion, each opponent who has more
//! than five cards in hand discards cards equal to the difference. Then if
//! fewer than three cards were discarded this way, you draw three cards."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger runs the discard-down.
//! CR 402.1 + CR 608.2c + CR 107.1: each over-five opponent discards
//! `Difference(HandSize, 5)`; the scope is strict GT, so exactly five
//! discards nothing. CR 608.2c + CR 701.9a: the tail gates on the published
//! discard count (`TrackedSetSize LT 3`) — zero discards still draws.
//!
//! Revert-to-red: drop the head idiom — the document gaps (scheme_face
//! asserts). Drop the tail gate — leg B draws (no-draw assertion fails).
//! Scope with GE 5 instead of GT — leg C prompts (prompt-count fails).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const YOUR_NIGHTMARES_ARE_DELICIOUS: &str = "When you set this scheme in motion, each opponent who has more than five cards in hand discards cards equal to the difference. Then if fewer than three cards were discarded this way, you draw three cards.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Hand && obj.owner == player)
        .count()
}

/// Fire the scheme with `p1_hand` cards in the opponent's hand. Returns
/// (cards P0 drew off the tail, P1's hand afterwards, discard prompts seen).
fn run_nightmares(p1_hand: usize) -> (usize, usize, usize) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    for i in 0..p1_hand {
        scenario.add_card_to_hand(P1, &format!("P1 Hand {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face(
        "Your Nightmares Are Delicious",
        YOUR_NIGHTMARES_ARE_DELICIOUS,
    );
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    let p0_before = hand_size(&runner, P0);

    let mut discard_prompts = 0;
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DiscardChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(player, P1, "only the opponent discards down");
                discard_prompts += 1;
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(count).collect(),
                    })
                    .expect("discard-down choice");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt resolving the scheme: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
    (
        hand_size(&runner, P0) - p0_before,
        hand_size(&runner, P1),
        discard_prompts,
    )
}

#[test]
fn your_nightmares_are_delicious_draws_when_few_discarded() {
    // Six cards: discard one (< 3 total) → draw three.
    let (drawn, p1_hand, prompts) = run_nightmares(6);
    assert_eq!(prompts, 1, "the over-five opponent must discard once");
    assert_eq!(p1_hand, 5, "the opponent must discard down to five");
    assert_eq!(drawn, 3, "fewer than three discarded must draw three");
}

#[test]
fn your_nightmares_are_delicious_skips_draw_when_enough_discarded() {
    // Eight cards: discard three (not fewer than three) → no draw.
    let (drawn, p1_hand, prompts) = run_nightmares(8);
    assert_eq!(prompts, 1, "the over-five opponent must discard once");
    assert_eq!(p1_hand, 5, "the opponent must discard down to five");
    assert_eq!(drawn, 0, "three discarded must not draw");
}

#[test]
fn your_nightmares_are_delicious_draws_when_nobody_discards() {
    // Exactly five: the strict-GT scope excludes the opponent (no prompt),
    // zero discarded is fewer than three → draw three.
    let (drawn, p1_hand, prompts) = run_nightmares(5);
    assert_eq!(prompts, 0, "exactly five must not prompt");
    assert_eq!(p1_hand, 5, "the at-five hand must be untouched");
    assert_eq!(drawn, 3, "zero discarded must draw three");
}
