//! Regression guard for duel-suite follow-up 1.
//!
//! The greasefang-mirror matchup previously exhausted the per-game
//! `MAX_TOTAL_ACTIONS` (10,000) safety cap because the deck's
//! "Discard a card: <redundant-effect>" activated abilities (Fleeting
//! Spirit, Iron-Shield Elf, Guardian of New Benalia) combined with
//! Monument to Endurance's discard-triggered draw to produce an
//! unbounded card-neutral activation loop that the AI softmax scored
//! net-positive.
//!
//! The fix (3f1939bd5 + abfd60e76) adds two layers of mitigation:
//!
//! 1. `GameState::pending_activations` — per-priority-window AI-guard
//!    that prevents the AI from re-picking the same `(source_id,
//!    ability_index)` while its prior activation is unresolved on the
//!    stack.
//!
//! 2. `MAX_ACTIVATIONS_PER_SOURCE_PER_TURN` in `phase-ai/src/search.rs`
//!    — per-turn cap on repeated same-source activation, checked
//!    against the engine's existing `activated_abilities_this_turn`
//!    HashMap (which tracks CR 602.5b activation limits).
//!
//! Together these bound the pathology: the game completes naturally.
//! This test locks in the bound so regressions surface immediately.
//!
//! `#[ignore]` because it loads card-data.json (requires `cargo run
//! --bin card-data-export` or the setup.sh script), which is not
//! available in unit-test CI. Opt in via
//! `cargo test -p phase-ai --test greasefang_bounded -- --ignored`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use engine::database::CardDatabase;
use engine::game::deck_loading::load_deck_into_state;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::duel_suite::find_matchup;
use phase_ai::duel_suite::run::resolve_matchup;

/// Bound on total AI actions across the game. Pre-fix runs hit the
/// 10,000-action safety cap in `ai_duel.rs`. Post-fix games complete
/// naturally in ~1500–2500 actions for this seed; 4000 gives comfortable
/// headroom for softmax variance without tolerating a regression back
/// toward the 10,000 cap.
const BOUND_ACTIONS: usize = 4000;

fn load_db() -> CardDatabase {
    let cards_dir = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("client")
                .join("public")
        });
    let export_path = cards_dir.join("card-data.json");
    CardDatabase::from_export(&export_path)
        .unwrap_or_else(|e| panic!("load card-data.json from {}: {e}", export_path.display()))
}

#[test]
#[ignore = "loads card-data.json + runs a full game; opt in via --ignored"]
fn greasefang_mirror_terminates_within_bound() {
    let db = load_db();
    let spec = find_matchup("greasefang-mirror").expect("greasefang-mirror registered");
    let (payload, _p0, _p1) = resolve_matchup(&db, spec).expect("resolve matchup");

    let mut state = GameState::new_two_player(1);
    load_deck_into_state(&mut state, &payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    let config = create_config_for_players(AiDifficulty::Easy, Platform::Native, 2);
    let ai_configs: HashMap<PlayerId, _> =
        [(PlayerId(0), config.clone()), (PlayerId(1), config.clone())]
            .into_iter()
            .collect();

    let mut total_actions: usize = 0;
    loop {
        let results = run_ai_actions(&mut state, &ai_players, &ai_configs);
        if results.is_empty() {
            break;
        }
        total_actions += results.len();
        if total_actions >= BOUND_ACTIONS {
            panic!(
                "greasefang-mirror exceeded action bound: {total_actions} >= {BOUND_ACTIONS} \
                 at turn {}. Likely a regression in \
                 `pending_activations` or `MAX_ACTIVATIONS_PER_SOURCE_PER_TURN`.",
                state.turn_number,
            );
        }
    }

    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "game did not reach GameOver (actions = {total_actions}, turn = {}, waiting_for = {:?})",
        state.turn_number,
        std::mem::discriminant(&state.waiting_for),
    );
}
