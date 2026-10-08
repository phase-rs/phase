//! Atraxa's Skitterfang — "At the beginning of combat on your turn, you may
//! remove an oil counter from this creature. When you do, target creature you
//! control gains your choice of flying, vigilance, deathtouch, or lifelink
//! until end of turn."
//!
//! Reported from a real game: once the last oil counter was gone the trigger
//! kept asking for a target and kept granting the keyword. The removal is
//! impossible, so the reflexive event never occurs and nothing may be granted.
//!
//! Oracle text below is verified against `client/public/card-data.json`; the
//! first line ("enters with three oil counters") is omitted because the
//! scenario places the permanent directly and sets the counters itself.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 603.12: a reflexive triggered ability triggers "based on whether the
//!   trigger event or events occurred earlier during the resolution".
//! - CR 608.2d: a player can't choose an impossible option, so the "you may"
//!   is never offered and the action is never taken.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::TargetRef;

const SKITTERFANG: &str = "At the beginning of combat on your turn, you may remove an oil counter from this creature. When you do, target creature you control gains your choice of flying, vigilance, deathtouch, or lifelink until end of turn.";

/// The four keywords the "your choice of" branch can grant. Asserting over all
/// of them (rather than the one the probe happened to pick) keeps the test from
/// passing merely because a different branch index was chosen.
const GRANTABLE: [Keyword; 4] = [
    Keyword::Flying,
    Keyword::Vigilance,
    Keyword::Deathtouch,
    Keyword::Lifelink,
];

/// Branch index 1 = vigilance, in the printed order flying / vigilance /
/// deathtouch / lifelink.
const VIGILANCE_BRANCH: usize = 1;

fn oil() -> CounterType {
    CounterType::Generic("oil".to_string())
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

struct Board {
    runner: GameRunner,
    skitterfang: ObjectId,
    bears: ObjectId,
}

fn board_with_oil(oil_counters: u32) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let skitterfang = scenario
        .add_creature_from_oracle(P0, "Atraxa's Skitterfang", 2, 2, SKITTERFANG)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    if oil_counters > 0 {
        scenario.with_counter(skitterfang, oil(), oil_counters);
    }
    // Library padding so advancing the turn cannot deck anyone.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    let runner = scenario.build();
    Board {
        runner,
        skitterfang,
        bears,
    }
}

/// Play the begin-combat trigger to completion, targeting Grizzly Bears and
/// picking vigilance. `take_the_may` decides the answer to the "you may remove
/// an oil counter" prompt. Records whether the reflexive ever demanded a target,
/// which is the observable half of "the trigger fired". Returns that flag.
fn play_the_trigger(board: &mut Board, take_the_may: bool) -> bool {
    let mut reflexive_asked_for_a_target = false;
    board.runner.advance_to_combat();
    for _ in 0..30 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                reflexive_asked_for_a_target = true;
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(board.bears)),
                    })
                    .expect("choosing the reflexive's target must be allowed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: take_the_may,
                    })
                    .expect("answering the counter-removal prompt must be allowed");
            }
            WaitingFor::ChooseOneOfBranch { .. } => {
                board
                    .runner
                    .act(GameAction::ChooseBranch {
                        index: VIGILANCE_BRANCH,
                    })
                    .expect("choosing vigilance must be allowed");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                board
                    .runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering triggers must be allowed");
            }
            WaitingFor::Priority { .. } => {
                if board.runner.state().stack.is_empty() {
                    break;
                }
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be allowed");
            }
            _ => break,
        }
    }
    reflexive_asked_for_a_target
}

/// The reported bug. With no oil counter the removal is impossible (CR 608.2d),
/// so the reflexive trigger never happens (CR 603.12): no target is demanded and
/// no creature gains anything. Reverting the `ability.optional &&
/// !optional_effect_performed` gate in `evaluate_condition`'s `WhenYouDo` arm
/// re-grants the keyword from nothing.
#[test]
fn no_oil_counter_means_no_reflexive_trigger_and_no_keyword() {
    let mut board = board_with_oil(0);
    assert_eq!(
        board.runner.state().objects[&board.skitterfang]
            .counters
            .get(&oil())
            .copied()
            .unwrap_or(0),
        0,
        "precondition: Atraxa's Skitterfang carries no oil counter"
    );

    let asked_for_a_target = play_the_trigger(&mut board, true);

    assert!(
        !asked_for_a_target,
        "CR 603.12: the removal could not happen, so the reflexive trigger must \
         never be created — it must not ask for a target"
    );
    for keyword in &GRANTABLE {
        assert!(
            !has_kw(&mut board.runner, board.bears, keyword),
            "no oil counter was removed, so nothing may be granted — but the \
             creature gained {keyword:?}"
        );
    }
}

/// Positive reach guard: with an oil counter present the card must still work
/// end to end — the counter comes off and the chosen keyword lands. This is what
/// proves the gate does not over-suppress a legitimate reflexive.
#[test]
fn one_oil_counter_still_removes_it_and_grants_the_chosen_keyword() {
    let mut board = board_with_oil(1);

    let asked_for_a_target = play_the_trigger(&mut board, true);

    assert!(
        asked_for_a_target,
        "with an oil counter to remove, the reflexive must fire and target"
    );
    assert_eq!(
        board.runner.state().objects[&board.skitterfang]
            .counters
            .get(&oil())
            .copied()
            .unwrap_or(0),
        0,
        "accepting must remove the oil counter (1 -> 0)"
    );
    assert!(
        has_kw(&mut board.runner, board.bears, &Keyword::Vigilance),
        "the chosen keyword must be granted to the targeted creature"
    );
    for keyword in [Keyword::Flying, Keyword::Deathtouch, Keyword::Lifelink] {
        assert!(
            !has_kw(&mut board.runner, board.bears, &keyword),
            "only the chosen branch may be granted — {keyword:?} leaked"
        );
    }
}

/// The other way the parent event fails to occur: an oil counter IS present, so
/// the "you may" is offered, and the player declines it. Nothing was removed, so
/// the reflexive must not fire (CR 603.12).
///
/// Stated plainly: this row does NOT discriminate the new gate — measured, it
/// passes with the gate reverted too, because an explicitly declined optional is
/// suppressed structurally (`resolve_optional_effect_decision` never runs the
/// dependent sub-chain, so the condition is never reached). It is kept as a pin:
/// the decline path and the never-offered path must stay in agreement, and this
/// is what fails if a future change makes decline reach the gate instead.
#[test]
fn declining_the_removal_fires_no_reflexive_and_keeps_the_counter() {
    let mut board = board_with_oil(1);

    let asked_for_a_target = play_the_trigger(&mut board, false);

    assert!(
        !asked_for_a_target,
        "a declined removal produced no trigger event, so the reflexive must \
         not ask for a target"
    );
    assert_eq!(
        board.runner.state().objects[&board.skitterfang]
            .counters
            .get(&oil())
            .copied()
            .unwrap_or(0),
        1,
        "declining must leave the oil counter in place"
    );
    for keyword in &GRANTABLE {
        assert!(
            !has_kw(&mut board.runner, board.bears, keyword),
            "the removal was declined, so nothing may be granted — but the \
             creature gained {keyword:?}"
        );
    }
}
