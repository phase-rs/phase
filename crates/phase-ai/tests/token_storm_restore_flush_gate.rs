//! Token-storm restore-flush CI gate (phase-ai tier).
//!
//! Companion to `crates/engine/tests/token_storm_scaling_gate.rs`, guarding the
//! phase-ai side of the `StaticModePresence` O(1) index: the production
//! serde-restore → `flush_layers` seam that `load_saved_game_state`
//! (`saved_state.rs`) and the AI search hit. A deserialized `GameState` arrives
//! with `static_mode_presence = all_present` (the conservative `#[serde(skip,
//! default)]` value) and `layers_dirty = Full`; `load_saved_game_state` flushes
//! on load so read-only AI consumers (`choose_action` takes `&GameState` and
//! cannot flush) pay O(1) presence reads instead of O(battlefield) scans.
//!
//! DB-free by construction: `GameState::new_two_player` + `create_object` only,
//! never loading `client/public/card-data.json`
//! (`scripts/check-test-card-data-load.sh` guards this). Under `cargo nextest`
//! each test runs in its own process, so the `thread_local!` perf counters
//! cannot bleed across tests and the exact `== 0` assertion is sound.

use engine::game::perf_counters;
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{StaticDefinition, TargetFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::{StaticMode, StaticModeKind};
use engine::types::zones::Zone;
use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const TOKENS: usize = 1000;

/// `TargetFilter` matching every creature (built from public API — the private
/// `#[cfg(test)]` helper in engine is not reachable here).
fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

/// 1000 vanilla creature tokens controlled by `owner`, plus one bare global
/// Vigilance static (no `.affected()`) so the presence index is provably
/// non-empty yet leaves `IgnoreHexproof` precisely absent after a flush. Mirrors
/// the engine-tier `token_storm_board` DB-free idiom exactly.
fn token_storm_board(owner: PlayerId) -> GameState {
    let mut state = GameState::new_two_player(42);
    for i in 0..TOKENS {
        let id = create_object(
            &mut state,
            CardId(1000 + i as u64),
            owner,
            format!("Token{i}"),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }
    let src = create_object(
        &mut state,
        CardId(9999),
        owner,
        "Vigilance Anthem".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&src).unwrap().static_definitions =
        vec![StaticDefinition::new(StaticMode::Vigilance)].into();
    state
}

/// Wrap a state in the saved-game envelope `{"gameState": <state-json>}` that
/// `load_saved_game_state` (`saved_state.rs`) expects.
fn saved_envelope(state: &GameState) -> String {
    serde_json::json!({ "gameState": state }).to_string()
}

/// Test 2a — `load_saved_game_state` flushes the presence index (always-on backbone).
///
/// Revert-failing assertion: if the `flush_layers` call in `saved_state.rs` (the
/// load-time flush) is deleted, the loaded state keeps the all-present serde
/// default → `contains(IgnoreHexproof)` stays true AND target enumeration runs a
/// whole-battlefield scan per token. Both assertions below flip.
#[test]
fn load_saved_game_state_flushes_presence_index() {
    let state = token_storm_board(PlayerId(1));
    let envelope = saved_envelope(&state);

    let loaded = load_saved_game_state(&envelope).expect("load saved game state");

    // The load-time flush made the index PRECISE: Vigilance present (discrimination
    // guard — proves it is not "all-empty"), IgnoreHexproof absent (the revert guard).
    assert!(
        loaded
            .static_mode_presence
            .contains(StaticModeKind::Vigilance),
        "the seeded Vigilance static must be indexed present after the load-time flush"
    );
    assert!(
        !loaded
            .static_mode_presence
            .contains(StaticModeKind::IgnoreHexproof),
        "load_saved_game_state must flush the index so IgnoreHexproof reads absent \
         (trips if the saved_state.rs load-time flush is deleted)"
    );

    perf_counters::reset();
    let targets = find_legal_targets(&loaded, &creature_filter(), PlayerId(0), ObjectId(99));
    let counters = perf_counters::snapshot();

    assert_eq!(
        counters.static_full_scans, 0,
        "target enumeration on a loaded saved game must not run any whole-battlefield \
         static scan (trips if the load-time flush is deleted)"
    );
    // Reach-guard: the 0-scan is not from an empty result.
    assert_eq!(targets.len(), TOKENS, "every token must be a legal target");
}

/// Test 2b — full AI search stays bounded on a restored board (`#[ignore]`
/// defense-in-depth backstop; 2a is the always-on gate).
///
/// `perf_counters` is `thread_local`; nextest's process-per-test isolates it, but
/// `choose_action` drives real search work, so the bound is deliberately loose
/// and the test is `#[ignore]`. Covers the `search.rs` `choose_action(&GameState,
/// …)` consumer, which relies on `load_saved_game_state` having flushed the index.
#[test]
#[ignore = "opt-in perf backstop; 2a is the always-on gate"]
fn choose_action_on_restored_board_is_bounded() {
    let state = token_storm_board(PlayerId(1));
    let envelope = saved_envelope(&state);
    let loaded = load_saved_game_state(&envelope).expect("load saved game state");

    let cfg = create_config_for_players(AiDifficulty::VeryEasy, Platform::Native, 2);
    let mut rng = SmallRng::seed_from_u64(42);

    perf_counters::reset();
    let _ = choose_action(&loaded, PlayerId(0), &cfg, &mut rng);
    let counters = perf_counters::snapshot();

    assert!(
        counters.static_full_scans < 200_000,
        "full AI search on a restored token-storm board must stay bounded; got {}",
        counters.static_full_scans
    );
}
