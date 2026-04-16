use std::cell::{Cell, RefCell};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use engine::ai_support::{auto_pass_recommended, legal_actions_with_costs};
use engine::database::CardDatabase;
use engine::game::engine::apply;
use engine::game::{
    evaluate_deck_compatibility, filter_state_for_viewer, finalize_public_state,
    load_deck_into_state, rehydrate_game_from_card_db, resolve_deck_list, start_game,
    start_game_with_starting_player, validate_deck_for_format, DeckCompatibilityRequest, DeckList,
};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::{GameAction, GameState, PlayerId};

use engine::game::deck_loading::PlayerDeckPayload;
use engine::game::{resolve_player_deck_list, PlayerDeckList};
use engine::starter_decks;
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx, SeatMutation, SeatState};

/// Result of `get_legal_actions_js` — bundles actions with the engine's auto-pass
/// recommendation so frontends don't need to classify action meaningfulness.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegalActionsResult {
    actions: Vec<GameAction>,
    auto_pass_recommended: bool,
    /// Effective mana costs for castable spells, keyed by object_id.
    /// Reflects all cost modifiers (reductions, commander tax, alt costs).
    spell_costs: std::collections::HashMap<ObjectId, ManaCost>,
}

/// Serialize a Rust value to a JS object via JSON.
///
/// Uses `serde_json` as the intermediary format, then `JSON.parse` on the JS side.
/// This naturally converts all HashMap keys to strings (e.g., `ObjectId(42)` → `"42"`),
/// producing plain JS objects instead of `Map` instances — no frontend post-processing needed.
///
/// V8's `JSON.parse` is heavily optimized and often outperforms equivalent direct
/// object construction for large payloads.
fn to_js<T: Serialize>(value: &T) -> JsValue {
    let json = serde_json::to_string(value)
        .unwrap_or_else(|e| panic!("serde_json serialization failed: {e}"));
    js_sys::JSON::parse(&json).unwrap_or_else(|e| panic!("JSON.parse failed: {e:?}"))
}

use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
thread_local! {
    /// Game state uses Cell<Option<T>> with take/set to avoid RefCell borrow poisoning.
    /// In WASM, panics don't unwind (no RAII cleanup), so a RefCell::borrow_mut() that
    /// panics leaves the borrow flag permanently set — every subsequent call fails.
    /// Cell::take() + Cell::set() has no borrow guard, making it panic-resilient.
    static GAME_STATE: Cell<Option<GameState>> = const { Cell::new(None) };
    static CARD_DB: RefCell<Option<CardDatabase>> = const { RefCell::new(None) };
    /// When set, the engine is running inside a multiplayer session (online
    /// WebSocket, P2P host, or P2P guest). Undo-style state rollback is
    /// refused in this mode because rewinding a single client's view would
    /// desync from the authoritative game on the wire. See `restore_game_state`.
    static MULTIPLAYER_MODE: Cell<bool> = const { Cell::new(false) };
}

/// Toggle the multiplayer enforcement flag. Called by multiplayer adapters
/// (P2P host/guest, WS) after the engine is initialized so subsequent
/// `restore_game_state` calls fail fast with a clear error instead of
/// silently rewriting the local view.
#[wasm_bindgen]
pub fn set_multiplayer_mode(enabled: bool) {
    MULTIPLAYER_MODE.with(|cell| cell.set(enabled));
}

/// Read the multiplayer enforcement flag. Exposed primarily for tests and
/// adapters that need to defend their own paths (e.g., skip history pushes).
#[wasm_bindgen]
pub fn is_multiplayer_mode() -> bool {
    MULTIPLAYER_MODE.with(|cell| cell.get())
}

/// Take the game state out of the Cell, pass it to a closure that may mutate it,
/// then put it back. If the closure panics, the state is lost (None) but subsequent
/// calls won't fail with "RefCell already borrowed".
fn with_state_mut<R>(f: impl FnOnce(&mut GameState) -> R) -> Result<R, JsValue> {
    GAME_STATE.with(|cell| {
        let mut state = cell.take().ok_or_else(|| {
            JsValue::from_str("Game not initialized. Call initialize_game first.")
        })?;
        let result = f(&mut state);
        cell.set(Some(state));
        Ok(result)
    })
}

