//! Your Own Face Mocks You: choose-up-to-two copy tokens + fewer-than-two top-up.
//!
//! Oracle: "When you set this scheme in motion, choose up to two target
//! creatures your opponents control. For each one, create a token that's a
//! copy of it. If you created fewer than two tokens this way, create a
//! number of 4/4 colorless Scarecrow artifact creature tokens with vigilance
//! equal to the difference."
//!
//! CR 115.1d + CR 601.2c: "choose up to two target" declares two optional
//! slots; "for each one" distributes the copy over the chosen targets.
//! CR 608.2c + CR 701.7a: the tail gates on how many tokens the copy step
//! created and tops up to two with Scarecrows.
//!
//! Revert-to-red: drop the "for each one" strip — sentence 2 gaps
//! (scheme_face asserts). Drop the created-this-way arm — the tail gate
//! vanishes and the difference unbinds (asserted counts fail). Stop
//! publishing created tokens — the gate reads a stale set (top-up counts
//! fail). Skip a slot — the prompt count fails.

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
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const YOUR_OWN_FACE_MOCKS_YOU: &str = "When you set this scheme in motion, choose up to two target creatures your opponents control. For each one, create a token that's a copy of it. If you created fewer than two tokens this way, create a number of 4/4 colorless Scarecrow artifact creature tokens with vigilance equal to the difference.";

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

/// Names of P0's battlefield permanents (P0 starts with an empty board, so
/// everything here is a token this scheme created).
fn p0_battlefield_names(runner: &GameRunner) -> Vec<String> {
    let mut names: Vec<String> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.controller == P0)
        .map(|obj| obj.name.clone())
        .collect();
    names.sort();
    names
}

/// Build the 3-player game: P1 holds two distinctly-named creatures.
/// Returns the runner parked at precombat main with the scheme face-up, plus
/// the two creature ids.
fn setup_face() -> (GameRunner, ObjectId, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let drake = scenario.add_creature(P1, "Cloud Drake", 2, 2).flying().id();
    let mut runner = scenario.build();

    let face = scheme_face("Your Own Face Mocks You", YOUR_OWN_FACE_MOCKS_YOU);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    (runner, bears, drake)
}

/// Resolve the trigger, choosing `picks` (`None` declines that slot).
/// Returns the number of target-selection prompts seen.
fn resolve_with_picks(
    runner: &mut GameRunner,
    bears: ObjectId,
    drake: ObjectId,
    picks: &[Option<ObjectId>],
) -> usize {
    let mut target_prompts = 0;
    let mut pick_idx = 0;
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
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(slot.optional, "choose-up-to-two slots are optional");
                let legals = &slot.legal_targets;
                assert!(
                    legals.contains(&TargetRef::Object(bears))
                        && legals.contains(&TargetRef::Object(drake)),
                    "each slot must offer both creatures, got {legals:?}"
                );
                let choice = picks[pick_idx].map(TargetRef::Object);
                pick_idx += 1;
                target_prompts += 1;
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose target must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt resolving the face: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    target_prompts
}

#[test]
fn your_own_face_mocks_you_two_copies_no_scarecrows() {
    let (mut runner, bears, drake) = setup_face();

    let target_prompts = resolve_with_picks(&mut runner, bears, drake, &[Some(bears), Some(drake)]);
    assert_eq!(target_prompts, 2, "one prompt per up-to-two slot");
    runner.advance_until_stack_empty();

    assert_eq!(
        p0_battlefield_names(&runner),
        vec!["Cloud Drake".to_string(), "Grizzly Bears".to_string()],
        "two copies and no top-up"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn your_own_face_mocks_you_one_copy_one_scarecrow() {
    let (mut runner, bears, drake) = setup_face();

    let target_prompts = resolve_with_picks(&mut runner, bears, drake, &[Some(bears), None]);
    assert_eq!(target_prompts, 2, "the declined slot must still prompt");
    runner.advance_until_stack_empty();

    assert_eq!(
        p0_battlefield_names(&runner),
        vec!["Grizzly Bears".to_string(), "Scarecrow".to_string()],
        "one copy plus one top-up Scarecrow"
    );
    let crow = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|obj| obj.controller == P0 && obj.name == "Scarecrow")
        .expect("the Scarecrow must be on the battlefield");
    assert_eq!((crow.power, crow.toughness), (Some(4), Some(4)));
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn your_own_face_mocks_you_zero_copies_two_scarecrows() {
    let (mut runner, bears, drake) = setup_face();

    // CR 601.2c: declining the first optional slot of the up-to-two phrase
    // announces no more targets — the engine auto-skips the second slot
    // instead of forcing another Skip click.
    let target_prompts = resolve_with_picks(&mut runner, bears, drake, &[None]);
    assert_eq!(target_prompts, 1, "declining the first slot ends selection");
    runner.advance_until_stack_empty();

    assert_eq!(
        p0_battlefield_names(&runner),
        vec!["Scarecrow".to_string(), "Scarecrow".to_string()],
        "zero copies plus two top-up Scarecrows"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
