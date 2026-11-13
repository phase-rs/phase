//! CR 903.12h: Brawl games do not use CR 704.6c's 21 commander-damage loss.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CommanderDamageEntry, GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const GHALTA_PRIMAL_HUNGER: &str = "This spell costs {X} less to cast, where X is the total power of creatures you control.\nTrample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)";

/// P1 has already taken 9 combat damage from P0's Ghalta commander, at
/// precombat main.
fn ghalta_with_9_commander_damage(format: FormatConfig) -> (GameRunner, ObjectId) {
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
    (runner, ghalta)
}

/// Ghalta attacks P1 unblocked for 12 more. Returns the state after combat
/// damage and state-based actions.
fn ghalta_attacks_unblocked(mut runner: GameRunner, ghalta: ObjectId) -> GameState {
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

fn ghalta_brings_commander_damage_to_21(format: FormatConfig) -> GameState {
    let (runner, ghalta) = ghalta_with_9_commander_damage(format);
    ghalta_attacks_unblocked(runner, ghalta)
}

/// The trusted and raw persisted shapes of `state`.
fn save_shapes(state: &GameState) -> [serde_json::Value; 2] {
    let trusted = serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap();
    let raw = trusted["state"].clone();
    [trusted, raw]
}

fn restore(save: serde_json::Value) -> Result<GameState, String> {
    serde_json::from_value::<PersistedGameState>(save)
        .map_err(|err| err.to_string())?
        .into_game_state()
        .map_err(|err| err.to_string())
}

fn format_config_mut(save: &mut serde_json::Value) -> &mut serde_json::Value {
    let pointer = if save.get("state").is_some() {
        "/state/format_config"
    } else {
        "/format_config"
    };
    save.pointer_mut(pointer).unwrap()
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

#[test]
fn legacy_brawl_save_resumes_without_commander_damage() {
    let mut failures = Vec::new();
    for format in [FormatConfig::brawl(), FormatConfig::historic_brawl()] {
        let (runner, ghalta) = ghalta_with_9_commander_damage(format.clone());
        let saved = runner.state().clone();
        for (shape, mut legacy) in ["trusted", "raw"].into_iter().zip(save_shapes(&saved)) {
            let label = format!("{:?} {shape}", format.format);
            format_config_mut(&mut legacy)["commander_damage_threshold"] = 21.into();
            let restored = match restore(legacy) {
                Ok(restored) => restored,
                Err(err) => {
                    failures.push(format!("{label}: {err}"));
                    continue;
                }
            };

            assert_eq!(restored.format_config, format, "{label}");
            assert_eq!(restored.turn_number, saved.turn_number, "{label}");
            assert_eq!(restored.phase, saved.phase, "{label}");
            assert_eq!(restored.commander_damage, saved.commander_damage, "{label}");
            for player in [0, 1] {
                assert_eq!(
                    restored.players[player].life, saved.players[player].life,
                    "{label}"
                );
            }
            assert!(restored.battlefield.contains(&ghalta), "{label}");

            let state = ghalta_attacks_unblocked(GameRunner::from_state(restored), ghalta);
            assert_eq!(state.players[1].life, 25 - 12, "{label}");
            assert!(!state.players[1].is_eliminated, "{label}");
            assert!(
                !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
                "{label}"
            );
        }
    }
    assert!(
        failures.is_empty(),
        "legacy saves must restore: {failures:#?}"
    );
}

/// Only persisted saves are migrated; network ingress stays strict.
#[test]
fn network_ingress_still_rejects_a_brawl_commander_damage_threshold() {
    for format in [FormatConfig::brawl(), FormatConfig::historic_brawl()] {
        let label = format.format;
        let (runner, _) = ghalta_with_9_commander_damage(format);
        let [_, mut raw] = save_shapes(runner.state());
        assert!(serde_json::from_value::<FormatConfig>(raw["format_config"].clone()).is_ok());
        assert!(serde_json::from_value::<GameState>(raw.clone()).is_ok());
        raw["format_config"]["commander_damage_threshold"] = 21.into();
        for err in [
            serde_json::from_value::<FormatConfig>(raw["format_config"].clone())
                .map(drop)
                .unwrap_err(),
            serde_json::from_value::<GameState>(raw)
                .map(drop)
                .unwrap_err(),
        ] {
            assert!(
                err.to_string().contains("commander_damage_threshold"),
                "{label:?}: {err}"
            );
        }
    }
}

#[test]
fn persisted_commander_save_keeps_its_commander_damage_threshold() {
    let (runner, _) = ghalta_with_9_commander_damage(FormatConfig::commander());
    for save in save_shapes(runner.state()) {
        assert_eq!(
            restore(save)
                .unwrap()
                .format_config
                .commander_damage_threshold,
            Some(21)
        );
    }
}