/// Borrow the game state immutably. Same take/set pattern to avoid RefCell poisoning.
fn with_state<R>(f: impl FnOnce(&GameState) -> R) -> Result<R, JsValue> {
    GAME_STATE.with(|cell| {
        let state = cell.take().ok_or_else(|| {
            JsValue::from_str("Game not initialized. Call initialize_game first.")
        })?;
        let result = f(&state);
        cell.set(Some(state));
        Ok(result)
    })
}

/// Initialize panic hook for better error messages in WASM.
/// Called automatically on first use — safe to call multiple times.
#[wasm_bindgen(start)]
pub fn init_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Clear the game state without dropping the WASM instance or card database.
///
/// Used by the singleton adapter to reset between game sessions. Any in-flight
/// AI computation that calls `with_state()` after this will return an error
/// immediately rather than running a full search on stale state.
#[wasm_bindgen]
pub fn clear_game_state() {
    GAME_STATE.with(|cell| cell.set(None));
}

/// Verify WASM integration works.
#[wasm_bindgen]
pub fn ping() -> String {
    "phase-rs engine ready".to_string()
}

/// Create a default 2-player game state.
#[wasm_bindgen]
pub fn create_initial_state() -> JsValue {
    let state = GameState::default();
    to_js(&state)
}

/// Load the card database from a JSON string (card-data.json contents).
/// Must be called before initialize_game to enable name-based deck resolution.
#[wasm_bindgen]
pub fn load_card_database(json_str: &str) -> Result<u32, JsValue> {
    let db = CardDatabase::from_json_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse card database: {}", e)))?;
    let count = db.card_count() as u32;
    CARD_DB.with(|cell| {
        *cell.borrow_mut() = Some(db);
    });
    Ok(count)
}

/// Look up a card face by name from the loaded card database.
/// Returns the serialized `CardFace` (keywords, abilities, triggers, static_abilities,
/// replacements, card_type, oracle_text, etc.) or null if not found.
/// Used by the deck builder to display engine-parsed ability data.
#[wasm_bindgen]
pub fn get_card_face_data(name: &str) -> JsValue {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return JsValue::NULL;
        };
        match db.get_face_by_name(name) {
            Some(face) => to_js(face),
            None => JsValue::NULL,
        }
    })
}

/// Returns the hierarchical parse tree for a card face, with per-item support status.
/// Each `ParsedItem` contains category, label, source_text, supported (bool), details
/// (key-value pairs), and recursive children (sub-abilities, modal modes, costs).
/// Returns null if the card database is not loaded or the card is not found.
#[wasm_bindgen]
pub fn get_card_parse_details(name: &str) -> JsValue {
    use engine::game::coverage::build_parse_details_for_face;

    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return JsValue::NULL;
        };
        match db.get_face_by_name(name) {
            Some(face) => to_js(&build_parse_details_for_face(face)),
            None => JsValue::NULL,
        }
    })
}

/// Evaluate deck compatibility and format legality using the loaded card database.
/// Returns strict Standard/Commander checks, BO3 readiness, and selected-format compatibility.
#[wasm_bindgen]
pub fn evaluate_deck_compatibility_js(request: JsValue) -> Result<JsValue, JsValue> {
    let request: DeckCompatibilityRequest = serde_wasm_bindgen::from_value(request)
        .map_err(|e| JsValue::from_str(&format!("Invalid compatibility request: {e}")))?;

    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        let result = evaluate_deck_compatibility(db, &request);
        Ok(to_js(&result))
    })
}

