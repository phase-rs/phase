use std::cell::{Cell, RefCell};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use engine::ai_support::{auto_pass_recommended, legal_actions_for_viewer, legal_actions_full};
use engine::database::CardDatabase;
use engine::game::engine::apply;
use engine::game::{
    evaluate_deck_compatibility, filter_state_for_viewer, finalize_public_state,
    is_commander_eligible, load_and_hydrate_decks, rehydrate_game_from_card_db, resolve_deck_list,
    start_game, start_game_with_starting_player, validate_name_deck_for_format,
    DeckCompatibilityRequest, DeckList, PlayerDeckList,
};
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::{GameAction, GameState, PlayerId};

use engine::game::deck_loading::PlayerDeckPayload;
use engine::game::resolve_player_deck_list;
use engine::starter_decks;
use phase_ai::deck_profile::{ArchetypeClassification, DeckArchetype, DeckProfile};
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
    /// Engine-grouped subset of `actions` keyed by `GameAction::source_object()`.
    /// Frontend uses this for "what can I do with this card?" lookups so it
    /// doesn't have to introspect `GameAction` variants client-side.
    legal_actions_by_object: std::collections::HashMap<ObjectId, Vec<GameAction>>,
}

/// Serialize a Rust value to a JS object via JSON.
///
/// Uses `serde_json` as the intermediary format, then `JSON.parse` on the JS side.
/// This naturally converts all HashMap keys to strings (e.g., `ObjectId(42)` → `"42"`),
/// producing plain JS objects instead of `Map` instances — no frontend post-processing needed.
///
/// V8's `JSON.parse` is heavily optimized and often outperforms equivalent direct
/// object construction for large payloads.
fn to_js<T: Serialize + ?Sized>(value: &T) -> JsValue {
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

/// Stable sentinel prefix for "game state thread-local is None" errors.
/// JS adapter code matches on this prefix to classify the failure as
/// `AdapterErrorCode.STATE_LOST` and trigger transparent rehydrate-and-retry
/// recovery. Keep the prefix exact — it is part of the adapter contract.
const NOT_INITIALIZED_ERR: &str =
    "NOT_INITIALIZED: Game state not initialized. Call initialize_game or restore_game_state first.";

/// Take the game state out of the Cell, pass it to a closure that may mutate it,
/// then put it back. If the closure panics, the state is lost (None) but subsequent
/// calls won't fail with "RefCell already borrowed".
fn with_state_mut<R>(f: impl FnOnce(&mut GameState) -> R) -> Result<R, JsValue> {
    GAME_STATE.with(|cell| {
        let mut state = cell
            .take()
            .ok_or_else(|| JsValue::from_str(NOT_INITIALIZED_ERR))?;
        let result = f(&mut state);
        cell.set(Some(state));
        Ok(result)
    })
}

/// Borrow the game state immutably. Same take/set pattern to avoid RefCell poisoning.
fn with_state<R>(f: impl FnOnce(&GameState) -> R) -> Result<R, JsValue> {
    GAME_STATE.with(|cell| {
        let state = cell
            .take()
            .ok_or_else(|| JsValue::from_str(NOT_INITIALIZED_ERR))?;
        let result = f(&state);
        cell.set(Some(state));
        Ok(result)
    })
}

thread_local! {
    /// Last panic message + location, captured by our panic hook below.
    /// JS reads this via `take_last_panic_message` after a WASM trap so the
    /// "Engine connection lost" modal can show the real cause + offer a
    /// pre-filled bug report instead of asking the user to reload blind.
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Initialize panic hook for better error messages in WASM.
/// Called automatically on first use — safe to call multiple times.
///
/// We install our own hook (composing with `console_error_panic_hook`'s
/// console output) so panics are *both* logged to devtools and captured
/// for later retrieval. With `panic = 'abort'`, the hook runs before the
/// WASM trap, so a thread-local written here is readable from the next JS
/// call into the module.
#[wasm_bindgen(start)]
pub fn init_panic_hook() {
    use std::sync::Once;
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let payload = info.payload();
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Box<dyn Any> panic payload".to_string()
            };
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());
            let formatted = format!("panicked at {location}: {msg}");
            // Capture FIRST so the message lands even if the console mirror
            // re-panics (its formatter allocates; an OOM panic could trip it).
            // `try_borrow_mut` keeps a re-entrant write from blowing up — at
            // worst we lose the second panic's text, never the first.
            LAST_PANIC.with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    *slot = Some(formatted);
                }
            });
            // Mirror to the browser console with full backtrace + symbol names.
            console_error_panic_hook::hook(info);
        }));
    });
}

