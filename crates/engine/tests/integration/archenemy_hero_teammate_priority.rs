//! Archenemy Commander shares the 2HG turn shape: the heroes side takes a
//! shared team turn (`OneVsMany` + `SharedTeamTurns`), so priority parks on
//! the heroes' representative (P1) while P2/P3 must still be able to act.
//! Regression coverage that the CR 805.5a/805.5b fix applies here too.

use engine::ai_support;
use engine::game::engine as game_engine;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// P0 is the archenemy; P1/P2/P3 are heroes on a shared team turn parked
/// on P1, the side's first living member.
fn hero_turn_board() -> GameScenario {
    let mut sc = GameScenario::new_with_format(FormatConfig::archenemy_commander(), 4, 42);
    sc.at_phase(Phase::PreCombatMain);
    sc
}

fn park_heroes_priority(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
}

/// A non-representative hero sees their own legal actions while the
/// representative holds the heroes' priority window.
#[test]
fn hero_has_legal_actions_on_shared_hero_turn() {
    let mut sc = hero_turn_board();
    let land = sc.add_land_to_hand(P2, "Forest").id();
    let mut runner = sc.build();
    park_heroes_priority(&mut runner);

    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P2);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::PlayLand { object_id, .. } if *object_id == land
        )),
        "P2 must be offered their own land on the heroes' turn, got {actions:?}"
    );
}

/// A non-representative hero's land play is accepted through the
/// authenticated boundary while the representative holds priority.
#[test]
fn hero_land_play_accepted_on_shared_hero_turn() {
    let mut sc = hero_turn_board();
    let land = sc.add_land_to_hand(P2, "Forest").id();
    let mut runner = sc.build();
    park_heroes_priority(&mut runner);

    let card_id = runner.state().objects[&land].card_id;
    game_engine::apply(
        runner.state_mut(),
        P2,
        GameAction::PlayLand {
            object_id: land,
            card_id,
        },
    )
    .expect("hero land play must be accepted on the shared hero turn");
    assert_eq!(
        runner.state().objects[&land].zone,
        engine::types::zones::Zone::Battlefield
    );
}

/// CR 805.5b on the heroes side: one hero passing does not pass for the
/// side. All three living heroes must pass before priority leaves for the
/// archenemy.
#[test]
fn hero_pass_requires_all_heroes() {
    let sc = hero_turn_board();
    let mut runner = sc.build();
    park_heroes_priority(&mut runner);

    // First hero passes: the heroes still hold priority.
    game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
        .expect("representative pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1 || player == P2 || player == P3
        ),
        "heroes must retain priority until all pass, got {:?}",
        runner.state().waiting_for
    );

    // Second hero passes: still the heroes' window.
    game_engine::apply(runner.state_mut(), P2, GameAction::PassPriority)
        .expect("second hero pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1 || player == P2 || player == P3
        ),
        "heroes must retain priority until all pass, got {:?}",
        runner.state().waiting_for
    );

    // Final hero passes: priority goes to the archenemy.
    game_engine::apply(runner.state_mut(), P3, GameAction::PassPriority)
        .expect("final hero pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "archenemy must receive priority after all heroes pass, got {:?}",
        runner.state().waiting_for
    );
}
