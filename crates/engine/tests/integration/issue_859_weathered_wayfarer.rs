//! Issue #859: Weathered Wayfarer cannot activate when an opponent controls
//! more lands. Regression for existential "an opponent controls more [type]
//! than you" activation restrictions (CR 109.4).

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const WEATHERED_WAYFARER: &str = "\
{W}, {T}: Search your library for a land card, reveal it, put it into your hand, \
then shuffle. Activate only if an opponent controls more lands than you.";

#[test]
fn weathered_wayfarer_activates_when_opponent_has_more_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario
        .add_creature_from_oracle(P0, "Weathered Wayfarer", 1, 1, WEATHERED_WAYFARER)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Red);

    let runner = scenario.build();
    assert!(
        can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "opponent controls three lands vs controller's one"
    );

    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == wayfarer
        )),
        "legal_actions must offer Weathered Wayfarer's search ability"
    );
}

#[test]
fn weathered_wayfarer_blocked_when_land_counts_tied() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario
        .add_creature_from_oracle(P0, "Weathered Wayfarer", 1, 1, WEATHERED_WAYFARER)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let runner = scenario.build();
    assert!(
        !can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "equal land counts must block activation"
    );
}

/// "An opponent" is existential — a single opponent must individually control
/// more lands than you. Two opponents with one land each (combined 2 > your 1)
/// must NOT satisfy the restriction. This is the only scenario where the
/// existential parse diverges from the old aggregate-opponent-count parse, so
/// this test is the discriminating regression pin for this fix.
#[test]
fn weathered_wayfarer_blocked_when_only_combined_opponents_exceed() {
    let mut scenario = GameScenario::new_n_player(3, 99);
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario
        .add_creature_from_oracle(P0, "Weathered Wayfarer", 1, 1, WEATHERED_WAYFARER)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P2, ManaColor::Green);

    let runner = scenario.build();
    assert!(
        !can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "no single opponent controls more lands than the controller; \
         combined opponent land counts must not satisfy the restriction"
    );
}