/// Drain the last captured panic message (consuming it). Returns `null` when
/// no panic has been observed since the last drain. JS calls this after a
/// thrown `RuntimeError` to decide whether to surface the modal as a real
/// engine crash (with the panic text + report link) or a transient
/// state-loss (the legacy reload prompt).
#[wasm_bindgen]
pub fn take_last_panic_message() -> Option<String> {
    LAST_PANIC.with(|cell| cell.borrow_mut().take())
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

/// Returns the official WotC rulings for a card as a JS array of `{date, text}`
/// objects. Returns an empty array if the card is not found, the database is
/// not loaded, or the card has no rulings (back faces of multi-face cards
/// inherit empty rulings — they're deduped at export time to the front face).
#[wasm_bindgen]
pub fn get_card_rulings(name: &str) -> JsValue {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return to_js(&Vec::<engine::database::mtgjson::Ruling>::new());
        };
        to_js(db.rulings_for(name))
    })
}

/// CR 903.3: Whether the named card can serve as a commander
/// (legendary creature, legendary background, or "can be your commander").
/// Returns false if the card database isn't loaded or the card isn't found.
#[wasm_bindgen]
pub fn is_card_commander_eligible(name: &str) -> bool {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return false;
        };
        db.get_face_by_name(name).is_some_and(is_commander_eligible)
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

/// Classify a deck's archetype (Aggro / Midrange / Control / Combo / Ramp) using
/// `phase_ai::DeckProfile::analyze`. The engine is the single authority for archetype
/// classification — the frontend must not compute this from card lists itself.
///
/// Input: a flat list of card names (duplicates allowed — `resolve_player_deck_list`
/// groups them into DeckEntry counts). Unresolvable names are silently skipped.
/// Output: `{ archetype, confidence: "Pure" | "Hybrid", secondary? }`.
#[wasm_bindgen]
pub fn classify_deck_js(names_js: JsValue) -> Result<JsValue, JsValue> {
    let names: Vec<String> = serde_wasm_bindgen::from_value(names_js)
        .map_err(|e| JsValue::from_str(&format!("Invalid card name list: {e}")))?;

    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        let list = PlayerDeckList {
            main_deck: names,
            sideboard: Vec::new(),
            commander: Vec::new(),
        };
        let payload = resolve_player_deck_list(db, &list);
        let profile = DeckProfile::analyze(&payload.main_deck);
        Ok(to_js(&DeckProfileResult::from(&profile)))
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeckProfileResult {
    archetype: &'static str,
    confidence: &'static str,
    /// Present only when `confidence == "Hybrid"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    secondary: Option<&'static str>,
}

impl DeckProfileResult {
    fn from(profile: &DeckProfile) -> Self {
        let archetype = archetype_name(profile.archetype);
        match &profile.classification {
            ArchetypeClassification::Pure(_) => Self {
                archetype,
                confidence: "Pure",
                secondary: None,
            },
            ArchetypeClassification::Hybrid { secondary, .. } => Self {
                archetype,
                confidence: "Hybrid",
                secondary: Some(archetype_name(*secondary)),
            },
        }
    }
}

fn archetype_name(a: DeckArchetype) -> &'static str {
    match a {
        DeckArchetype::Aggro => "Aggro",
        DeckArchetype::Midrange => "Midrange",
        DeckArchetype::Control => "Control",
        DeckArchetype::Combo => "Combo",
        DeckArchetype::Ramp => "Ramp",
    }
}

/// CR 100.4a: Returns the sideboard policy for a given game format as a
/// tagged union: `{"type": "Forbidden"}`, `{"type": "Limited", "data": 15}`,
/// or `{"type": "Unlimited"}`.
///
/// The frontend must exhaustive-switch on `.type` — unit variants (`Forbidden`,
/// `Unlimited`) emit no `data` field under `#[serde(tag, content)]`.
///
/// The engine is the single authority for format sideboard rules; the frontend
/// never hardcodes 15 or any other cap.
#[wasm_bindgen(js_name = sideboardPolicyForFormat)]
pub fn sideboard_policy_for_format(format: JsValue) -> Result<JsValue, JsValue> {
    let format: GameFormat = serde_wasm_bindgen::from_value(format)
        .map_err(|e| JsValue::from_str(&format!("Invalid GameFormat: {e}")))?;
    Ok(to_js(&format.sideboard_policy()))
}

