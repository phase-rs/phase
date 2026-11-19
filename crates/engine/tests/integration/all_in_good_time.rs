//! All in Good Time: the extra turn comes without a scheme chain.
//!
//! Oracle: "When you set this scheme in motion, take an extra turn after this
//! one. Schemes can't be set in motion that turn."
//!
//! CR 701.32a + CR 904.9: "that turn" is the extra turn granted by the first
//! sentence (the Final Fortune anaphora). CR 101.2: the prohibition skips the
//! precombat-main turn-based action entirely — the next scheme stays on top,
//! face down, and no trigger fires. CR 904.10 / CR 314.6: each resolved
//! non-ongoing scheme still reaches the bottom of the scheme deck via SBA.
//!
//! Revert-to-red: drop the `CantSetSchemeInMotion` gate — the extra turn sets
//! the second scheme in motion (skip assertions fail). Resolve "that turn" as
//! `UntilEndOfTurn` — the prohibition expires before the extra turn (same).
//! Break the duration arming — the second scheme never fires again (expiry
//! assertions fail).

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
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const ALL_IN_GOOD_TIME: &str = "When you set this scheme in motion, take an extra turn after this one. Schemes can't be set in motion that turn.";

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

/// Resolve every pending trigger with no choices (extra turn + prohibition
/// install are both prompt-free).
fn drain_triggers(runner: &mut GameRunner) {
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
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
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

fn is_face_up_in_command(runner: &GameRunner, scheme: ObjectId) -> bool {
    runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down
}

#[test]
fn all_in_good_time_skips_the_extra_turn_scheme_and_refires_after() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("All in Good Time", ALL_IN_GOOD_TIME);
    let first = create_scheme_deck_card(runner.state_mut(), &face, P0);
    let second = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // Turn 1: the turn-based action sets the first copy in motion; its
    // trigger queues the extra turn and installs the prohibition.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        is_face_up_in_command(&runner, first),
        "reach guard: turn 1 must set the first scheme in motion"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "the trigger must queue one extra turn"
    );
    assert_eq!(
        runner.state().extra_turns[0].player,
        P0,
        "the extra turn must belong to the archenemy"
    );
    // The resolved non-ongoing scheme reaches the bottom via SBA.
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&first),
        "the resolved scheme must be on the bottom of the scheme deck"
    );

    // Extra turn: the prohibition skips the turn-based action — the second
    // copy stays on top, face down, and no trigger fires.
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: the extra turn must belong to the archenemy"
    );
    assert_eq!(
        runner.state().scheme_deck.front(),
        Some(&second),
        "the prohibited turn must not move the next scheme off the top"
    );
    assert!(
        runner.state().objects[&second].face_down,
        "the prohibited turn must not turn the next scheme face up"
    );
    assert!(
        !runner.state().command_zone.contains(&second),
        "the prohibited turn must not promote the next scheme"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the prohibited turn must fire no scheme trigger"
    );

    // Turn 2 (after the intervening opponent turn): the window has expired,
    // so the second copy fires normally — proving both the skip above (same
    // machinery, unblocked) and the expiry.
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the opponent's turn must intervene"
    );
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: back to the archenemy's turn"
    );
    assert!(
        is_face_up_in_command(&runner, second),
        "after expiry the turn-based action must fire again"
    );
    drain_triggers(&mut runner);
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "the second copy must queue its own extra turn"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
