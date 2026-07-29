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
        // On `saheeli-legend-loop` the expectation is `PlayLand`, which RESTORES the
        // value recorded when the state was captured (c7b5044f62). #6637
        // (e50ae6b6cb) flipped it to `CastSpell` when the first-land fast path was
        // removed; the mana-development offset in `eval::evaluate_features` restores
        // the original.
        //
        // Measured on this saved state, not inferred from the board:
        //   without the offset — the AI casts *Dauntless Escort* (a creature) and
        //     passes, WASTING the land drop for the whole turn;
        //   with it — the AI plays the land and then casts *Commander's Sphere*
        //     (a mana rock).
        // Name the second action rather than saying "still casts a spell": that it
        // is a mana ROCK is the whole basis of the rock-over-card-draw scope
        // correction tracked separately, and a reader who only sees "a spell"
        // cannot reconstruct it.
        //
        // Note the previous `CastSpell` expectation was satisfied by *Dauntless
        // Escort*, NOT by Harmonize. Harmonize is castable here but is chosen by
        // neither version, so "there is a castable Harmonize" describes the board,
        // not the AI's pick — which also means this state does NOT isolate the
        // offset for a rock-vs-draw comparison: the baseline never reaches the
        // post-land-drop position where that choice is made.
        //
        // CR 305.1: playing a land is a special action and doesn't use the stack, so
        // taking the land drop first cannot cost the player a spell this turn.
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