/// Return the authoritative list of user-selectable formats as a typed array.
/// The frontend treats this as the single source of truth for rendering
/// format pickers, badges, and default configs — no hand-maintained mirrors.
#[wasm_bindgen(js_name = getFormatRegistry)]
pub fn get_format_registry() -> JsValue {
    to_js(&GameFormat::registry())
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
    state.debug_mode = true;
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

                for (seat, deck) in [
                    ("Player".to_string(), &deck_list.player),
                    ("AI opponent".to_string(), &deck_list.opponent),
                ] {
                    if let Err(reasons) = validate_name_deck_for_format(
                        db,
                        &deck.main_deck,
                        &deck.sideboard,
                        &deck.commander,
                        game_format,
                        Some(state.match_config.match_type),
                    ) {
                        return Some(
                            reasons
                                .into_iter()
                                .map(|reason| format!("{seat} deck: {reason}"))
                                .collect(),
                        );
                    }
                }
                for (idx, deck) in deck_list.ai_decks.iter().enumerate() {
                    let seat = format!("AI player {}", idx + 2);
                    if let Err(reasons) = validate_name_deck_for_format(
                        db,
                        &deck.main_deck,
                        &deck.sideboard,
                        &deck.commander,
                        game_format,
                        Some(state.match_config.match_type),
                    ) {
                        return Some(
                            reasons
                                .into_iter()
                                .map(|reason| format!("{seat} deck: {reason}"))
                                .collect(),
                        );
                    }
                }

                let mut payload = resolve_deck_list(db, &deck_list);

                // When player_count > 2 and no explicit AI decks provided,
                // replicate the opponent deck for all additional AI players.
                if count > 2 && payload.ai_decks.is_empty() {
                    for _ in 2..count {
                        payload.ai_decks.push(payload.opponent.clone());
                    }
                }

                load_and_hydrate_decks(&mut state, &payload, Some(db));
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

    if matches!(action, GameAction::Debug(_)) && is_multiplayer_mode() {
        return JsValue::from_str("Engine error: debug actions disabled in multiplayer");
    }

    if let GameAction::Debug(engine::types::actions::DebugAction::CreateCard {
        ref card_name,
        owner,
        zone,
    }) = action
    {
        return handle_debug_create_card(card_name, owner, zone);
    }

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

fn handle_debug_create_card(
    card_name: &str,
    owner: PlayerId,
    zone: engine::types::zones::Zone,
) -> JsValue {
    let face = CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err("Engine error: card database not loaded");
        };
        match db.get_face_by_name(card_name) {
            Some(face) => Ok(face.clone()),
            None => Err("Engine error: card not found in database"),
        }
    });
    let face = match face {
        Ok(f) => f,
        Err(msg) => return JsValue::from_str(msg),
    };
    match with_state_mut(|state| {
        if !state.debug_mode {
            return JsValue::from_str(
                "Engine error: Debug actions require debug_mode to be enabled",
            );
        }
        if !state.players.iter().any(|p| p.id == owner) {
            return JsValue::from_str("Engine error: Debug: invalid owner player id");
        }
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let obj_id =
            engine::game::zones::create_object(state, card_id, owner, face.name.clone(), zone);
        let obj = state.objects.get_mut(&obj_id).expect("just created");
        engine::game::printed_cards::apply_card_face_to_object(obj, &face);
        state.layers_dirty = true;
        engine::game::public_state::bump_state_revision(state);
        engine::game::public_state::mark_public_state_all_dirty(state);
        engine::game::public_state::finalize_public_state(state);
        let result = engine::types::game_state::ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        };
        to_js(&result)
    }) {
        Ok(val) => val,
        Err(e) => e,
    }
}

