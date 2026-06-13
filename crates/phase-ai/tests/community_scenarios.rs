use std::path::Path;
use std::process::Command;

use engine::types::actions::GameAction;
use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::Deserialize;

#[derive(Deserialize)]
struct CommunityScenario {
    id: String,
    thread_id: String,
    archive: String,
    expected_action_type: String,
}

#[test]
fn community_ai_scenarios_choose_expected_action_type() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/scenarios");
    let specs: Vec<CommunityScenario> = serde_json::from_str(include_str!(
        "../fixtures/scenarios/community-scenarios.json"
    ))
    .expect("scenario specs deserialize");

    for spec in specs {
        let raw = read_zipped_json(&fixture_dir.join(&spec.archive));
        let state = load_saved_game_state(&raw).unwrap_or_else(|err| {
            panic!(
                "{} ({}) did not deserialize: {err}",
                spec.id, spec.thread_id
            )
        });
        let player = state
            .waiting_for
            .acting_player()
            .unwrap_or(state.active_player);
        let config = create_config_for_players(
            AiDifficulty::Medium,
            Platform::Native,
            state.players.len() as u8,
        )
        .into_measurement(42);
        let mut rng = StdRng::seed_from_u64(42);
        let action = choose_action(&state, player, &config, &mut rng)
            .unwrap_or_else(|| panic!("{} ({}) returned no action", spec.id, spec.thread_id));
        let action_type: &'static str = action_type(action);

        assert_eq!(
            action_type, spec.expected_action_type,
            "{} ({}) chose unexpected action type",
            spec.id, spec.thread_id
        );
    }
}

fn read_zipped_json(path: &Path) -> String {
    let output = Command::new("unzip")
        .arg("-p")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run unzip for {}: {err}", path.display()));

    assert!(
        output.status.success(),
        "unzip failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .unwrap_or_else(|err| panic!("{} was not utf-8 json: {err}", path.display()))
}

fn action_type(action: GameAction) -> &'static str {
    action.into()
}
