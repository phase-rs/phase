//! Issue #1539: Queen Marchesa must only create an Assassin token at your
//! upkeep when an opponent is the monarch (CR 603.4 intervening-if).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::PlayerId;

const QUEEN_MARCHESA_UPKEEP: &str =
    "At the beginning of your upkeep, if an opponent is the monarch, create a 1/1 black Assassin creature token with haste.";

fn build_runner(monarch: Option<PlayerId>) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature(P0, "Queen Marchesa", 3, 3)
        .from_oracle_text(QUEEN_MARCHESA_UPKEEP);
    let mut runner = scenario.build();
    runner.state_mut().monarch = monarch;
    runner
}

#[test]
fn queen_marchesa_skips_assassin_when_controller_is_monarch() {
    let mut runner = build_runner(Some(P0));
    let before = runner.battlefield_count(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.battlefield_count(P0),
        before,
        "Queen Marchesa must not create an Assassin when you are the monarch"
    );
}

#[test]
fn queen_marchesa_creates_assassin_when_opponent_is_monarch() {
    let mut runner = build_runner(Some(P1));
    let before = runner.battlefield_count(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert!(
        runner.battlefield_count(P0) > before,
        "Queen Marchesa must create an Assassin when an opponent is the monarch"
    );
}

#[test]
fn queen_marchesa_upkeep_trigger_parses_with_condition() {
    let parsed = parse_oracle_text(
        QUEEN_MARCHESA_UPKEEP,
        "Queen Marchesa",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("upkeep trigger");
    assert!(
        trigger.condition.is_some(),
        "intervening-if must be present"
    );
}
