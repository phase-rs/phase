//! Runtime regression for issue #11105 — Jadar's intervening-if condition must
//! be checked when the end-step trigger would fire, not only while its token
//! effect resolves.
//!
//! Oracle text: "At the beginning of your end step, if you control no
//! creatures with decayed, create a 2/2 black Zombie creature token with
//! decayed."
//!
//! CR 603.4: An intervening-if condition is checked both as the trigger would
//! trigger and as it resolves. A decayed creature therefore suppresses the
//! trigger entirely; without one, Jadar creates exactly one token.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const JADAR: &str = "At the beginning of your end step, if you control no creatures with decayed, create a 2/2 black Zombie creature token with decayed.";
// Synthetic support spell: no printed card in the current card pool grants
// decayed to a target creature. Its oracle shape mirrors the continuous-grant
// path already exercised by Wilhelt's layer-6 decayed regression.
const GRANT_DECAYED: &str = "Target creature gains decayed until end of turn.";

fn jadar_board(has_decayed_creature: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jadar, Ghoulcaller of Nephalia", 1, 1, JADAR)
        .id();

    if has_decayed_creature {
        scenario
            .add_creature(P0, "Decayed Zombie", 2, 2)
            .with_keyword(Keyword::Decayed);
    }

    scenario.build()
}

fn reach_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).expect("declare no attackers");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End);
}

fn zombie_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.controller == P0
                && object.name == "Zombie"
                && object.has_keyword(&Keyword::Decayed)
        })
        .count()
}

#[test]
fn jadar_does_not_trigger_with_a_decayed_creature() {
    let mut runner = jadar_board(true);
    reach_end_step(&mut runner);

    assert!(!runner
        .stack_names()
        .iter()
        .any(|name| name == "Jadar, Ghoulcaller of Nephalia"));
    runner.advance_until_stack_empty();
    assert_eq!(zombie_token_count(&runner), 0);
}

#[test]
fn jadar_creates_one_decayed_zombie_without_a_decayed_creature() {
    let mut runner = jadar_board(false);
    reach_end_step(&mut runner);

    assert!(runner
        .stack_names()
        .iter()
        .any(|name| name == "Jadar, Ghoulcaller of Nephalia"));
    runner.advance_until_stack_empty();
    assert_eq!(zombie_token_count(&runner), 1);
}

#[test]
fn jadar_rechecks_the_condition_when_the_trigger_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jadar, Ghoulcaller of Nephalia", 1, 1, JADAR)
        .id();
    let target = scenario.add_creature(P0, "Zombie", 2, 2).id();
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Grant Decayed", true, GRANT_DECAYED)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    reach_end_step(&mut runner);
    assert!(runner
        .stack_names()
        .iter()
        .any(|name| name == "Jadar, Ghoulcaller of Nephalia"));

    runner.cast(grant).target_object(target).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zombie_token_count(&runner), 0);
}