/// Initialize a new game.
/// Accepts deck_data as a DeckList (name-only) or null/undefined for empty libraries.
/// format_config_js: optional FormatConfig JSON — defaults to Standard if null/undefined.
/// match_config_js: optional MatchConfig JSON — defaults to BO1 if null/undefined.
/// player_count: number of players — defaults to 2 if not provided.
/// first_player: 0 = human plays first (CR 103.1), 1 = opponent plays first, None = random.
/// Names are resolved against the card database loaded via load_card_database().
/// Returns the initial ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn initialize_game(
    deck_data: JsValue,
    seed: Option<f64>,
    format_config_js: JsValue,
    match_config_js: JsValue,
    player_count: Option<u8>,
    first_player: Option<u8>,
) -> JsValue {
    let seed = seed.map(|s| s as u64).unwrap_or(42);

    let format_config = if !format_config_js.is_null() && !format_config_js.is_undefined() {
        serde_wasm_bindgen::from_value::<FormatConfig>(format_config_js)
            .unwrap_or_else(|_| FormatConfig::standard())
    } else {
        FormatConfig::standard()
    };
    let count = player_count.unwrap_or(2);
    let game_format = format_config.format;

    let mut state = GameState::new(format_config, count, seed);
    state.match_config = if !match_config_js.is_null() && !match_config_js.is_undefined() {
        serde_wasm_bindgen::from_value::<MatchConfig>(match_config_js)
            .unwrap_or_else(|_| MatchConfig::default())
    } else {
        MatchConfig::default()
    };

    // Load deck data if provided — resolve names via the loaded card database
    if !deck_data.is_null() && !deck_data.is_undefined() {
        if let Ok(deck_list) = serde_wasm_bindgen::from_value::<DeckList>(deck_data) {
            let validation_error: Option<Vec<String>> = CARD_DB.with(|cell| {
                let borrow = cell.borrow();
                let db = borrow.as_ref()?;

                // Validate player deck against the selected format before loading
                let validation_request = DeckCompatibilityRequest {
                    main_deck: deck_list.player.main_deck.clone(),
                    sideboard: deck_list.player.sideboard.clone(),
                    commander: deck_list.player.commander.clone(),
                    selected_format: Some(game_format),
                    selected_match_type: None,
                };
                if let Err(reasons) = validate_deck_for_format(db, &validation_request) {
                    return Some(reasons);
                }

                let mut payload = resolve_deck_list(db, &deck_list);

                // When player_count > 2 and no explicit AI decks provided,
                // replicate the opponent deck for all additional AI players.
                if count > 2 && payload.ai_decks.is_empty() {
                    for _ in 2..count {
                        payload.ai_decks.push(payload.opponent.clone());
                    }
                }

                load_deck_into_state(&mut state, &payload);
                rehydrate_game_from_card_db(&mut state, db);
                state.all_card_names = db.card_names().into();
                None
            });

            if let Some(reasons) = validation_error {
                return to_js(&serde_json::json!({
                    "error": true,
                    "reasons": reasons,
                }));
            }
        }
    }

    // CR 103.1: Start the game with the chosen starting player.
    let result = match first_player {
        Some(0) => start_game_with_starting_player(&mut state, PlayerId(0)),
        Some(1) => start_game_with_starting_player(&mut state, PlayerId(1)),
        _ => start_game(&mut state),
    };

    GAME_STATE.with(|cell| cell.set(Some(state)));

    to_js(&result)
}

/// Submit a game action on behalf of `actor` and return the ActionResult
/// (events + waiting_for).
///
/// **Security contract:** `actor` must be the transport-authenticated
/// `PlayerId` of the caller — either the local human's seat (in local/AI
/// games) or the connection-authenticated seat (in P2P/WebSocket games).
/// It must *never* come from UI or wire payload data. The engine rejects any
/// action whose `actor` does not match `authorized_submitter(state)`, so
/// passing a spoofed value here will fail cleanly rather than silently
/// applying the action as another player.
#[wasm_bindgen]
pub fn submit_action(actor: u8, action: JsValue) -> JsValue {
    // Deserialize outside `with_state_mut` and use a recoverable error, not
    // `.expect()`. In WASM, panics do not unwind — a panic *inside*
    // `with_state_mut` would leave `GAME_STATE` taken-but-not-returned,
    // permanently bricking the game with "Game not initialized" for every
    // subsequent call. Callers passing malformed `action` (including stale JS
    // bindings post-signature-change) now get a clean error instead.
    let action: GameAction = match serde_wasm_bindgen::from_value(action) {
        Ok(a) => a,
        Err(e) => {
            return JsValue::from_str(&format!("Engine error: failed to deserialize action: {e}"));
        }
    };
    let actor = PlayerId(actor);

    match with_state_mut(|state| match apply(state, actor, action) {
        Ok(result) => to_js(&result),
        Err(e) => {
            let error_msg = format!("Engine error: {}", e);
            JsValue::from_str(&error_msg)
        }
    }) {
        Ok(val) => val,
        Err(e) => e,
    }
}

