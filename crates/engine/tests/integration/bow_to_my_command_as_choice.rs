//! Bow to My Command: the "as" choice is made inside the set-in-motion action.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! As you set this scheme in motion, choose an opponent.
//! Creatures the chosen player controls can't attack you or planeswalkers you control.
//! At the beginning of your opponents' end step, they may tap any number of untapped creatures they control with total power 8 or greater. If they do, abandon this scheme."
//!
//! CR 904.9: setting a scheme in motion is a turn-based action that doesn't
//! use the stack. CR 603.1: "as" abilities are not triggered abilities. The
//! choice must therefore be offered with an EMPTY stack — no player receives
//! priority first and nothing can counter it — and the chosen player must be
//! stored on the scheme for its linked "the chosen player" static (CR 607.2d).
//!
//! Revert-to-red: lower the "as" line back to `TriggerMode::SetInMotion` —
//! the choice arrives as a stack trigger (after priority), the empty-stack
//! assertion fails, and the choice is counterable.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{ChoiceType, ChosenAttribute};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const BOW_TO_MY_COMMAND: &str = "(An ongoing scheme remains face up until it's abandoned.)\nAs you set this scheme in motion, choose an opponent.\nCreatures the chosen player controls can't attack you or planeswalkers you control.\nAt the beginning of your opponents' end step, they may tap any number of untapped creatures they control with total power 8 or greater. If they do, abandon this scheme.";

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
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    face
}

fn scheme_object_id(runner: &GameRunner) -> ObjectId {
    *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Bow to My Command")
        .expect("Bow must be face up in the command zone")
}

#[test]
fn bow_choice_is_made_inside_set_in_motion_with_empty_stack() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let mut runner = scenario.build();

    // Production scheme-deck installation, then designate the archenemy.
    let face = scheme_face("Bow to My Command", BOW_TO_MY_COMMAND);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets Bow in motion.
    // The "as" choice must be offered with NOTHING on the stack.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "Bow must have left the scheme deck"
    );
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*player, P0, "the archenemy makes the as-choice");
            assert!(
                matches!(choice_type, ChoiceType::Opponent { .. }),
                "expected an opponent choice, got {choice_type:?}"
            );
            assert_eq!(
                options,
                &["1"],
                "the only legal opponent must be offered, got {options:?}"
            );
        }
        other => panic!("expected the as-choice prompt, got {other:?}"),
    }
    assert!(
        runner.state().stack.is_empty(),
        "the as-choice must not use the stack"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("choosing P1 must be accepted");

    // Drain anything the flushed event collected (Bow has no "when" trigger,
    // but the drain proves the apply settled normally).
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt after the as-choice: {other:?}"),
        }
    }

    // CR 607.2d: the chosen player is stored on the scheme for the linked
    // "the chosen player" static.
    let bow = scheme_object_id(&runner);
    assert!(
        runner.state().objects[&bow]
            .chosen_attributes
            .contains(&ChosenAttribute::Player(P1)),
        "Bow must store P1 as its chosen player, got {:?}",
        runner.state().objects[&bow].chosen_attributes
    );
    assert!(
        runner.state().stack.is_empty(),
        "no choice trigger may remain on the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at priority, got {:?}",
        runner.state().waiting_for
    );
}
