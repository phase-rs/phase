//! CR 903.12h: Brawl games do not use CR 704.6c's 21 commander-damage loss.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CommanderDamageEntry, GameState, WaitingFor};
use engine::types::phase::Phase;

const GHALTA_PRIMAL_HUNGER: &str = "This spell costs {X} less to cast, where X is the total power of creatures you control.\nTrample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)";

/// P1 has already taken 9 combat damage from P0's Ghalta commander; Ghalta
/// attacks P1 unblocked for 12 more. Returns the state after combat damage
/// and state-based actions.
fn ghalta_brings_commander_damage_to_21(format: FormatConfig) -> GameState {
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ghalta = scenario
        .add_creature_from_oracle(P0, "Ghalta, Primal Hunger", 12, 12, GHALTA_PRIMAL_HUNGER)
        .commander()
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .commander_damage
        .push(CommanderDamageEntry {
            player: P1,
            commander: ghalta,
            damage: 9,
        });

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ghalta, AttackTarget::Player(P1))])
        .unwrap();
    runner.pass_both_players();
    runner.pass_both_players();

    let state = runner.state().clone();
    let tally: u32 = state
        .commander_damage
        .iter()
        .filter(|entry| entry.player == P1 && entry.commander == ghalta)
        .map(|entry| entry.damage)
        .sum();
    assert_eq!(tally, 21, "combat damage must reach the commander tally");
    state
}

#[test]
fn brawl_family_ignores_the_commander_damage_loss() {
    for format in [FormatConfig::brawl(), FormatConfig::historic_brawl()] {
        let label = format.format;
        let state = ghalta_brings_commander_damage_to_21(format);
        assert_eq!(state.players[1].life, 25 - 12, "{label:?}");
        assert!(!state.players[1].is_eliminated, "{label:?}");
        assert!(
            !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
            "{label:?}"
        );
    }
}

#[test]
fn commander_loses_to_21_commander_combat_damage() {
    let state = ghalta_brings_commander_damage_to_21(FormatConfig::commander());
    assert_eq!(state.players[1].life, 40 - 12);
    assert!(state.players[1].is_eliminated);
}