/// Get the current game state as a `ClientGameState` wire envelope
/// (`{ state, derived }`). The `derived` block holds engine-authored
/// presentation projections — commander-damage grouping, etc. — so the
/// frontend never computes game logic. Derivation happens just-in-time per
/// call and does not mutate `GameState`. See
/// `engine::game::derived_views::ClientGameStateRef`.
#[wasm_bindgen]
pub fn get_game_state() -> JsValue {
    match with_state(|state| {
        to_js(&engine::game::derived_views::ClientGameStateRef::wrap(
            state,
        ))
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Filtered-viewer variant of `get_game_state`. Runs the viewer filter
/// first (hides opponent hand/library per standard multiplayer redaction),
/// then derives views over the filtered state so the wire shape is
/// identical to `get_game_state` regardless of filter path.
#[wasm_bindgen]
pub fn get_filtered_game_state(viewer: u8) -> JsValue {
    match with_state(|state| {
        let filtered = filter_state_for_viewer(state, PlayerId(viewer));
        to_js(&engine::game::derived_views::ClientGameStateRef::wrap(
            &filtered,
        ))
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Get the legal actions, auto-pass recommendation, and spell costs for the current game state.
/// Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<ObjectId, ManaCost> }`.
#[wasm_bindgen]
pub fn get_legal_actions_js() -> JsValue {
    match with_state(|state| {
        let (actions, spell_costs, legal_actions_by_object) = legal_actions_full(state);
        let auto_pass = auto_pass_recommended(state, &actions);
        to_js(&LegalActionsResult {
            actions,
            auto_pass_recommended: auto_pass,
            spell_costs,
            legal_actions_by_object,
        })
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Viewer-scoped legal actions. Returns the same shape as `get_legal_actions_js`
/// but empty when the viewer is not the player currently expected to act. Used
/// by the P2P host to broadcast per-guest legal-action payloads without leaking
/// game logic into the transport adapter.
#[wasm_bindgen]
pub fn get_legal_actions_for_viewer_js(player_id: u32) -> JsValue {
    match with_state(|state| {
        let (actions, spell_costs, legal_actions_by_object) =
            legal_actions_for_viewer(state, PlayerId(player_id as u8));
        let auto_pass = auto_pass_recommended(state, &actions);
        to_js(&LegalActionsResult {
            actions,
            auto_pass_recommended: auto_pass,
            spell_costs,
            legal_actions_by_object,
        })
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Combined filtered-state + viewer-scoped legal-actions snapshot. Collapses
/// two WASM round-trips into one for the P2P host broadcast loop. Field names
/// match `LegalActionsResult` so the existing `legalActionsToWire` helper on
/// the TS side accepts it via structural typing.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewerSnapshot {
    state: GameState,
    actions: Vec<GameAction>,
    auto_pass_recommended: bool,
    spell_costs: std::collections::HashMap<ObjectId, ManaCost>,
    legal_actions_by_object: std::collections::HashMap<ObjectId, Vec<GameAction>>,
}

#[wasm_bindgen]
pub fn get_viewer_snapshot_js(player_id: u32) -> JsValue {
    match with_state(|state| {
        let viewer = PlayerId(player_id as u8);
        let filtered = filter_state_for_viewer(state, viewer);
        let (actions, spell_costs, legal_actions_by_object) =
            legal_actions_for_viewer(state, viewer);
        let auto_pass = auto_pass_recommended(state, &actions);
        to_js(&ViewerSnapshot {
            state: filtered,
            actions,
            auto_pass_recommended: auto_pass,
            spell_costs,
            legal_actions_by_object,
        })
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Current stack pressure bucket for animation pacing (Normal/Elevated/Rapid/Instant).
/// Not a rules concept — presentation policy owned by the engine for consistency
/// across browser/desktop/server consumers. Returned as a string to avoid
/// tsify enum-sharing overhead; frontend maps the string to a multiplier.
#[wasm_bindgen]
pub fn get_stack_pressure() -> JsValue {
    match with_state(|state| {
        let s = match engine::game::stack::stack_pressure(state) {
            engine::game::stack::StackPressure::Normal => "Normal",
            engine::game::stack::StackPressure::Elevated => "Elevated",
            engine::game::stack::StackPressure::Rapid => "Rapid",
            engine::game::stack::StackPressure::Instant => "Instant",
        };
        JsValue::from_str(s)
    }) {
        Ok(v) => v,
        Err(_) => JsValue::NULL,
    }
}

// `get_stack_display_groups` and `get_commander_damage_received` were both
// retired when their grouping moved into the authoritative
// `ClientGameState.derived` wire envelope produced by `get_game_state` /
// `get_filtered_game_state`. Leaving the standalone exports alongside would
// have created two paths to the same derived value — "duplicate logic
// across adapters" per CLAUDE.md — and the async RPC path also required a
// generation-counter race guard on the frontend to survive rapid stack
// mutations. Riding the same snapshot that carries `state.stack` makes the
// grouping atomically consistent with the stack it describes.
// See `engine::game::derived_views`.

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
    state.debug_mode = true;
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

/// Batch-resolve the stack by auto-passing priority for the requesting player
/// and delegating to the AI for opponent decisions. Runs entirely inside WASM
/// with no JS round-trips between resolutions — collapses the O(N) priority
/// pass cycle into a single call.
///
/// `requester` is the human player seat (whose "Resolve All" click initiated
/// this). `ai_seats_json` is a JSON array of `{ playerId, difficulty }` for
/// each AI opponent.
///
/// Returns a `BatchResolveResult` with all accumulated events, the final
/// `WaitingFor`, and a count of items resolved.
///
/// Stop conditions (all CR-compliant):
/// - Stack empties
/// - Stack grows beyond baseline (new triggers appeared — pause for human)
/// - An interactive `WaitingFor` appears (target selection, scry, etc.)
/// - An AI player chooses a non-pass action (response spell/ability)
/// - Game ends
/// - Safety cap reached (prevents infinite loops from cascading triggers)
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchResolveResult {
    events: Vec<engine::types::events::GameEvent>,
    waiting_for: engine::types::game_state::WaitingFor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    log_entries: Vec<engine::types::log::GameLogEntry>,
    items_resolved: u32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiSeatConfig {
    player_id: u8,
    difficulty: String,
}

#[wasm_bindgen]
pub fn resolve_all(
    requester: u8,
    ai_seats_json: &str,
    max_resolutions: u32,
) -> Result<JsValue, JsValue> {
    let ai_seats: Vec<AiSeatConfig> = serde_json::from_str(ai_seats_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize AI seats: {e}")))?;

    let requester = PlayerId(requester);
    let resolution_cap = if max_resolutions == 0 {
        u32::MAX
    } else {
        max_resolutions
    };

    with_state_mut(|state| {
        let initial_stack_len = state.stack.len();
        let mut all_events = Vec::new();
        let mut all_log_entries = Vec::new();
        let mut items_resolved: u32 = 0;
        // Safety cap: 2 passes per player per stack item + headroom for new triggers
        let max_iterations = initial_stack_len
            .saturating_mul(state.players.len())
            .saturating_mul(4)
            .clamp(100, 20_000);

        for _ in 0..max_iterations {
            let wf = &state.waiting_for;

            // Stop: game over
            if matches!(wf, engine::types::game_state::WaitingFor::GameOver { .. }) {
                break;
            }

            // Stop: non-priority interactive WaitingFor
            let Some(acting) = wf.acting_player() else {
                break;
            };
            if !matches!(wf, engine::types::game_state::WaitingFor::Priority { .. }) {
                break;
            }

            // Stop: stack emptied
            if state.stack.is_empty() {
                break;
            }

            // Stop: stack grew beyond initial size (new triggers appeared)
            if state.stack.len() > initial_stack_len {
                break;
            }

            // Determine the action for the current priority holder
            let action = if acting == requester {
                // Human requested resolve-all — auto-pass
                GameAction::PassPriority
            } else if let Some(seat) = ai_seats.iter().find(|s| PlayerId(s.player_id) == acting) {
                // AI player — ask the AI what to do
                let ai_difficulty = match seat.difficulty.as_str() {
                    "VeryEasy" => AiDifficulty::VeryEasy,
                    "Easy" => AiDifficulty::Easy,
                    "Medium" => AiDifficulty::Medium,
                    "Hard" => AiDifficulty::Hard,
                    "VeryHard" => AiDifficulty::VeryHard,
                    _ => AiDifficulty::Medium,
                };
                let config = create_config_for_players(
                    ai_difficulty,
                    Platform::Wasm,
                    state.players.len() as u8,
                );
                let mut rng = rand::rng();
                match choose_action(state, acting, &config, &mut rng) {
                    Some(action) => {
                        if !matches!(action, GameAction::PassPriority) {
                            // AI wants to respond — apply and stop
                            if let Ok(result) = apply(state, acting, action) {
                                all_events.extend(result.events);
                                all_log_entries.extend(result.log_entries);
                            }
                            break;
                        }
                        GameAction::PassPriority
                    }
                    None => GameAction::PassPriority,
                }
            } else {
                // Unknown seat — shouldn't happen, but pass to avoid deadlock
                GameAction::PassPriority
            };

            // Track resolutions: a resolution happens when all players pass
            // and the stack shrinks
            let stack_before = state.stack.len();

            match apply(state, acting, action) {
                Ok(result) => {
                    all_events.extend(result.events);
                    all_log_entries.extend(result.log_entries);

                    if state.stack.len() < stack_before {
                        items_resolved += (stack_before - state.stack.len()) as u32;
                        if items_resolved >= resolution_cap {
                            break;
                        }
                    }

                    // Stop: stack grew (new triggers from resolution)
                    if state.stack.len() > initial_stack_len {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        Ok(to_js(&BatchResolveResult {
            events: all_events,
            waiting_for: state.waiting_for.clone(),
            log_entries: all_log_entries,
            items_resolved,
        }))
    })?
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
