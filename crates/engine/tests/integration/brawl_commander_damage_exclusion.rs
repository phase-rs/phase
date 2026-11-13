//! CR 903.12h: Brawl games do not use CR 704.6c's 21 commander-damage loss.

use std::io::BufReader;
use std::sync::Arc;

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::deck_loading::{DeckList, PlayerDeckList};
use engine::game::engine::apply;
use engine::game::replay::{reconstruct_initial_state, ReplayPlayer};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, CommanderDamageEntry, GameState, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::match_config::MatchConfig;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replay::{ReplayHeader, ReplayLog};
use engine::types::zones::Zone;
use flate2::read::GzDecoder;

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

fn replay_header(format_config: FormatConfig, deck_data: Option<DeckList>) -> ReplayHeader {
    ReplayHeader {
        format_config,
        match_config: MatchConfig::default(),
        player_count: 2,
        first_player: Some(0),
        seed: 11,
        deck_data,
    }
}

/// `log` as exported JSON, with the header's commander-damage threshold set to `threshold`.
fn with_header_threshold(log: &ReplayLog, threshold: serde_json::Value) -> String {
    let mut json = serde_json::to_value(log).unwrap();
    json["header"]["format_config"]["commander_damage_threshold"] = threshold;
    json.to_string()
}

fn load_replay(json: &str, db: Option<&Arc<CardDatabase>>) -> Result<ReplayPlayer, String> {
    let log: ReplayLog = serde_json::from_str(json).map_err(|err| err.to_string())?;
    ReplayPlayer::load(log, db).map_err(|err| format!("{err:?}"))
}

#[test]
fn legacy_brawl_replay_loads_with_its_recorded_threshold() {
    for format in [FormatConfig::brawl(), FormatConfig::historic_brawl()] {
        for version in [2, 3, 4] {
            let label = format!("{:?} v{version}", format.format);
            let mut log = ReplayLog::new(replay_header(format.clone(), None));
            log.format_version = Some(version);

            let mut current = load_replay(&serde_json::to_string(&log).unwrap(), None)
                .unwrap_or_else(|err| panic!("{label}: {err}"));
            assert_eq!(
                current
                    .seek(0)
                    .unwrap()
                    .format_config
                    .commander_damage_threshold,
                None,
                "{label}"
            );

            let mut legacy = load_replay(&with_header_threshold(&log, 21.into()), None)
                .unwrap_or_else(|err| panic!("{label}: {err}"));
            let state = legacy.seek(0).unwrap();
            assert_eq!(state.format_config.format, format.format, "{label}");
            assert_eq!(
                state.format_config.commander_damage_threshold,
                Some(21),
                "{label}"
            );

            assert!(
                serde_json::from_str::<ReplayLog>(&with_header_threshold(&log, 0.into())).is_err(),
                "{label}: no engine ever admitted a zero threshold"
            );
        }
    }
}

fn fixture_db() -> Arc<CardDatabase> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/integration_cards.json.gz");
    let file = std::fs::File::open(path).expect("fixture opens");
    Arc::new(
        CardDatabase::from_export_reader(GzDecoder::new(BufReader::new(file)))
            .expect("fixture loads"),
    )
}

fn mountain_deck(commander: Option<&str>, mountains: usize) -> PlayerDeckList {
    PlayerDeckList {
        main_deck: vec!["Mountain".to_string(); mountains],
        commander: commander.map(str::to_string).into_iter().collect(),
        ..Default::default()
    }
}

/// The actions a simple aggressive pilot tries, in preference order.
fn candidate_actions(state: &GameState) -> Vec<(PlayerId, GameAction)> {
    match &state.waiting_for {
        WaitingFor::MulliganDecision { pending, .. } => vec![(
            pending[0].player,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        )],
        WaitingFor::Priority { player } => {
            let player = *player;
            let land = state.players[player.0 as usize]
                .hand
                .iter()
                .next()
                .map(|&id| GameAction::PlayLand {
                    object_id: id,
                    card_id: state.objects[&id].card_id,
                });
            let commander = state
                .objects
                .iter()
                .filter(|(_, obj)| obj.zone == Zone::Command && obj.owner == player)
                .map(|(&id, obj)| GameAction::CastSpell {
                    object_id: id,
                    card_id: obj.card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                });
            land.into_iter()
                .chain(commander)
                .chain([GameAction::PassPriority])
                .map(|action| (player, action))
                .collect()
        }
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            ..
        } => vec![(
            *player,
            GameAction::DeclareAttackers {
                attacks: valid_attacker_ids
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            },
        )],
        WaitingFor::DeclareBlockers { player, .. } => vec![(
            *player,
            GameAction::DeclareBlockers {
                assignments: vec![],
            },
        )],
        other => panic!("unexpected decision {other:?}"),
    }
}

/// A Brawl game recorded while Brawl still carried a commander-damage threshold
/// of 21: Zurgo Bellstriker attacks a player with no creatures until the game ends.
fn record_legacy_brawl_game(db: &Arc<CardDatabase>) -> (ReplayLog, GameState) {
    let mut legacy = FormatConfig::brawl();
    legacy.commander_damage_threshold = Some(21);
    let decks = DeckList {
        player: mountain_deck(Some("Zurgo Bellstriker"), 59),
        opponent: mountain_deck(None, 60),
        ..Default::default()
    };
    let header = replay_header(legacy, Some(decks));
    let mut live = reconstruct_initial_state(&header, Some(db)).unwrap();
    let mut log = ReplayLog::new(header);
    while !matches!(live.waiting_for, WaitingFor::GameOver { .. }) {
        assert!(log.actions.len() < 3000, "the recorded game must end");
        let (actor, action) = candidate_actions(&live)
            .into_iter()
            .find(|(actor, action)| apply(&mut live, *actor, action.clone()).is_ok())
            .unwrap_or_else(|| panic!("no action applied at {:?}", live.waiting_for));
        log.push_action(actor, action);
    }
    (log, live)
}

#[test]
fn legacy_brawl_replay_reconstructs_its_commander_damage_loss() {
    let db = fixture_db();
    let (log, live) = record_legacy_brawl_game(&db);
    assert!(live.players[1].is_eliminated);

    let mut recorded = load_replay(&serde_json::to_string(&log).unwrap(), Some(&db))
        .expect("an exported legacy Brawl replay loads");
    let end = recorded.len();
    let state = recorded.seek(end).unwrap();
    assert!(matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    assert!(state.players[1].is_eliminated);
    assert!(state.players[1].life > 0, "the loss is not from life");
    let tally: u32 = state
        .commander_damage
        .iter()
        .filter(|entry| entry.player == P1)
        .map(|entry| entry.damage)
        .sum();
    assert!(tally >= 21, "CR 704.6c: {tally} commander damage");

    let mut current = load_replay(
        &with_header_threshold(&log, serde_json::Value::Null),
        Some(&db),
    )
    .expect("the same replay loads under current Brawl rules");
    let end = current.len();
    let state = current.seek(end).unwrap();
    assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    assert!(!state.players[1].is_eliminated);
}
