//! Your Plans Mean Nothing: mass hand-discard, per-seat minus-one draws, self-discard seven.
//!
//! Oracle: "When you set this scheme in motion, any number of target players
//! each discard their hands. Each opponent draws cards equal to the number of
//! cards that player discarded minus one. Then if you discarded a card this
//! way, draw seven cards."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger fires on turn 1.
//! CR 608.2c + CR 701.9a + CR 109.5: each targeted player discards their whole
//! hand in one event; sentence 2 counts per-seat discards (`FilteredTracked-
//! SetSize{Owned{ScopedPlayer}, Discarded}`) minus one, clamped at zero — an
//! opponent who discarded nothing draws nothing (the card's ruling).
//! CR 608.2c: sentence 3 gates on the controller's own discard events.
//!
//! Revert-to-red: drop the `number of cards that player discarded` arm — the
//! document gaps (scheme_face asserts). Drop the trailing-minus Offset — the
//! minus-one assertions fail. Drop the Offset clamp — the zero-draw opponent
//! draws (hand assertion fails). Drop the S3 gate — the untargeted-controller
//! leg draws seven.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const YOUR_PLANS_MEAN_NOTHING: &str = "When you set this scheme in motion, any number of target players each discard their hands. Each opponent draws cards equal to the number of cards that player discarded minus one. Then if you discarded a card this way, draw seven cards.";

const P2: PlayerId = PlayerId(2);

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
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
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

fn library_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Library && obj.owner == player)
        .count()
}

/// Build the 3-player game: P0 (archenemy) is dealt 2 (3 after the draw
/// step), P1 holds 3, P2 holds 1; every library holds 10. Returns the runner
/// parked at precombat main with the scheme face-up.
fn setup_plans() -> GameRunner {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    for i in 0..2 {
        scenario.add_card_to_hand(P0, &format!("P0 Hand {i}"));
    }
    for i in 0..3 {
        scenario.add_card_to_hand(P1, &format!("P1 Hand {i}"));
    }
    scenario.add_card_to_hand(P2, "P2 Hand 0");
    let mut runner = scenario.build();

    let face = scheme_face("Your Plans Mean Nothing", YOUR_PLANS_MEAN_NOTHING);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    // P0 draws for the turn on the way through the draw step.
    assert_eq!(hand_size(&runner, P0), 3, "reach guard: P0's hand");
    assert_eq!(hand_size(&runner, P1), 3, "reach guard: P1's hand");
    assert_eq!(hand_size(&runner, P2), 1, "reach guard: P2's hand");
    runner
}

/// Resolve the trigger with `targets` as the any-number pick. Returns the
/// number of target-selection prompts seen.
fn resolve_with_targets(runner: &mut GameRunner, targets: Vec<TargetRef>) -> usize {
    let mut target_prompts = 0;
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
            WaitingFor::TriggerTargetSelection { .. } => {
                target_prompts += 1;
                runner
                    .act(GameAction::SelectTargets {
                        targets: targets.clone(),
                    })
                    .expect("any-number pick must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt resolving the scheme: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    target_prompts
}

#[test]
fn your_plans_mean_nothing_split_table_minus_one_and_seven() {
    let mut runner = setup_plans();

    // P0 (self) + P1 discard whole hands; P2 sits out (zero-draw ruling).
    let target_prompts = resolve_with_targets(
        &mut runner,
        vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    );
    assert_eq!(target_prompts, 1, "the any-number pick must prompt once");
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P1),
        2,
        "P1 discarded 3, so P1 must draw 3 minus one"
    );
    assert_eq!(
        hand_size(&runner, P0),
        7,
        "P0 discarded (self-target), so P0 must draw seven"
    );
    assert_eq!(
        hand_size(&runner, P2),
        1,
        "P2 discarded nothing, so P2 must draw nothing (0 minus one clamps)"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn your_plans_mean_nothing_opponent_only_skips_the_seven() {
    let mut runner = setup_plans();

    // Only P1 discards: P1 draws 3 minus one, the S3 self-gate stays shut.
    let target_prompts = resolve_with_targets(&mut runner, vec![TargetRef::Player(P1)]);
    assert_eq!(target_prompts, 1, "the any-number pick must prompt once");
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P1),
        2,
        "P1 discarded 3, so P1 must draw 3 minus one"
    );
    assert_eq!(
        hand_size(&runner, P0),
        3,
        "P0 discarded nothing, so the seven-draw gate must stay shut"
    );
    assert_eq!(
        library_size(&runner, P0),
        9,
        "P0's library must be untouched when the gate fails"
    );
    assert_eq!(
        hand_size(&runner, P2),
        1,
        "P2 discarded nothing, so P2 must draw nothing"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn your_plans_mean_nothing_empty_pick_changes_nothing() {
    let mut runner = setup_plans();

    // Zero targets: nobody discards, nobody draws, libraries untouched.
    let target_prompts = resolve_with_targets(&mut runner, vec![]);
    assert_eq!(target_prompts, 1, "even the empty pick must prompt once");
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P0), 3, "nobody targeted: P0's hand");
    assert_eq!(hand_size(&runner, P1), 3, "nobody targeted: P1's hand");
    assert_eq!(hand_size(&runner, P2), 1, "nobody targeted: P2's hand");
    assert_eq!(
        library_size(&runner, P0),
        9,
        "nobody targeted: P0's library"
    );
    assert_eq!(
        library_size(&runner, P1),
        10,
        "nobody targeted: P1's library"
    );
    assert_eq!(
        library_size(&runner, P2),
        10,
        "nobody targeted: P2's library"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