/// Get the current game state as JSON.
/// Derived display fields (summoning sickness, devotion, etc.) are computed
/// automatically by the engine in apply()/start_game().
#[wasm_bindgen]
pub fn get_game_state() -> JsValue {
    match with_state(to_js) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Get a filtered view of the current game state for the given player.
#[wasm_bindgen]
pub fn get_filtered_game_state(viewer: u8) -> JsValue {
    match with_state(|state| to_js(&filter_state_for_viewer(state, PlayerId(viewer)))) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Get the legal actions, auto-pass recommendation, and spell costs for the current game state.
/// Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<ObjectId, ManaCost> }`.
#[wasm_bindgen]
pub fn get_legal_actions_js() -> JsValue {
    match with_state(|state| {
        let (actions, spell_costs) = legal_actions_with_costs(state);
        let auto_pass = auto_pass_recommended(state, &actions);
        to_js(&LegalActionsResult {
            actions,
            auto_pass_recommended: auto_pass,
            spell_costs,
        })
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Export the current game state as a JSON string.
/// Used by the engine worker to transfer state to AI workers for root parallelism.
#[wasm_bindgen]
pub fn export_game_state_json() -> Result<String, JsValue> {
    with_state(|state| {
        serde_json::to_string(state)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize GameState: {e}")))
    })?
}

/// Restore the game state from a JSON string.
/// Uses serde_json which handles string-keyed maps (from localStorage round-trip)
/// correctly deserializing into HashMap<ObjectId, V>.
///
/// Refuses when `MULTIPLAYER_MODE` is set — rewriting a single client's
/// state in a multiplayer session would diverge from the authoritative
/// game on the wire. Undo is a single-player affordance only.
#[wasm_bindgen]
pub fn restore_game_state(json_str: &str) -> Result<(), JsValue> {
    if MULTIPLAYER_MODE.with(|cell| cell.get()) {
        return Err(JsValue::from_str(
            "restore_game_state refused: undo is disabled in multiplayer sessions",
        ));
    }
    let mut state: GameState = serde_json::from_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {}", e)))?;
    state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed);
    CARD_DB.with(|cell| {
        if let Some(db) = cell.borrow().as_ref() {
            rehydrate_game_from_card_db(&mut state, db);
        }
    });
    finalize_public_state(&mut state);
    GAME_STATE.with(|cell| cell.set(Some(state)));
    Ok(())
}

/// Resume a multiplayer host session from a persisted `GameState`.
///
/// Called when a P2P host returns after a crash/reload and needs to restore
/// the authoritative game state from disk so returning guests (still in
/// their reconnect backoff) can re-bind to their seats. Mirrors
/// `server-core::GameSession::from_persisted` — the analogous pattern for
/// the WebSocket-server authority.
///
/// Differs from `restore_game_state` in two load-bearing ways:
///
/// 1. **Fresh RNG seed.** `restore_game_state` re-seeds from the saved
///    `rng_seed`, which rewinds the ChaCha20 stream to position 0 —
///    correct for undo (replay from origin) but wrong for resume
///    (subsequent draws would replay the pre-save sequence). This
///    function stamps a fresh seed so continued play diverges.
/// 2. **Atomic multiplayer-flag flip.** Sets `MULTIPLAYER_MODE` in the
///    same call that loads state, so there's no window where a stray
///    `restore_game_state` (undo) would be accepted on the resumed
///    session.
///
/// Refuses when the engine is already in use — this is a fresh-instance
/// entry point. Callers must clear any existing state first.
#[wasm_bindgen]
pub fn resume_multiplayer_host_state(json_str: &str) -> Result<(), JsValue> {
    if MULTIPLAYER_MODE.with(|cell| cell.get()) {
        return Err(JsValue::from_str(
            "resume_multiplayer_host_state refused: multiplayer mode already set",
        ));
    }
    let already_has_state = GAME_STATE.with(|cell| {
        let s = cell.take();
        let present = s.is_some();
        cell.set(s);
        present
    });
    if already_has_state {
        return Err(JsValue::from_str(
            "resume_multiplayer_host_state refused: engine already initialized; call clear_game_state first",
        ));
    }

    let mut state: GameState = serde_json::from_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {}", e)))?;

    // Stale `rng_seed` replays the pre-save ChaCha20 sequence because
    // stream position is `#[serde(skip)]`. Mirrors server-core.
    let fresh_seed: u64 = rand::rng().random();
    state.rng_seed = fresh_seed;
    state.rng = ChaCha20Rng::seed_from_u64(fresh_seed);

    CARD_DB.with(|cell| {
        if let Some(db) = cell.borrow().as_ref() {
            rehydrate_game_from_card_db(&mut state, db);
        }
    });
    finalize_public_state(&mut state);

    GAME_STATE.with(|cell| cell.set(Some(state)));
    MULTIPLAYER_MODE.with(|cell| cell.set(true));
    Ok(())
}

/// Get the AI's chosen action for the current game state.
/// `difficulty` is one of: "VeryEasy", "Easy", "Medium", "Hard", "VeryHard".
/// `player_id` is the seat index of the AI player (0-based).
#[wasm_bindgen]
pub fn get_ai_action(difficulty: &str, player_id: u8) -> Result<JsValue, JsValue> {
    let ai_difficulty = match difficulty {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    };

    with_state(|state| {
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);

        let ai_player = PlayerId(player_id);
        let mut rng = rand::rng();

        match choose_action(state, ai_player, &config, &mut rng) {
            Some(action) => Ok(to_js(&action)),
            None => Ok(JsValue::NULL),
        }
    })?
}

/// Score all candidate actions and return `[GameAction, score]` tuples.
/// Used by AI workers for root parallelism — each worker scores independently,
/// then results are merged on the main thread.
/// `rng_seed` seeds the game state's RNG so each worker's MCTS explores
/// different paths through the search tree, producing diverse score vectors.
#[wasm_bindgen]
pub fn get_ai_scored_candidates(
    difficulty: &str,
    player_id: u8,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = match difficulty {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    };

    with_state_mut(|state| {
        // Re-seed the state RNG so each parallel worker explores different
        // MCTS rollout paths and beam-search tie-breaking orders.
        state.rng = ChaCha20Rng::seed_from_u64(rng_seed);
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        let ai_player = PlayerId(player_id);
        let scored = phase_ai::score_candidates(state, ai_player, &config);
        Ok(to_js(&scored))
    })?
}

/// Select an action from merged scores using softmax.
/// Called after collecting scored candidates from parallel workers and merging.
/// `scores_json` is a JSON array of `[GameAction, score]` tuples.
/// `difficulty` determines the softmax temperature (engine is the single
/// authority for AI tuning parameters — the frontend never specifies temperature).
/// `rng_seed` provides deterministic randomness.
#[wasm_bindgen]
pub fn select_action_from_scores(
    scores_json: &str,
    difficulty: &str,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = match difficulty {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    };
    let config = phase_ai::config::create_config(ai_difficulty, Platform::Wasm);
    let scored: Vec<(GameAction, f64)> = serde_json::from_str(scores_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize scores: {e}")))?;
    let mut rng = ChaCha20Rng::seed_from_u64(rng_seed);
    match phase_ai::softmax_select_pairs(&scored, config.temperature, &mut rng) {
        Some(action) => Ok(to_js(&action)),
        None => Ok(JsValue::NULL),
    }
}

/// Apply a seat mutation to a seat state, using the TLS card database for deck
/// resolution. Both arguments are JSON strings; returns `{ state, delta }` as
/// a JS object on success, or a JS error string on failure.
#[wasm_bindgen]
pub fn apply_seat_mutation(state_json: &str, mutation_json: &str) -> Result<JsValue, JsValue> {
    struct WasmDeckResolver;
    impl DeckResolver for WasmDeckResolver {
        fn resolve(&self, choice: &DeckChoice) -> Result<PlayerDeckPayload, String> {
            let deck_data = match choice {
                DeckChoice::Random => starter_decks::random_starter_deck(),
                DeckChoice::Named(name) => starter_decks::find_starter_deck(name)
                    .ok_or_else(|| format!("Starter deck not found: {name}"))?,
            };
            CARD_DB.with(|cell| {
                let db = cell.borrow();
                let db = db.as_ref().ok_or("Card database not loaded")?;
                Ok(resolve_player_deck_list(
                    db,
                    &PlayerDeckList {
                        main_deck: deck_data.main_deck,
                        sideboard: deck_data.sideboard,
                        commander: deck_data.commander,
                    },
                ))
            })
        }
    }

    let mut state: SeatState = serde_json::from_str(state_json)
        .map_err(|e| JsValue::from_str(&format!("Invalid SeatState: {e}")))?;
    let mutation: SeatMutation = serde_json::from_str(mutation_json)
        .map_err(|e| JsValue::from_str(&format!("Invalid SeatMutation: {e}")))?;

    let ctx = ReducerCtx {
        platform: Platform::Wasm,
        deck_resolver: &WasmDeckResolver,
    };

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SeatMutationResult {
        state: SeatState,
        delta: seat_reducer::types::SeatDelta,
    }

    match seat_reducer::apply(&mut state, mutation, &ctx) {
        Ok(delta) => Ok(to_js(&SeatMutationResult { state, delta })),
        Err(e) => Err(JsValue::from_str(&format!("{e:?}"))),
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use engine::game::deck_loading::create_object_from_card_face;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect, QuantityExpr,
        TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::player::PlayerId;

    use engine::types::zones::Zone;

    fn make_face(name: &str, oracle_id: &str, keyword: Keyword) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            },
            power: Some(engine::types::ability::PtValue::Fixed(2)),
            toughness: Some(engine::types::ability::PtValue::Fixed(2)),
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![keyword],
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                },
            )],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: Some(vec![ManaColor::Green]),
            scryfall_oracle_id: Some(oracle_id.to_string()),
            modal: None,
            additional_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            strive_cost: None,
            brawl_commander: false,
            metadata: Default::default(),
        }
    }

    fn load_db_with_updated_face() {
        let json = serde_json::json!({
            "test card": {
                "name": "Test Card",
                "mana_cost": { "Cost": { "shards": ["Green"], "generic": 1 } },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Bear"] },
                "power": { "type": "Fixed", "value": 2 },
                "toughness": { "type": "Fixed", "value": 2 },
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": ["Trample"],
                "abilities": [{
                    "kind": "Spell",
                    "effect": {
                        "type": "DealDamage",
                        "amount": { "type": "Fixed", "value": 4 },
                        "target": { "type": "Any" }
                    },
                    "cost": null,
                    "sub_ability": null,
                    "duration": null,
                    "description": null,
                    "target_prompt": null,
                    "sorcery_speed": false
                }],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": ["Green"],
                "scryfall_oracle_id": "oracle-1"
            }
        })
        .to_string();
        load_card_database(&json).unwrap();
    }

    #[test]
    fn restore_rehydrates_saved_state_when_db_loaded() {
        load_db_with_updated_face();

        let mut state = GameState::new_two_player(42);
        let card = make_face("Test Card", "oracle-1", Keyword::Vigilance);
        let object_id = create_object_from_card_face(&mut state, &card, PlayerId(0));
        engine::game::zones::move_to_zone(
            &mut state,
            object_id,
            Zone::Battlefield,
            &mut Vec::new(),
        );
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.counters
            .insert(engine::types::CounterType::Plus1Plus1, 1);
        state.add_transient_continuous_effect(
            object_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: object_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }],
            None,
        );
        evaluate_layers(&mut state);
        derive_display_state(&mut state);

        let json = serde_json::to_string(&state).unwrap();
        restore_game_state(&json).unwrap();
        let restored: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();
        let obj = restored.objects.get(&object_id).unwrap();

        assert_eq!(obj.printed_ref.as_ref().unwrap().oracle_id, "oracle-1");
        assert!(obj.base_keywords.contains(&Keyword::Trample));
        assert!(obj.keywords.contains(&Keyword::Flying));
        assert_eq!(
            obj.counters
                .get(&engine::types::CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn multiplayer_mode_refuses_restore_game_state() {
        // Single-player baseline: restore succeeds.
        let state = GameState::new_two_player(7);
        let json = serde_json::to_string(&state).unwrap();
        set_multiplayer_mode(false);
        assert!(restore_game_state(&json).is_ok());

        // Toggle multiplayer on; restore must now refuse with a descriptive
        // error and not mutate the stored game state.
        set_multiplayer_mode(true);
        let err = restore_game_state(&json).expect_err("should refuse in multiplayer");
        let msg = err.as_string().unwrap_or_default();
        assert!(
            msg.contains("multiplayer"),
            "error should mention multiplayer; got: {msg}"
        );

        // Flag is observable via the getter and clears cleanly.
        assert!(is_multiplayer_mode());
        set_multiplayer_mode(false);
        assert!(!is_multiplayer_mode());
        assert!(restore_game_state(&json).is_ok());
    }

    #[test]
    fn resume_multiplayer_host_state_refuses_if_already_initialized() {
        // Must start from a clean slate — other tests may have populated the
        // thread-local state.
        clear_game_state();
        set_multiplayer_mode(false);

        // Seed a game so `resume_` sees it as "already initialized".
        let state = GameState::new_two_player(7);
        let json = serde_json::to_string(&state).unwrap();
        restore_game_state(&json).unwrap();

        let err = resume_multiplayer_host_state(&json)
            .expect_err("should refuse when engine already has state");
        let msg = err.as_string().unwrap_or_default();
        assert!(
            msg.contains("already initialized"),
            "error should mention engine-in-use; got: {msg}"
        );

        // Cleanup so following tests start clean.
        clear_game_state();
        set_multiplayer_mode(false);
    }

    #[test]
    fn resume_multiplayer_host_state_refuses_if_multiplayer_already_on() {
        clear_game_state();
        set_multiplayer_mode(true);

        let state = GameState::new_two_player(7);
        let json = serde_json::to_string(&state).unwrap();

        let err = resume_multiplayer_host_state(&json)
            .expect_err("should refuse when multiplayer mode is already set");
        let msg = err.as_string().unwrap_or_default();
        assert!(
            msg.contains("multiplayer mode already set"),
            "error should mention multiplayer flag state; got: {msg}"
        );

        set_multiplayer_mode(false);
    }

    #[test]
    fn resume_multiplayer_host_state_stamps_fresh_rng_seed_and_enables_flag() {
        clear_game_state();
        set_multiplayer_mode(false);

        let mut state = GameState::new_two_player(42);
        // Force a known "stale" seed so we can prove it was replaced.
        state.rng_seed = 0xDEAD_BEEF_0000_0001;
        let json = serde_json::to_string(&state).unwrap();

        resume_multiplayer_host_state(&json).unwrap();

        // Flag flipped atomically with state load.
        assert!(is_multiplayer_mode());

        // RNG seed was replaced with a fresh random value — stale seed would
        // replay the pre-save ChaCha20 stream from position 0 and cause
        // deterministic redraws.
        let restored: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();
        assert_ne!(
            restored.rng_seed, 0xDEAD_BEEF_0000_0001,
            "rng_seed should be freshly stamped, not preserved from the save"
        );

        // Cleanup.
        clear_game_state();
        set_multiplayer_mode(false);
    }

    #[test]
    fn restore_keeps_legacy_state_without_printed_ref() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(1);
        state.objects.insert(
            object_id,
            engine::game::GameObject::new(
                object_id,
                engine::types::identifiers::CardId(1),
                PlayerId(0),
                "Legacy Card".to_string(),
                Zone::Hand,
            ),
        );
        state.players[0].hand.push(object_id);

        let json = serde_json::to_string(&state).unwrap();
        restore_game_state(&json).unwrap();
        let restored: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();

        assert!(restored.objects[&object_id].printed_ref.is_none());
        assert_eq!(restored.objects[&object_id].name, "Legacy Card");
    }
}
