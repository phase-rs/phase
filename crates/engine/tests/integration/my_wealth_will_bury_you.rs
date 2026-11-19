//! My Wealth Will Bury You: Treasure count with a floor of four.
//!
//! Oracle: "When you set this scheme in motion, create a number of Treasure
//! tokens equal to the number of artifacts and enchantments your opponents
//! control. If they control fewer than four artifacts and enchantments,
//! create four Treasure tokens instead. (...)"
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger creates the tokens.
//! CR 614.1a + CR 608.2c: the second sentence is a conditional override —
//! the base count and the floor never both run (CR 614.6). CR 107.1: "they"
//! is the opponents' collective board, compared strictly against four.
//!
//! Revert-to-red: drop the fewer-than arm — the document gaps
//! (scheme_face asserts). Run the base instead of the floor — leg A makes 2
//! Treasure, not 4. Run the floor instead of the base — leg B makes 4, not 5.

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

const MY_WEALTH_WILL_BURY_YOU: &str = "When you set this scheme in motion, create a number of Treasure tokens equal to the number of artifacts and enchantments your opponents control. If they control fewer than four artifacts and enchantments, create four Treasure tokens instead. (Treasure tokens are artifacts with \"{T}, Sacrifice this token: Add one mana of any color.\")";

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

/// Resolve every pending trigger with no choices (token creation is prompt-free).
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

/// Set the scheme in motion with `artifacts`/`enchantments` on the opponent's
/// board (plus one decoy artifact of the archenemy's own) and return how many
/// Treasure tokens the archenemy controls after the trigger resolves.
fn treasure_made(artifacts: usize, enchantments: usize) -> usize {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    for i in 0..artifacts {
        scenario.add_artifact_from_oracle(P1, &format!("Foe Rock {i}"), "");
    }
    for i in 0..enchantments {
        scenario.add_enchantment_from_oracle(P1, &format!("Foe Aura {i}"), "");
    }
    // Own permanents must not feed the count.
    scenario.add_artifact_from_oracle(P0, "Own Rock", "");
    let mut runner = scenario.build();

    let face = scheme_face("My Wealth Will Bury You", MY_WEALTH_WILL_BURY_YOU);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    drain_triggers(&mut runner);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );

    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield
                && obj.controller == P0
                && obj.name == "Treasure"
        })
        .count()
}

#[test]
fn my_wealth_will_bury_you_floors_at_four_treasure() {
    // Two opposing permanents (< 4): the floor replaces the count.
    assert_eq!(
        treasure_made(1, 1),
        4,
        "fewer than four opposing artifacts/enchantments must still make four Treasure"
    );
}

#[test]
fn my_wealth_will_bury_you_counts_opposing_permanents() {
    // Five opposing permanents (>= 4): the count runs (the archenemy's own
    // artifact must not inflate it to six).
    assert_eq!(
        treasure_made(3, 2),
        5,
        "five opposing artifacts/enchantments must make five Treasure"
    );
}
