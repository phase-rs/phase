//! CR 800.4j + CR 117.4: once the active player has left the game, nobody is granted priority in
//! its place where a pass completes the round, and a departed player's pass does not count.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P3: PlayerId = PlayerId(3);

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&t| ManaUnit::new(t, ObjectId(0), false, vec![]))
        .collect()
}

fn pass(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass");
}

fn concede(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Concede { player_id: player })
        .expect("CR 104.3a: a player may concede at any time");
}

fn priority_seat(runner: &GameRunner) -> Option<PlayerId> {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => Some(player),
        _ => None,
    }
}

fn eliminated(state: &GameState, player: PlayerId) -> bool {
    state.players[player.0 as usize].is_eliminated
}

/// Four players, P0 active at its precombat main phase at 3 life.
fn free_for_all() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 3);
    for seat in 0..4u8 {
        scenario.with_library_top(PlayerId(seat), &["Filler A", "Filler B", "Filler C"]);
    }
    scenario
}

/// CR 800.4j + CR 117.4: once the active player has left, a pass that would hand it priority
/// resolves the top object, and then ends the step.
#[test]
fn a_departed_active_players_priority_resolves_then_ends_the_step() {
    let db = shared_card_db().expect("card db");
    let mut scenario = free_for_all();
    let opt = scenario.add_real_card(P1, "Opt", Zone::Hand, db);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(&[ManaType::Blue, ManaType::Red]));
    let mut runner = scenario.build();

    pass(&mut runner);
    runner.cast(opt).commit();
    runner.cast(bolt).target_player(P0).commit();
    for _ in 0..4 {
        pass(&mut runner);
    }
    assert!(eliminated(runner.state(), P0));
    assert_eq!(priority_seat(&runner), Some(P1));
    assert_eq!(runner.stack_names(), ["Opt"]);

    for _ in 0..3 {
        pass(&mut runner);
    }
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ScryChoice { player: P1, .. }
    ));
    let hand = runner.state().players[1].hand.len();
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("scry");
    assert_eq!(priority_seat(&runner), Some(P1));
    assert_eq!(runner.state().players[1].hand.len(), hand + 1);
}

/// CR 800.4a + CR 117.4: a player who passed and then left is not among the players who pass
/// in succession.
#[test]
fn a_departed_players_pass_does_not_complete_the_round() {
    let mut runner = free_for_all().build();
    pass(&mut runner);
    pass(&mut runner);
    concede(&mut runner, P0);
    pass(&mut runner);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(priority_seat(&runner), Some(P3));
    pass(&mut runner);
    assert_ne!(runner.state().phase, Phase::PreCombatMain);
}

/// An Archenemy game with P1, of the archenemy's opponents' team, active at its precombat main
/// phase with priority.
fn archenemy(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.turn_number = 2;
    state.active_player = P1;
    state.phase = Phase::PreCombatMain;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    state.priority_passes.clear();
    runner
}

fn archenemy_board() -> GameScenario {
    GameScenario::new_with_format(FormatConfig::archenemy(), 4, 7)
}

/// CR 805.5b: a team's pass stays the team's when the player who made it leaves and a teammate
/// remains.
#[test]
fn a_team_keeps_its_pass_when_its_representative_leaves() {
    let mut runner = archenemy(archenemy_board());
    pass(&mut runner);
    assert_eq!(priority_seat(&runner), Some(P0));
    assert_eq!(
        runner
            .state()
            .priority_passes
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [P1]
    );
    concede(&mut runner, P1);
    pass(&mut runner);
    assert_ne!(runner.state().phase, Phase::PreCombatMain);
}
