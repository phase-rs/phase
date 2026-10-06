use std::path::Path;
use std::process::Command;

use engine::game::engine::apply_as_current;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use phase_ai::{choose_action, score_candidates};
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

#[test]
fn fertile_ground_beneficial_aura_targets_own_land_at_medium() {
    fertile_ground_checkpoint_target_choice(AiDifficulty::Medium);
}

#[test]
fn fertile_ground_beneficial_aura_targets_own_land_at_very_hard() {
    fertile_ground_checkpoint_target_choice(AiDifficulty::VeryHard);
}

fn fertile_ground_checkpoint_target_choice(difficulty: AiDifficulty) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/scenarios/fertile-ground-beneficial-aura-targeting.zip");
    let raw = read_zipped_json(&fixture);
    let capture: serde_json::Value = serde_json::from_str(&raw).expect("capture is json");
    let checkpoint = capture["turnCheckpoints"]
        .as_array()
        .and_then(|checkpoints| checkpoints.last())
        .expect("capture has a final turn checkpoint");
    let envelope = serde_json::json!({ "gameState": checkpoint }).to_string();
    let mut state = load_saved_game_state(&envelope).expect("checkpoint restores");

    assert_eq!(state.turn_number, 11, "positive guard: final checkpoint");
    assert_eq!(state.phase, Phase::End, "positive guard: end step");
    assert_eq!(
        state.objects[&engine::types::identifiers::ObjectId(71)].name,
        "Fertile Ground"
    );

    let config = create_config_for_players(difficulty, Platform::Native, state.players.len() as u8)
        .into_measurement(42);
    for _ in 0..20 {
        if state.turn_number == 12
            && state.phase == Phase::PreCombatMain
            && state.waiting_for.acting_player() == Some(PlayerId(1))
        {
            break;
        }
        apply_as_current(&mut state, GameAction::PassPriority)
            .expect("priority pass advances the captured game");
    }
    assert_eq!(state.turn_number, 12, "reached the reported turn");
    assert_eq!(
        state.phase,
        Phase::PreCombatMain,
        "reached the reported phase"
    );

    let forest = ObjectId(72);
    let forest_card_id = state.objects[&forest].card_id;
    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: forest,
            card_id: forest_card_id,
        },
    )
    .expect("play the reported Forest before casting Fertile Ground");

    let fertile_ground = ObjectId(71);
    let card_id = state.objects[&fertile_ground].card_id;
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: fertile_ground,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("the reported Fertile Ground cast is legal");

    let WaitingFor::TargetSelection {
        pending_cast,
        selection,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "Fertile Ground cast must reach target selection, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        pending_cast.object_id, fertile_ground,
        "reported source reached the prompt"
    );
    let player = state.waiting_for.acting_player().expect("target chooser");
    assert_eq!(player, PlayerId(1), "reported AI controls Fertile Ground");

    let mut own = Vec::new();
    let mut opposing = Vec::new();
    for target in &selection.current_legal_targets {
        let TargetRef::Object(id) = target else {
            continue;
        };
        let object = &state.objects[id];
        if object.controller == player {
            own.push(*id);
        } else {
            opposing.push(*id);
        }
    }
    assert!(!own.is_empty(), "reach guard: own land is legal");
    assert!(!opposing.is_empty(), "reach guard: opponent land is legal");

    let target_candidates: Vec<_> = engine::ai_support::candidate_actions(&state)
        .into_iter()
        .filter(|candidate| matches!(candidate.action, GameAction::ChooseTarget { .. }))
        .collect();
    assert!(
        !target_candidates.is_empty(),
        "reach guard: the engine issued target candidates"
    );
    for candidate in target_candidates {
        let mut probe = state.clone();
        assert!(
            apply_as_current(&mut probe, candidate.action.clone()).is_ok(),
            "reach guard: raw target candidate must be reducer-accepted: {:?}",
            candidate.action
        );
    }

    let scored = score_candidates(&state, player, &config);
    let own_scores: Vec<_> = scored
        .iter()
        .filter_map(|(action, score)| match action {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            } if own.contains(id) && score.is_finite() => Some(*score),
            _ => None,
        })
        .collect();
    assert!(
        !own_scores.is_empty(),
        "reach guard: scoring retains a finite own-land candidate"
    );
    assert!(
        scored
            .iter()
            .all(|(action, _)| !matches!(action, GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            } if opposing.contains(id))),
        "reach guard: scoring rejects every opposing-land candidate"
    );
    let mut choice_rng = StdRng::seed_from_u64(42);
    let chosen = choose_action(&state, player, &config, &mut choice_rng);
    assert!(matches!(
        chosen,
        Some(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id))
        }) if own.contains(&id)
    ));
}
