use std::cell::{Cell, RefCell};
use std::sync::Arc;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use engine::ai_support::{
    auto_pass_recommended, auto_pass_recommended_for_viewer, legal_actions_for_viewer,
    legal_actions_full,
};
use engine::database::legality::{any_ai_difficulty_is_cedh, validate_cedh_bracket};
use engine::database::{CardDatabase, CardSearchQuery};
use engine::game::engine::{
    apply, apply_for_simulation, resolve_all_fast_forward, ResolveAllCallbackDecision,
    ResolveAllFastForwardResult as BatchResolveResult,
};
use engine::game::preview::{compute_preview_diff, preview_auto_payment_sources};
use engine::game::{
    can_pair_commanders, companion_candidates, deck_copy_limit_for, estimate_bracket,
    evaluate_deck_compatibility, filter_state_for_viewer, finalize_public_state,
    is_brawl_commander_eligible, is_commander_eligible, is_tiny_leader_eligible,
    load_and_hydrate_decks, rehydrate_game_from_card_db, resolve_deck_list,
    signature_spell_selection_policy, start_game, start_game_with_starting_player,
    validate_name_deck_for_format_full, BracketEstimate, DeckCompatibilityRequest, DeckList,
    PlayerDeckList, ReplayPlayer,
};
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::game_state::{PersistedGameState, TrustedGameStateEnvelope, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::{GameAction, GameState, PlayerId, ReplayHeader, ReplayLog};

use engine::game::resolve_player_deck_list;
use engine::starter_decks;
use phase_ai::deck_profile::{ArchetypeClassification, DeckArchetype, DeckProfile};
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx, SeatMutation, SeatState};

fn decode_restored_game_state(json_str: &str) -> Result<GameState, JsValue> {
    serde_json::from_str::<PersistedGameState>(json_str)
        .map(PersistedGameState::into_game_state)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {e}")))
}

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
    /// Engine-level progress-wedge diagnostic: non-fatal signal that an owed
    /// decision has no legal action for any authorized submitter (an engine
    /// anomaly, not a rules outcome). `None` normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    stuck_diagnostic: Option<engine::ai_support::StuckDecisionDiagnostic>,
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

use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::{
    choose_action_with_session, score_candidates_with_session, AiSession, SessionCache,
};
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
    /// Per-thread cache of the last-built `AiSession`, keyed by deck-composition
    /// fingerprint. The WASM bridge cannot hold the session on the stack across
    /// JS round-trips (unlike native `run_ai_actions`), so it caches here and
    /// reuses whenever `deck_pools` are unchanged. Invalidated on game
    /// init/clear/resume; deliberately NOT invalidated on `restore_game_state`
    /// so per-decision pool workers reuse the session.
    static AI_SESSION_CACHE: Cell<SessionCache> = const { Cell::new(SessionCache::new_empty()) };
    /// In-progress recording of GAME_STATE's actions for the Replay system.
    /// Auto-started by `initialize_game` and appended to by `submit_action` on
    /// every successfully-applied action. `None` before any game has started,
    /// or after the recording was invalidated by undo/restore (see
    /// `restore_game_state`). Independent of CARD_DB/GAME_STATE's own
    /// take/set discipline but follows the same panic-resilient pattern.
    static REPLAY_LOG: Cell<Option<ReplayLog>> = const { Cell::new(None) };
    /// A loaded replay being scrubbed/played back by the Replay Viewer.
    /// Entirely independent of GAME_STATE / REPLAY_LOG — loading or seeking a
    /// replay never touches (or requires) a live game.
    static REPLAY_PLAYER: Cell<Option<ReplayPlayer>> = const { Cell::new(None) };
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
const NOT_INITIALIZED_ERR: &str = "NOT_INITIALIZED: Game state not initialized. Call initialize_game or restore_game_state first.";

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

/// Fetch (or lazily build) the per-thread `AiSession` for `state`, reusing the
/// cached session whenever the deck-composition fingerprint is unchanged.
fn ai_session_for(state: &GameState) -> Arc<AiSession> {
    AI_SESSION_CACHE.with(|cell| {
        let mut cache = cell.take();
        let session = cache.get_or_build(state);
        cell.set(cache);
        session
    })
}

/// Drop the cached session so the next `ai_session_for` rebuilds from scratch.
/// Called whenever the game identity changes (init/clear/resume).
fn clear_ai_session_cache() {
    AI_SESSION_CACHE.with(|cell| {
        let mut cache = cell.take();
        cache.clear();
        cell.set(cache);
    });
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
    clear_ai_session_cache();
    REPLAY_LOG.with(|cell| cell.set(None));
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

/// Build a game-scoped AI card-database subset from the loaded full database and
/// the live game state, serialized as the `AiCardSubsetResult` tagged union
/// (`{"kind":"full"}` or `{"kind":"subset","json":...,"count":N}`). The MAIN
/// worker (full CARD_DB + live GAME_STATE) calls this; the AI worker pool loads
/// the returned subset so its WASM instances don't each parse the full ~93MB
/// corpus. Returns `{"kind":"full"}` defensively when the database or game state
/// is absent (the engine is the single authority for this fallback — see
/// `card_subset::build_ai_card_subset_or_full`). The game state is taken out of
/// and restored to the thread-local on every path.
#[wasm_bindgen]
pub fn build_ai_card_subset() -> Result<String, JsValue> {
    let result = CARD_DB.with(|db_cell| {
        let db_ref = db_cell.borrow();
        GAME_STATE.with(|gs_cell| {
            let state_opt = gs_cell.take();
            let r = engine::game::card_subset::build_ai_card_subset_or_full(
                state_opt.as_ref(),
                db_ref.as_ref(),
            );
            gs_cell.set(state_opt);
            r
        })
    });
    serde_json::to_string(&result).map_err(|e| JsValue::from_str(&e.to_string()))
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

/// Search the loaded card database. The engine is the single authority for the
/// rules data search filters on — format legality, set membership, card types,
/// mana value, and colors — so deck-builder search runs here, never as a
/// third-party API call. Returns `{ results, total }` (see `CardSearchResults`),
/// or an error if the database is not loaded or the query is malformed.
#[wasm_bindgen]
pub fn search_cards_js(query: JsValue) -> Result<JsValue, JsValue> {
    let query: CardSearchQuery = serde_wasm_bindgen::from_value(query)
        .map_err(|e| JsValue::from_str(&format!("Invalid search query: {e}")))?;
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        Ok(to_js(&db.search(&query)))
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

/// CR 100.2a / CR 903.5b: The named card's per-card deck-construction copy-limit
/// override, or `null` when the default four-of / singleton limit applies.
/// Serialized as the `DeckCopyLimit` tagged union (`{"type":"Unlimited"}` or
/// `{"type":"UpTo","data":N}`); the frontend must switch on `.type`. The engine
/// is the single authority — the frontend never re-parses Oracle text.
#[wasm_bindgen(js_name = deckCopyLimit)]
pub fn deck_copy_limit(name: &str) -> JsValue {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return JsValue::NULL;
        };
        to_js(&deck_copy_limit_for(db, name))
    })
}

/// Whether the named card can serve as this format's command-zone leader.
/// Reads the engine's MTGJSON-derived `CardFace` leadership fields and
/// format-specific deck-validation predicates.
#[wasm_bindgen(js_name = isCardCommanderEligibleForFormat)]
pub fn is_card_commander_eligible_for_format(name: &str, format: JsValue) -> bool {
    let Ok(format) = serde_wasm_bindgen::from_value::<GameFormat>(format) else {
        return false;
    };
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return false;
        };
        let Some(face) = db.get_face_by_name(name) else {
            return false;
        };
        match format {
            GameFormat::Commander | GameFormat::DuelCommander => is_commander_eligible(face),
            GameFormat::PauperCommander => is_commander_eligible(face),
            GameFormat::TinyLeaders => is_tiny_leader_eligible(face),
            GameFormat::Oathbreaker => face.is_oathbreaker,
            GameFormat::Brawl | GameFormat::HistoricBrawl => is_brawl_commander_eligible(face),
            _ => false,
        }
    })
}

/// CR 702.124: Of `candidates`, which can legally pair with `first_commander`
/// as a co-commander? Applies the full partner family (generic Partner, Partner
/// with [Name], Friends Forever, Character Select, Doctor's Companion, Choose a
/// Background) via the engine's single-authority `can_pair_commanders`. The
/// frontend must not re-derive partner-pairing rules — it filters its candidate
/// list through this. Returns an empty array if the database isn't loaded.
#[wasm_bindgen(js_name = commanderPartnerCandidates)]
pub fn commander_partner_candidates(
    first_commander: String,
    candidates: JsValue,
) -> Result<JsValue, JsValue> {
    let candidates: Vec<String> = serde_wasm_bindgen::from_value(candidates)
        .map_err(|e| JsValue::from_str(&format!("Invalid candidate list: {e}")))?;
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Ok(to_js(&Vec::<String>::new()));
        };
        let eligible: Vec<String> = candidates
            .into_iter()
            .filter(|name| can_pair_commanders(db, &first_commander, name))
            .collect();
        Ok(to_js(&eligible))
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
            ..Default::default()
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

/// Returns the engine-authored Oathbreaker signature-spell selection policy.
#[wasm_bindgen(js_name = signatureSpellSelectionPolicy)]
pub fn signature_spell_selection_policy_js(request: JsValue) -> Result<JsValue, JsValue> {
    let request: DeckCompatibilityRequest = serde_wasm_bindgen::from_value(request)
        .map_err(|e| JsValue::from_str(&format!("Invalid compatibility request: {e}")))?;
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        Ok(to_js(&signature_spell_selection_policy(db, &request)))
    })
}

/// Returns legal Commander-family companion candidates from the main deck.
#[wasm_bindgen(js_name = companionCandidates)]
pub fn companion_candidates_js(request: JsValue) -> Result<JsValue, JsValue> {
    let request: DeckCompatibilityRequest = serde_wasm_bindgen::from_value(request)
        .map_err(|e| JsValue::from_str(&format!("Invalid compatibility request: {e}")))?;
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        Ok(to_js(&companion_candidates(db, &request)))
    })
}

/// Estimates a Commander deck's bracket without touching `GAME_STATE`.
/// Reads `CARD_DB` for bracket signals. Returns `null` (via serde) when the
/// deck has no commander or the card database is not loaded.
#[wasm_bindgen]
pub fn estimate_bracket_for_deck(deck_js: JsValue) -> Result<JsValue, JsError> {
    let deck: PlayerDeckList = serde_wasm_bindgen::from_value(deck_js)
        .map_err(|e| JsError::new(&format!("invalid deck: {e}")))?;
    let result = estimate_bracket_inner(&deck);
    Ok(to_js(&result))
}

/// Pure helper, exposed for native-side tests. Reads `CARD_DB` thread-local.
fn estimate_bracket_inner(deck: &PlayerDeckList) -> Option<BracketEstimate> {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let db = db.as_ref()?;
        estimate_bracket(deck, db)
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
    if let Err(reason) = format_config.validate_for_player_count(count) {
        return to_js(&serde_json::json!({
            "error": true,
            "reasons": [reason],
        }));
    }

    let mut state = GameState::new(format_config.clone(), count, seed);
    state.debug_mode = true;
    // Sandbox capability: in a P2P-host (WASM-authoritative) game, the
    // `submit_action` gate checks `debug_permitted`, mirroring server-core's
    // WebSocket gate. server-core seeds every seat when `allow_debug_actions`
    // is set (session.rs); the WASM host must do the same or sandbox Debug
    // actions are rejected for everyone — the host included. Every seat is
    // permitted by default; the host's grant/revoke flow still narrows it.
    if state.format_config.allow_debug_actions {
        for i in 0..count {
            state.debug_permitted.insert(PlayerId(i));
        }
    }
    let match_config = if !match_config_js.is_null() && !match_config_js.is_undefined() {
        serde_wasm_bindgen::from_value::<MatchConfig>(match_config_js)
            .unwrap_or_else(|_| MatchConfig::default())
    } else {
        MatchConfig::default()
    };
    // CR 732.2a: project the immutable match config (incl. the combo-detector opt-in)
    // onto the runtime `loop_detection` gate via the single engine authority shared
    // with the server path. The detector is player-count-agnostic, so it carries
    // through for local 3-/4-player tables too.
    state.set_match_config(match_config);

    // Captured for the Replay system's `ReplayHeader` once the game actually
    // starts (below) — `None` mirrors the empty-libraries `deck_data: null`
    // path. Cloned at parse time rather than read back from `state` because
    // the engine's resolved/hydrated deck shape (`DeckPayload`) is lossy
    // relative to the name-only `DeckList` a replay needs to re-resolve from
    // scratch on reconstruction.
    let mut recorded_deck_list: Option<DeckList> = None;

    // Load deck data if provided — resolve names via the loaded card database.
    //
    // Each failure mode below MUST surface as a hard error: a game that enters
    // MatchPhase::InGame with empty libraries triggers CR 704.5b on the first
    // draw step and eliminates every player in turn order. The frontend
    // (wasm-adapter.ts:701) already throws on `{ error: true, reasons }`, so
    // returning that envelope here gives the user a real failure message
    // instead of a silently-broken match.
    if !deck_data.is_null() && !deck_data.is_undefined() {
        let deck_list = match serde_wasm_bindgen::from_value::<DeckList>(deck_data) {
            Ok(d) => d,
            Err(e) => {
                return to_js(&serde_json::json!({
                    "error": true,
                    "reasons": [format!("Deck payload deserialization failed: {e}")],
                }));
            }
        };
        recorded_deck_list = Some(deck_list.clone());

        let card_db_missing = CARD_DB.with(|cell| cell.borrow().is_none());
        if card_db_missing {
            return to_js(&serde_json::json!({
                "error": true,
                "reasons": [
                    "Card database not loaded in engine worker. \
                     Call load_card_database before initialize_game.".to_string(),
                ],
            }));
        }

        let validation_error: Option<Vec<String>> = CARD_DB.with(|cell| {
            let borrow = cell.borrow();
            let db = borrow.as_ref().expect("CARD_DB presence checked above");

            // Fixed-deck formats (Momir's Madness) supply the deck from the
            // engine for every seat, so the client submits empty decks — there
            // is nothing client-side to validate. `load_and_hydrate_decks` below
            // fills each seat's library with the engine-owned fixed deck. Gate on
            // the engine predicate, never a format literal.
            if !game_format.supplies_fixed_deck() {
                for (seat, deck) in [
                    ("Player".to_string(), &deck_list.player),
                    ("AI opponent".to_string(), &deck_list.opponent),
                ] {
                    if let Err(reasons) = validate_name_deck_for_format_full(
                        db,
                        &deck.main_deck,
                        &deck.sideboard,
                        &deck.commander,
                        &deck.companion,
                        &deck.planar_deck,
                        &deck.scheme_deck,
                        &deck.signature_spell,
                        game_format,
                        Some(state.match_config.match_type),
                        count as usize,
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
                    if let Err(reasons) = validate_name_deck_for_format_full(
                        db,
                        &deck.main_deck,
                        &deck.sideboard,
                        &deck.commander,
                        &deck.companion,
                        &deck.planar_deck,
                        &deck.scheme_deck,
                        &deck.signature_spell,
                        game_format,
                        Some(state.match_config.match_type),
                        count as usize,
                    ) {
                        return Some(
                            reasons
                                .into_iter()
                                .map(|reason| format!("{seat} deck: {reason}"))
                                .collect(),
                        );
                    }
                }
            }

            // Resolve the JS-supplied deck list against the card database.
            // We deliberately do NOT synthesize missing AI decks here: the
            // engine has no view of which decks are format-legal for the
            // host's catalog (that's `useAiDeckCatalog` on the frontend,
            // which already filters by `selectedFormat`). If the caller
            // passes fewer ai_decks than player_count expects, the
            // `deck_pools.is_empty()`-style invariants below — and the
            // per-player library check at game start — will surface it as
            // a hard error instead of a silently-wrong-format game.
            let payload = resolve_deck_list(db, &deck_list);

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

        // cEDH bracket lock: enforced only when an AI seat runs CEDH difficulty
        // (not merely when a deck carries a bracket-5 tag — bringing a B5 deck
        // against a non-cEDH AI is allowed by spec section 5.5). Gating on AI
        // difficulty is the correct "is this a cEDH game?" signal. Surfaced with
        // a dedicated `cedh_bracket_violation` flag so the adapter maps it to
        // AdapterErrorCode.BRACKET_VIOLATION rather than a generic deck-validation
        // failure. Re-resolves the deck list to read each seat's bracket_tier;
        // this only runs on the cEDH path.
        if any_ai_difficulty_is_cedh(&deck_list.ai_difficulties) {
            let cedh_error: Option<Vec<String>> = CARD_DB.with(|cell| {
                let borrow = cell.borrow();
                let db = borrow.as_ref().expect("CARD_DB presence checked above");
                let payload = resolve_deck_list(db, &deck_list);
                let all_decks: Vec<_> = std::iter::once(&payload.player)
                    .chain(std::iter::once(&payload.opponent))
                    .chain(payload.ai_decks.iter())
                    .collect();
                validate_cedh_bracket(&all_decks)
                    .err()
                    .map(|e| vec![e.to_string()])
            });
            if let Some(reasons) = cedh_error {
                return to_js(&serde_json::json!({
                    "error": true,
                    "cedh_bracket_violation": true,
                    "reasons": reasons,
                }));
            }
        }

        // Defense-in-depth: every seat must have at least one library card
        // before start_game runs. CR 704.5b eliminates a player whose
        // library is empty when they'd draw, so a seat that loads with zero
        // cards is unconditionally a broken game. The most common cause is
        // a JS caller supplying fewer `ai_decks` than the player_count
        // implies (e.g., 3 players but only one AI deck for seat 2 — seat 2
        // ends up with a deck while a missing seat would silently have an
        // empty library). Surface it as a hard error instead of starting.
        let empty_seats: Vec<u8> = state
            .players
            .iter()
            .filter(|p| p.library.is_empty())
            .map(|p| p.id.0)
            .collect();
        if !empty_seats.is_empty() {
            return to_js(&serde_json::json!({
                "error": true,
                "reasons": [format!(
                    "Empty library after deck load for seat(s): {empty_seats:?}. \
                     The JS caller must supply main_deck entries for every seat \
                     (player, opponent, and one ai_decks entry per additional seat).",
                )],
            }));
        }
    }

    // CR 103.1: Start the game with the chosen starting player.
    let result = match first_player {
        Some(0) => start_game_with_starting_player(&mut state, PlayerId(0)),
        Some(1) => start_game_with_starting_player(&mut state, PlayerId(1)),
        _ => start_game(&mut state),
    };

    // Auto-start the Replay recording for this game. Captures exactly the
    // inputs this function was called with — reconstructing from the header
    // alone (see `engine::game::replay::reconstruct_initial_state`) reproduces
    // this same starting state byte-for-byte given the same seed.
    let replay_header = ReplayHeader {
        format_config,
        match_config,
        player_count: count,
        first_player,
        seed,
        deck_data: recorded_deck_list,
    };
    REPLAY_LOG.with(|cell| cell.set(Some(ReplayLog::new(replay_header))));

    GAME_STATE.with(|cell| cell.set(Some(state)));
    clear_ai_session_cache();

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

    // In P2P-host multiplayer mode, debug actions are gated on the
    // sandbox per-player permission set, mirroring the server-core gate.
    // Single-player (non-multiplayer) WASM ignores this branch entirely.
    if matches!(action, GameAction::Debug(_)) && is_multiplayer_mode() {
        let permitted = with_state(|state| state.debug_permitted.contains(&actor)).unwrap_or(false);
        if !permitted {
            return JsValue::from_str(
                "Engine error: debug actions disabled (Sandbox mode off or no permission)",
            );
        }
    }

    if let GameAction::Debug(engine::types::actions::DebugAction::CreateCard {
        ref card_name,
        owner,
        zone,
        attach_to,
        run_etb,
    }) = action
    {
        return handle_debug_create_card(card_name, owner, zone, attach_to, run_etb);
    }

    // Cloned before `apply` consumes `action` — recorded into REPLAY_LOG only
    // on the success path below. CreateCard is handled above and never
    // reaches here.
    let action_for_replay = action.clone();
    let is_debug_action = matches!(action, GameAction::Debug(_));
    match with_state_mut(|state| match apply(state, actor, action) {
        Ok(result) => {
            record_replay_action(is_debug_action, actor, action_for_replay);
            to_js(&result)
        }
        Err(e) => {
            let error_msg = format!("Engine error: {}", e);
            JsValue::from_str(&error_msg)
        }
    }) {
        Ok(val) => val,
        Err(e) => e,
    }
}

/// Record a successfully-applied action into REPLAY_LOG, or invalidate any
/// in-progress recording if it was a (non-CreateCard) debug action.
///
/// Every `GameAction::Debug` variant other than `CreateCard` reaches this
/// point (unlike CreateCard, they mutate state already tracked by
/// `GameState` rather than resolving against the WASM-local `CardDatabase`,
/// so they aren't intercepted earlier in `submit_action`) — but
/// `reconstruct_initial_state` (`game/replay.rs`) never sets `debug_mode`
/// when rebuilding a replay's starting state, so a recorded debug action
/// would hit the `!state.debug_mode` gate in `apply` (`game/engine.rs`) and
/// desync playback. Rather than recording it and failing later, invalidate
/// any in-progress recording here — the same way
/// `handle_debug_create_card_inner` invalidates it for CreateCard — so
/// `export_replay_log` can't produce a log that silently can't be replayed.
///
/// Factored out of `submit_action` so it's testable under plain `cargo test`
/// without going through `to_js`, which requires a JS runtime (see
/// `handle_debug_create_card`'s doc comment for the same split).
fn record_replay_action(is_debug_action: bool, actor: PlayerId, action_for_replay: GameAction) {
    REPLAY_LOG.with(|cell| {
        if is_debug_action {
            cell.set(None);
        } else {
            let mut log = cell.take();
            if let Some(log) = log.as_mut() {
                log.push_action(actor, action_for_replay);
            }
            cell.set(log);
        }
    });
}

fn handle_debug_create_card(
    card_name: &str,
    owner: PlayerId,
    zone: engine::types::zones::Zone,
    attach_to: Option<engine::game::game_object::AttachTarget>,
    run_etb: bool,
) -> JsValue {
    match handle_debug_create_card_inner(card_name, owner, zone, attach_to, run_etb) {
        Ok(result) => to_js(&result),
        Err(msg) => JsValue::from_str(msg),
    }
}

/// Mutation core of `handle_debug_create_card`, factored out so it can be
/// exercised by native unit tests — the `#[wasm_bindgen]`-facing wrapper's
/// success path calls `to_js`, which requires a JS runtime and panics under
/// plain `cargo test`. See `bracket_estimate_tests::estimate_bracket_inner`
/// for the same split.
fn handle_debug_create_card_inner(
    card_name: &str,
    owner: PlayerId,
    zone: engine::types::zones::Zone,
    attach_to: Option<engine::game::game_object::AttachTarget>,
    run_etb: bool,
) -> Result<engine::types::game_state::ActionResult, &'static str> {
    let face = CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err("Engine error: card database not loaded");
        };
        match db.get_face_by_name(card_name) {
            Some(face) => Ok(face.clone()),
            None => Err("Engine error: card not found in database"),
        }
    })?;
    with_state_mut(|state| {
        if !state.debug_mode {
            return Err("Engine error: Debug actions require debug_mode to be enabled");
        }
        if !state.players.iter().any(|p| p.id == owner) {
            return Err("Engine error: Debug: invalid owner player id");
        }
        // Debug-spawned cards are resolved against the WASM-local CARD_DB and
        // never recorded into REPLAY_LOG (unlike normal actions in
        // `submit_action`), so a faithful replay can't reconstruct this
        // mutation. Invalidate any in-progress recording here, the same way
        // `restore_game_state` invalidates on a history-breaking state swap,
        // so `export_replay_log` can't produce a log that silently omits a
        // debug spawn.
        REPLAY_LOG.with(|cell| cell.set(None));
        // CR 400.7: For battlefield destination, stage the object in Hand
        // first, then route through the real ETB pipeline so replacements,
        // triggers, and SBAs all fire. Direct creation in Battlefield (the
        // old path) bypassed all of these and left Auras stranded with
        // `attached_to: None` plus a `entered_battlefield_turn` stamp that
        // survived later zone moves.
        let staging_zone = if zone == engine::types::zones::Zone::Battlefield {
            engine::types::zones::Zone::Hand
        } else {
            zone
        };
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let obj_id =
            engine::game::create_object(state, card_id, owner, face.name.clone(), staging_zone);
        let obj = state.objects.get_mut(&obj_id).expect("just created");
        engine::game::printed_cards::apply_card_face_to_object(obj, &face);
        state.layers_dirty.mark_full();

        // Hydrate `back_face` for dual-faced spawns (MDFC, Transform, Adventure,
        // Omen, Meld, Prepare). `apply_card_face_to_object` only writes the named
        // face; without this, a debug-spawned Esika, God of the Tree has no
        // Prismatic Bridge back face, so Ctrl-to-flip preview and MDFC face-choice
        // casting silently no-op until a page refresh re-runs deck hydration. This
        // is the same canonical primitive `load_and_hydrate_decks` uses, so the
        // debug-spawn path can't drift from the normal load path. The new object
        // already carries `printed_ref` (set by `apply_card_face_to_object`), which
        // rehydrate uses to resolve the card and its other face.
        CARD_DB.with(|cell| {
            if let Some(db) = cell.borrow().as_ref() {
                engine::game::printed_cards::rehydrate_game_from_card_db(state, db);
            }
        });

        // CR 303.4f + CR 704.5n: When the user picks an attachment target,
        // wire the host through the engine's attach resolvers BEFORE routing
        // through the ETB pipeline. The resolvers (`attach_to`,
        // `attach_to_player`) own all legality checks (CR 301.5 / 303.4i,
        // `CantBeAttached` / `CantBeEnchanted` / `CantBeEquipped` statics) and
        // back-link bookkeeping (host's `attachments` list, `layers_dirty`),
        // so the WASM bridge stays a thin transport layer with zero attachment
        // logic. Doing this pre-ETB means the post-ETB SBA pass sees the
        // attachment with a legal host instead of an orphan (CR 704.5n) and
        // any "becomes attached" trigger fires from the same resolved state
        // a real cast would produce. Only honored for Battlefield spawns —
        // Auras in Hand/Library/Exile/Graveyard have no battlefield host.
        if zone == engine::types::zones::Zone::Battlefield {
            if let Some(target) = attach_to {
                use engine::game::game_object::AttachTarget;
                match target {
                    AttachTarget::Object(target_id) => {
                        if state.objects.contains_key(&target_id) {
                            engine::game::effects::attach::attach_to(state, obj_id, target_id);
                        }
                    }
                    AttachTarget::Player(target_player) => {
                        if state.players.iter().any(|p| p.id == target_player) {
                            engine::game::effects::attach::attach_to_player(
                                state,
                                obj_id,
                                target_player,
                            );
                        }
                    }
                }
            }
        }

        let result = if zone == engine::types::zones::Zone::Battlefield {
            engine::game::route_debug_create_to_battlefield(state, obj_id, run_etb)
        } else {
            engine::types::game_state::ActionResult {
                events: vec![],
                waiting_for: state.waiting_for.clone(),
                log_entries: vec![],
            }
        };

        engine::game::public_state::bump_state_revision(state);
        engine::game::public_state::mark_public_state_all_dirty(state);
        engine::game::public_state::finalize_public_state(state);
        Ok(result)
    })
    .unwrap_or(Err(NOT_INITIALIZED_ERR))
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
        // Single-player WASM: the human is always PlayerId(0). Scope web-slinging
        // costs to the human's own hand even on this raw/unfiltered path.
        to_js(&engine::game::derived_views::ClientGameStateRef::wrap(
            state,
            Some(PlayerId(0)),
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
        to_js(
            &engine::game::derived_views::ClientGameStateRef::wrap_filtered(
                state,
                &filtered,
                Some(PlayerId(viewer)),
            ),
        )
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Get the legal actions, auto-pass recommendation, and spell costs for the current game state.
/// Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<ObjectId, ManaCost> }`.
#[wasm_bindgen]
pub fn get_legal_actions_js() -> JsValue {
    match with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let (actions, spell_costs, legal_actions_by_object) = legal_actions_full(state);
        let auto_pass = auto_pass_recommended(state, &actions);
        to_js(&LegalActionsResult {
            actions,
            auto_pass_recommended: auto_pass,
            spell_costs,
            legal_actions_by_object,
            stuck_diagnostic: engine::ai_support::stuck_decision_diagnostic(state),
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
    match with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        to_js(&legal_actions_result_for_viewer(
            state,
            PlayerId(player_id as u8),
        ))
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Read-only preview of cast-time target slots for a currently castable spell.
/// Returns `[]` for uncastable, untargeted, or target-ambiguous casts.
#[wasm_bindgen]
pub fn legal_targets_for_castable_js(object_id: u32) -> JsValue {
    match with_state(|state| {
        let slots = if let WaitingFor::Priority { player } = &state.waiting_for {
            let probe = engine::game::casting::PriorityCastProbe::new(state, *player);
            engine::game::casting::legal_target_slots_for_castable_spell_with_probe(
                probe.state(),
                *player,
                ObjectId(object_id as u64),
                Some(&probe),
            )
        } else {
            Vec::new()
        };
        to_js(&slots)
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Batch variant for hover/drag clients that need previews for many castable
/// cards. The engine flushes layers once and reuses that snapshot for every id.
#[wasm_bindgen]
pub fn legal_targets_for_castables_js(object_ids: JsValue) -> JsValue {
    let object_ids: Vec<u32> = serde_wasm_bindgen::from_value(object_ids).unwrap_or_default();
    match with_state(|state| {
        let object_ids = object_ids
            .into_iter()
            .map(|id| ObjectId(id as u64))
            .collect::<Vec<_>>();
        let slots =
            engine::game::casting::legal_target_slots_for_castable_spells(state, object_ids);
        to_js(&slots)
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
    /// Engine-level progress-wedge diagnostic: non-fatal signal that an owed
    /// decision has no legal action for any authorized submitter (an engine
    /// anomaly, not a rules outcome). `None` normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    stuck_diagnostic: Option<engine::ai_support::StuckDecisionDiagnostic>,
}

fn legal_actions_result_for_viewer(state: &GameState, viewer: PlayerId) -> LegalActionsResult {
    let (actions, spell_costs, legal_actions_by_object) = legal_actions_for_viewer(state, viewer);
    let auto_pass_recommended = auto_pass_recommended_for_viewer(state, viewer, &actions);
    LegalActionsResult {
        actions,
        auto_pass_recommended,
        spell_costs,
        legal_actions_by_object,
        stuck_diagnostic: engine::ai_support::stuck_decision_diagnostic(state),
    }
}

#[cfg(test)]
mod viewer_priority_tests {
    use super::*;
    use engine::types::game_state::{PriorityPassingMode, WaitingFor};
    use engine::types::phase::Phase;

    #[test]
    fn viewer_result_routes_turn_control_recommendation_only_to_controller() {
        let controller = PlayerId(0);
        let controlled = PlayerId(1);
        let mut state = GameState::new_two_player(19);
        state.active_player = controlled;
        state.phase = Phase::End;
        state.waiting_for = WaitingFor::Priority { player: controlled };
        state.priority_player = controller;
        state.turn_decision_controller = Some(controller);
        state
            .priority_passing_modes
            .insert(controller, PriorityPassingMode::SkipLowUseWindows);

        let controller_result = legal_actions_result_for_viewer(&state, controller);
        assert!(
            controller_result
                .actions
                .iter()
                .any(|action| matches!(action, GameAction::PassPriority)),
            "the authorized controller must receive the controlled seat's priority actions"
        );
        assert!(controller_result.auto_pass_recommended);

        let controlled_result = legal_actions_result_for_viewer(&state, controlled);
        assert!(controlled_result.actions.is_empty());
        assert!(
            auto_pass_recommended(&state, &controlled_result.actions),
            "reach guard: the unscoped recommendation would leak true to the controlled viewer"
        );
        assert!(
            !controlled_result.auto_pass_recommended,
            "the controlled viewer is not authorized to act and must receive false"
        );
    }
}

#[wasm_bindgen]
pub fn get_viewer_snapshot_js(player_id: u32) -> JsValue {
    match with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let viewer = PlayerId(player_id as u8);
        let filtered = filter_state_for_viewer(state, viewer);
        let legal = legal_actions_result_for_viewer(state, viewer);
        to_js(&ViewerSnapshot {
            state: filtered,
            actions: legal.actions,
            auto_pass_recommended: legal.auto_pass_recommended,
            spell_costs: legal.spell_costs,
            legal_actions_by_object: legal.legal_actions_by_object,
            stuck_diagnostic: legal.stuck_diagnostic,
        })
    }) {
        Ok(val) => val,
        Err(_) => JsValue::NULL,
    }
}

/// Issue #5468: non-mutating dry-run of `action` for `actor`. Runs the action on
/// a throwaway clone (the live `GAME_STATE` is never touched) and returns the
/// PUBLIC deltas — life-total changes, public-zone object transitions, created
/// tokens, and objects that ceased to exist — a viewer could observe, for
/// hover-preview UX ("this kills that", "you take 4").
///
/// Hidden-zone movements never leak: the diff is taken over
/// `filter_state_for_viewer` snapshots (so any identity the viewer can't see is
/// already redacted), AND a transition is surfaced only when at least one
/// endpoint is a public zone (see `engine::game::preview`), so a fully-hidden
/// hand↔library draw is elided even for the acting player's opponents. Returns
/// an error string when `action` is malformed or illegal in the current state.
#[wasm_bindgen]
pub fn preview_action_js(actor: u8, action: JsValue) -> JsValue {
    let action: GameAction = match serde_wasm_bindgen::from_value(action) {
        Ok(a) => a,
        Err(e) => {
            return JsValue::from_str(&format!("Engine error: failed to deserialize action: {e}"));
        }
    };
    let actor = PlayerId(actor);
    match with_state(|state| {
        // Simulate on a throwaway clone. `apply_for_simulation` is the same rules
        // resolution the AI look-ahead uses; it mutates only `sim`, never the
        // live `GAME_STATE` this closure borrows immutably.
        let mut sim = state.clone();
        engine::game::layers::flush_layers(&mut sim);
        let before = filter_state_for_viewer(&sim, actor);
        match apply_for_simulation(&mut sim, actor, action) {
            Ok(_) => {
                let after = filter_state_for_viewer(&sim, actor);
                Ok(compute_preview_diff(&before, &after))
            }
            Err(e) => Err(format!("Engine error: {e}")),
        }
    }) {
        Ok(Ok(diff)) => to_js(&diff),
        Ok(Err(msg)) => JsValue::from_str(&msg),
        Err(e) => e,
    }
}

/// Non-mutating automatic spell-payment preview. The engine simulates the
/// exact, currently legal `CastSpell` action and returns the permanent ids that
/// produced mana before that spell was committed to the stack. It returns an
/// empty array when the cast needs another choice before payment can be final.
#[wasm_bindgen]
pub fn preview_mana_payment_js(actor: u8, action: JsValue) -> JsValue {
    let action: GameAction = match serde_wasm_bindgen::from_value(action) {
        Ok(action) => action,
        Err(error) => {
            return JsValue::from_str(&format!(
                "Engine error: failed to deserialize action: {error}"
            ));
        }
    };

    match with_state(|state| {
        preview_auto_payment_sources(state, PlayerId(actor), &action)
            .map_err(|error| format!("Engine error: {error}"))
    }) {
        Ok(Ok(sources)) => to_js(&sources),
        Ok(Err(message)) => JsValue::from_str(&message),
        Err(error) => error,
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

/// Returns the engine-typed catalog of debug-spawnable token presets,
/// loaded from `crates/engine/data/known-tokens.toml`. Read by the debug UI
/// to populate the Create Token dropdown — frontend never derives this list.
#[wasm_bindgen]
pub fn list_token_presets_js() -> JsValue {
    let presets = engine::game::token_presets::known_token_presets();
    to_js(presets)
}

/// Export the current game state as a JSON string.
/// Used by the engine worker to transfer state to AI workers for root parallelism.
#[wasm_bindgen]
pub fn export_game_state_json() -> Result<String, JsValue> {
    with_state_mut(|state| {
        // Capture the live ChaCha20 stream position so `restore_game_state` can
        // fast-forward to it (issue #5466); `rng` is `#[serde(skip)]`. The
        // randomness logic lives in the engine (`GameState::capture_rng_word_pos`),
        // keeping this WASM boundary a thin serialization step.
        state.capture_rng_word_pos();
        serde_json::to_string(&TrustedGameStateEnvelope::capture(state.clone()))
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
    let mut state = decode_restored_game_state(json_str)?;
    // Reseed the skipped `rng` and fast-forward it to the offset captured at
    // export (issue #5466) so the restored game draws the values that would have
    // come NEXT rather than replaying from origin. The engine owns this logic
    // (`GameState::rehydrate_rng`); pre-#5466 snapshots carry `rng_word_pos == 0`
    // and reproduce the previous rewind-to-origin behavior.
    state.rehydrate_rng();
    state.debug_mode = true;
    CARD_DB.with(|cell| {
        if let Some(db) = cell.borrow().as_ref() {
            rehydrate_game_from_card_db(&mut state, db);
        }
    });
    finalize_public_state(&mut state);
    GAME_STATE.with(|cell| cell.set(Some(state)));
    // Restoring (undo, or resuming a save from a fresh worker that never saw
    // `initialize_game`) invalidates any in-progress recording — the restored
    // state's history no longer matches the recorded action sequence.
    REPLAY_LOG.with(|cell| cell.set(None));
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

    let mut state = decode_restored_game_state(json_str)?;

    // Deliberately re-roll a fresh seed on multiplayer host resume so continued
    // play diverges from any pre-save sequence (mirrors server-core). This is a
    // multiplayer-resume policy choice, independent of the #5466 undo fix: even
    // though the stream position now round-trips via `rng_word_pos`, a resumed
    // host should NOT replay the exact saved randomness. Reset the offset to 0
    // to match the freshly seeded stream.
    let fresh_seed: u64 = rand::rng().random();
    state.rng_seed = fresh_seed;
    state.rng = ChaCha20Rng::seed_from_u64(fresh_seed);
    state.rng_word_pos = 0;

    CARD_DB.with(|cell| {
        if let Some(db) = cell.borrow().as_ref() {
            rehydrate_game_from_card_db(&mut state, db);
        }
    });
    finalize_public_state(&mut state);

    GAME_STATE.with(|cell| cell.set(Some(state)));
    MULTIPLAYER_MODE.with(|cell| cell.set(true));
    clear_ai_session_cache();
    // Multiplayer games are out of scope for v1 recording (see
    // `crates/engine/src/types/replay.rs`); ensure no stale local-game
    // recording from this worker's previous session lingers.
    REPLAY_LOG.with(|cell| cell.set(None));
    Ok(())
}

// ── Replay system ───────────────────────────────────────────────────────
//
// Recording: `initialize_game` auto-starts a `ReplayLog` (REPLAY_LOG) and
// `submit_action` appends every successfully-applied action to it. See
// `engine::types::replay` and `engine::game::replay` for the reconstruction
// model — a replay carries no per-turn snapshots, only the inputs needed to
// reconstruct the starting state plus the ordered action sequence.
//
// Playback: entirely separate from the live game. `load_replay_for_playback`
// parses an exported log into a `ReplayPlayer` (REPLAY_PLAYER) that the
// Replay Viewer scrubs with `replay_seek_js`. Loading or seeking a replay
// never touches GAME_STATE / REPLAY_LOG.

/// Whether the current game has an in-progress replay recording. `false`
/// before any game has started, or after the recording was invalidated by
/// undo/restore (see `restore_game_state`).
#[wasm_bindgen]
pub fn has_replay_recording() -> bool {
    REPLAY_LOG.with(|cell| {
        let log = cell.take();
        let present = log.is_some();
        cell.set(log);
        present
    })
}

/// Serialize the current game's replay recording to a JSON string — the
/// format `load_replay_for_playback` reads back. Errors if no game has been
/// initialized in this worker (or the recording was invalidated by undo).
#[wasm_bindgen]
pub fn export_replay_log() -> Result<String, JsValue> {
    REPLAY_LOG.with(|cell| {
        let log = cell.take();
        let result = match &log {
            Some(log) => serde_json::to_string(log)
                .map_err(|e| JsValue::from_str(&format!("Failed to serialize replay log: {e}"))),
            None => Err(JsValue::from_str(
                "No replay recording available. Start a game first, or it was \
                 invalidated by an undo/restore.",
            )),
        };
        cell.set(log);
        result
    })
}

/// Load a replay log (the JSON produced by `export_replay_log`) for
/// scrubbing/playback. Independent of the live `GAME_STATE` — does not
/// require, and does not affect, an active game. Uses the loaded `CARD_DB`
/// to resolve the recorded deck list when reconstructing the starting
/// state — and errors (rather than silently reconstructing empty
/// libraries) if the replay carries deck data but no card database is
/// loaded; see `ReplayError::MissingCardDatabase`. Returns the total number
/// of recorded actions; valid `replay_seek_js` targets are `0..=length`.
#[wasm_bindgen]
pub fn load_replay_for_playback(json_str: &str) -> Result<u32, JsValue> {
    let log: ReplayLog = serde_json::from_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse replay log: {e}")))?;
    let player = CARD_DB
        .with(|cell| {
            let db = cell.borrow();
            ReplayPlayer::load(log, db.as_ref())
        })
        .map_err(|e| JsValue::from_str(&format!("Engine error: {e}")))?;
    let len = player.len();
    REPLAY_PLAYER.with(|cell| cell.set(Some(player)));
    Ok(len)
}

/// Total number of recorded actions in the loaded replay, or `0` if none is loaded.
#[wasm_bindgen]
pub fn replay_length_js() -> u32 {
    REPLAY_PLAYER.with(|cell| {
        let player = cell.take();
        let len = player.as_ref().map(ReplayPlayer::len).unwrap_or(0);
        cell.set(player);
        len
    })
}

/// The loaded replay's header (format/match config, player count, seed,
/// deck data), or `null` if none is loaded. Lets the viewer show "vs. <deck>"
/// chrome without re-deriving it from the action sequence.
#[wasm_bindgen]
pub fn replay_header_js() -> JsValue {
    REPLAY_PLAYER.with(|cell| {
        let player = cell.take();
        let header = player
            .as_ref()
            .map(|p| to_js(p.header()))
            .unwrap_or(JsValue::NULL);
        cell.set(player);
        header
    })
}

/// Seek the loaded replay to `target` (clamped to the recording's length) and
/// return the reconstructed state at that point, wrapped the same way
/// `get_game_state` wraps the live state. Returns `Ok(null)` only when no
/// replay is loaded — a reconstruction desync (`ReplayError::Desync`, an
/// engine-version mismatch between recording and playback, not a rules
/// outcome) is a real failure and must not be silently swallowed into the
/// same null the caller uses for "nothing loaded"; it throws instead, like
/// every other fallible engine entry point that returns `Result<_, JsValue>`.
#[wasm_bindgen]
pub fn replay_seek_js(target: u32) -> Result<JsValue, JsValue> {
    REPLAY_PLAYER.with(|cell| {
        let mut player = cell.take();
        let result = match player.as_mut() {
            Some(player) => match player.seek(target) {
                Ok(state) => Ok(to_js(
                    &engine::game::derived_views::ClientGameStateRef::wrap(
                        state,
                        Some(PlayerId(0)),
                    ),
                )),
                Err(e) => Err(JsValue::from_str(&format!("Engine error: {e}"))),
            },
            None => Ok(JsValue::NULL),
        };
        cell.set(player);
        result
    })
}

/// Discard the loaded replay (if any). Safe to call even when none is loaded.
#[wasm_bindgen]
pub fn clear_replay_playback() {
    REPLAY_PLAYER.with(|cell| cell.set(None));
}

/// Get the AI's chosen action for the current game state.
/// `difficulty` is one of: "VeryEasy", "Easy", "Medium", "Hard", "VeryHard",
/// "CEDH" (case-insensitive; see `AiDifficulty::from_label`).
/// `player_id` is the seat index of the AI player (0-based).
#[wasm_bindgen]
pub fn get_ai_action(difficulty: &str, player_id: u8) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);

    with_state_mut(|state| {
        // Freshly-restored states carry `layers_dirty = Full` and a conservative
        // all-present `static_mode_presence`; flush before read-only candidate
        // generation so derived state and the presence index are precise
        // (mirrors `get_legal_actions_js`). No-op when layers are clean.
        engine::game::layers::flush_layers(state);
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);

        let ai_player = PlayerId(player_id);
        let mut rng = rand::rng();
        let session = ai_session_for(state);

        match choose_action_with_session(state, ai_player, &config, &mut rng, &session) {
            Some(action) => Ok(to_js(&action)),
            None => Ok(JsValue::NULL),
        }
    })?
}

/// Score all candidate actions and return `[GameAction, score]` tuples.
/// Used by AI workers for root parallelism — each worker scores independently,
/// then results are merged on the main thread.
/// `rng_seed` seeds the game state's RNG so each worker's beam search explores
/// different orderings, producing diverse score vectors.
#[wasm_bindgen]
pub fn get_ai_scored_candidates(
    difficulty: &str,
    player_id: u8,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);

    with_state_mut(|state| {
        // Pool workers restore a deserialized state per decision: `layers_dirty =
        // Full`, presence index conservatively all-present. Flush before scoring so
        // candidate generation runs on precise derived state (mirrors
        // `get_legal_actions_js`). No-op when layers are clean.
        engine::game::layers::flush_layers(state);
        // Re-seed the state RNG so each parallel worker explores different
        // beam-search rollout paths and tie-breaking orders.
        state.rng = ChaCha20Rng::seed_from_u64(rng_seed);
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        let ai_player = PlayerId(player_id);
        let session = ai_session_for(state);
        let scored = score_candidates_with_session(state, ai_player, &config, &session);
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
    let ai_difficulty = AiDifficulty::from_label(difficulty);
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
/// Returns a compact `BatchResolveResult` with the final `WaitingFor` and a
/// count of items resolved. The Resolve All UI does not animate individual
/// events, so the WASM boundary intentionally returns empty event/log arrays
/// instead of serializing thousands of records for pathological stacks.
///
/// Stop conditions (all CR-compliant):
/// - Stack empties
/// - Stack grows beyond the chunk-origin depth
/// - An interactive `WaitingFor` appears (target selection, scry, etc.)
/// - An unknown/non-requester human actor receives priority
/// - AI has no action for its priority decision
/// - Game ends
/// - Safety cap reached (prevents infinite loops from cascading triggers)
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiSeatConfig {
    player_id: u8,
    difficulty: String,
}

fn resolve_all_inner(
    state: &mut GameState,
    requester: PlayerId,
    ai_seats: &[AiSeatConfig],
    max_resolutions: u32,
    rng: &mut impl Rng,
) -> BatchResolveResult {
    // The first AI decision in the fast-forward loop can run before any
    // `apply()` (which would flush internally); flush up front so it sees
    // precise derived state + presence index. No-op when layers are clean.
    engine::game::layers::flush_layers(state);
    let session = ai_session_for(state);
    resolve_all_fast_forward(state, requester, max_resolutions, |state, actor| {
        if let Some(seat) = ai_seats
            .iter()
            .find(|seat| PlayerId(seat.player_id) == actor)
        {
            let ai_difficulty = AiDifficulty::from_label(&seat.difficulty);
            let config =
                create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
            match choose_action_with_session(state, actor, &config, rng, &session) {
                Some(action) => ResolveAllCallbackDecision::Action(action),
                None => ResolveAllCallbackDecision::Stop,
            }
        } else {
            ResolveAllCallbackDecision::Stop
        }
    })
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

    with_state_mut(|state| {
        let mut rng = rand::rng();
        let mut result = resolve_all_inner(state, requester, &ai_seats, max_resolutions, &mut rng);
        // A Resolve All burst applies real actions directly via
        // `apply_action_boundary_with_stack_limit` (bypassing `submit_action`,
        // which is the only other place REPLAY_LOG is appended to) — without
        // this, an exported replay would silently omit every action a player
        // fast-forwarded through, and playback would desync from the game
        // they actually played. `recorded_actions` is `#[serde(skip)]`, so
        // draining it here has no effect on the JS-visible result shape.
        if !result.recorded_actions.is_empty() {
            REPLAY_LOG.with(|cell| {
                let mut log = cell.take();
                if let Some(log) = log.as_mut() {
                    for (actor, action) in result.recorded_actions.drain(..) {
                        log.push_action(actor, action);
                    }
                }
                cell.set(log);
            });
        }
        result.events.clear();
        result.log_entries.clear();
        Ok(to_js(&result))
    })?
}

/// Apply a seat mutation to a seat state, using the TLS card database for deck
/// resolution. Both arguments are JSON strings; returns `{ state, delta }` as
/// a JS object on success, or a JS error string on failure.
#[wasm_bindgen]
pub fn apply_seat_mutation(state_json: &str, mutation_json: &str) -> Result<JsValue, JsValue> {
    struct WasmDeckResolver;
    impl DeckResolver for WasmDeckResolver {
        fn resolve(&self, choice: &DeckChoice) -> Result<PlayerDeckList, String> {
            let deck_data = match choice {
                DeckChoice::Random => starter_decks::random_starter_deck(),
                DeckChoice::Named(name) => starter_decks::find_starter_deck(name)
                    .ok_or_else(|| format!("Starter deck not found: {name}"))?,
                DeckChoice::DeckList(deck) => deck.as_ref().clone(),
            };
            // Stay at the name-only layer — `wasm.initialize_game` re-resolves
            // against `CARD_DB` when the game actually starts, so resolving
            // here would be wasted work and would force a name-vs-resolved
            // shape coercion at every JS boundary. The declared bracket_tier is
            // carried through so a cEDH seat's declaration survives the round-trip.
            Ok(PlayerDeckList {
                main_deck: deck_data.main_deck,
                sideboard: deck_data.sideboard,
                commander: deck_data.commander,
                companion: deck_data.companion,
                attraction_deck: deck_data.attraction_deck,
                planar_deck: deck_data.planar_deck,
                scheme_deck: deck_data.scheme_deck,
                contraption_deck: deck_data.contraption_deck,
                sticker_sheets: deck_data.sticker_sheets,
                signature_spell: deck_data.signature_spell,
                bracket_tier: deck_data.bracket_tier,
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

/// Project an authoritative seat view from Rust so frontend transports do not
/// need to understand format topology details.
#[wasm_bindgen]
pub fn project_seat_view(state_json: &str) -> Result<JsValue, JsValue> {
    let state: SeatState = serde_json::from_str(state_json)
        .map_err(|e| JsValue::from_str(&format!("Invalid SeatState: {e}")))?;
    Ok(to_js(&state.to_view()))
}

#[cfg(test)]
mod bracket_estimate_tests {
    use super::*;
    use engine::database::{BracketLists, CardDatabase};
    use engine::game::bracket_estimate::CommanderBracketTier;
    use engine::game::deck_loading::PlayerDeckList;

    #[test]
    fn estimate_bracket_inner_returns_b3_for_one_game_changer() {
        let db = CardDatabase::from_json_str(
            r#"{
                "smothering tithe": {
                    "name": "Smothering Tithe",
                    "mana_cost": { "type": "NoCost" },
                    "card_type": { "supertypes": [], "core_types": ["Enchantment"], "subtypes": [] },
                    "power": null,
                    "toughness": null,
                    "loyalty": null,
                    "defense": null,
                    "oracle_text": null,
                    "abilities": [],
                    "triggers": [],
                    "static_abilities": [],
                    "replacements": [],
                    "keywords": [],
                    "bracket_signals": {
                        "game_changer": true,
                        "mass_land_denial": false,
                        "extra_turn": false,
                        "efficient_tutor": false
                    }
                }
            }"#,
        )
        .unwrap()
        .with_bracket_lists(BracketLists::from_json_str(r#"{"version":"t"}"#).unwrap());
        CARD_DB.with(|c| *c.borrow_mut() = Some(db));

        let deck = PlayerDeckList {
            commander: vec!["Atraxa, Praetors' Voice".into()],
            main_deck: vec!["Smothering Tithe".into(), "Forest".into()],
            sideboard: vec![],
            ..Default::default()
        };
        let result = estimate_bracket_inner(&deck);
        let est = result.expect("estimate present");
        assert_eq!(est.tier, CommanderBracketTier::Upgraded);

        // Reset to avoid leaking state to other tests in this module.
        CARD_DB.with(|c| *c.borrow_mut() = None);
    }

    #[test]
    fn estimate_bracket_inner_returns_none_with_no_db() {
        CARD_DB.with(|c| *c.borrow_mut() = None);
        let deck = PlayerDeckList {
            commander: vec!["Cmdr".into()],
            main_deck: vec!["Forest".into()],
            sideboard: vec![],
            ..Default::default()
        };
        assert!(estimate_bracket_inner(&deck).is_none());
    }
}

#[cfg(test)]
mod resolve_all_tests {
    use super::*;
    use engine::types::ability::{Effect, ResolvedAbility};
    use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
    use engine::types::identifiers::ObjectId;

    fn no_op_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: ResolvedAbility::new(Effect::NoOp, vec![], object_id, controller),
            },
        }
    }

    fn priority_state(semantic_seat: PlayerId, stack: Vec<StackEntry>) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: semantic_seat,
        };
        state.priority_player = semantic_seat;
        state.stack = stack.into_iter().collect();
        state
    }

    #[test]
    fn resolve_all_tls_production_path_substitute_routes_controlled_priority() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.priority_player = PlayerId(0);
        state.priority_passes.insert(PlayerId(0));
        GAME_STATE.with(|cell| cell.set(Some(state)));

        let ai_seats: Vec<AiSeatConfig> = serde_json::from_str("[]").unwrap();
        let result = with_state_mut(|state| {
            let mut rng = ChaCha20Rng::seed_from_u64(13);
            resolve_all_inner(state, PlayerId(0), &ai_seats, 0, &mut rng)
        })
        .unwrap();

        assert_eq!(result.items_resolved, 1);
        with_state(|state| assert!(state.stack.is_empty())).unwrap();
        clear_game_state();
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use engine::game::deck_loading::create_object_from_card_face;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect, QuantityExpr,
        ResolvedAbility, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
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
            is_commander: false,
            deck_copy_limit: None,
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
                    "target_prompt": null
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

    fn no_op_stack_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: ResolvedAbility::new(Effect::NoOp, vec![], object_id, controller),
            },
        }
    }

    #[test]
    fn resolve_all_exported_path_routes_controlled_priority_to_requester() {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.priority_player = PlayerId(0);
        state.priority_passes.insert(PlayerId(0));
        state.stack.push_back(no_op_stack_entry(1, PlayerId(1)));
        GAME_STATE.with(|cell| cell.set(Some(state)));

        let value = resolve_all(0, "[]", 0).unwrap();
        let result: BatchResolveResult = serde_wasm_bindgen::from_value(value).unwrap();

        assert_eq!(result.items_resolved, 1);
        let restored: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();
        assert!(restored.stack.is_empty());
        clear_game_state();
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

#[cfg(test)]
mod replay_bridge_tests {
    use super::*;
    use engine::types::game_state::WaitingFor;

    /// Exercises the bridge wiring (auto-start in `initialize_game`, append
    /// in `submit_action`, clear in `restore_game_state`) through the
    /// inner helpers rather than the `#[wasm_bindgen]` entry points
    /// themselves — those return their result via `to_js`, which calls the
    /// real `JSON.parse` JS binding and panics outside a wasm32 runtime (see
    /// `bracket_estimate_tests` / `resolve_all_tests` above, which follow the
    /// same convention). Deterministic reconstruction itself is covered
    /// end-to-end by `crates/engine/src/game/replay.rs`'s tests; this test's
    /// job is narrower — proving the thread-local plumbing actually fires.
    #[test]
    fn replay_log_records_actions_and_survives_export_import_round_trip() {
        clear_game_state();
        clear_replay_playback();

        let mut state = GameState::new_two_player(99);
        let start_result = start_game(&mut state);
        let _ = start_result;

        let header = ReplayHeader {
            format_config: state.format_config.clone(),
            match_config: state.match_config,
            player_count: state.players.len() as u8,
            first_player: Some(state.active_player.0),
            seed: state.rng_seed,
            deck_data: None,
        };
        REPLAY_LOG.with(|cell| cell.set(Some(ReplayLog::new(header))));
        GAME_STATE.with(|cell| cell.set(Some(state)));

        assert!(
            has_replay_recording(),
            "seeding REPLAY_LOG must be observable via has_replay_recording"
        );

        // Mirror what `submit_action` does on every successful action: apply,
        // then record it via the same `record_replay_action` helper.
        for _ in 0..6 {
            let waiting = with_state(|state| state.waiting_for.clone()).expect("game initialized");
            let WaitingFor::Priority { player } = waiting else {
                break;
            };
            let applied =
                with_state_mut(|state| apply(state, player, GameAction::PassPriority).is_ok())
                    .expect("game initialized");
            assert!(
                applied,
                "passing priority while waiting on it is always legal"
            );
            record_replay_action(false, player, GameAction::PassPriority);
        }

        let replay_json =
            export_replay_log().expect("a recording should exist after at least one action");
        assert!(
            replay_json.contains("PassPriority"),
            "exported JSON should contain the recorded actions"
        );

        let length =
            load_replay_for_playback(&replay_json).expect("exported replay should load back");
        assert!(
            length >= 4,
            "expected several recorded priority passes, got {length}"
        );
        assert_eq!(replay_length_js(), length);

        clear_replay_playback();
        assert_eq!(
            replay_length_js(),
            0,
            "clear_replay_playback should drop the loaded replay"
        );
        clear_game_state();
    }

    #[test]
    fn restore_game_state_invalidates_the_in_progress_recording() {
        clear_game_state();

        let state = GameState::new_two_player(7);
        REPLAY_LOG.with(|cell| {
            cell.set(Some(ReplayLog::new(ReplayHeader {
                format_config: state.format_config.clone(),
                match_config: state.match_config,
                player_count: state.players.len() as u8,
                first_player: Some(0),
                seed: state.rng_seed,
                deck_data: None,
            })))
        });
        assert!(has_replay_recording());

        let json = serde_json::to_string(&state).unwrap();
        restore_game_state(&json).expect("restore should succeed");

        assert!(
            !has_replay_recording(),
            "undo/restore must invalidate the recording — it no longer matches \
             the restored state's history"
        );

        clear_game_state();
    }

    #[test]
    fn debug_create_card_invalidates_the_in_progress_recording() {
        use engine::database::CardDatabase;

        clear_game_state();
        let db = CardDatabase::from_json_str(
            r#"{
                "test card": {
                    "name": "Test Card",
                    "mana_cost": { "type": "NoCost" },
                    "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                    "power": "1",
                    "toughness": "1",
                    "loyalty": null,
                    "defense": null,
                    "oracle_text": null,
                    "abilities": [],
                    "triggers": [],
                    "static_abilities": [],
                    "replacements": [],
                    "keywords": []
                }
            }"#,
        )
        .unwrap();
        CARD_DB.with(|c| *c.borrow_mut() = Some(db));

        let mut state = GameState::new_two_player(11);
        state.debug_mode = true;
        REPLAY_LOG.with(|cell| {
            cell.set(Some(ReplayLog::new(ReplayHeader {
                format_config: state.format_config.clone(),
                match_config: state.match_config,
                player_count: state.players.len() as u8,
                first_player: Some(0),
                seed: state.rng_seed,
                deck_data: None,
            })))
        });
        GAME_STATE.with(|cell| cell.set(Some(state)));
        assert!(has_replay_recording());

        let result = handle_debug_create_card_inner(
            "Test Card",
            PlayerId(0),
            engine::types::zones::Zone::Hand,
            None,
            true,
        );
        assert!(
            result.is_ok(),
            "debug create-card should succeed in this fixture: {result:?}"
        );

        assert!(
            !has_replay_recording(),
            "a debug-spawned card is never appended to REPLAY_LOG (the WASM \
             bridge resolves it against CARD_DB before reaching `apply`), so \
             any in-progress recording must be invalidated rather than left \
             to silently omit the mutation"
        );

        clear_game_state();
        CARD_DB.with(|c| *c.borrow_mut() = None);
    }

    /// A non-`CreateCard` debug action (e.g. `DrawCards`) reaches
    /// `record_replay_action` through the normal `submit_action` path — it
    /// is not intercepted earlier the way `CreateCard` is. `reconstruct_initial_state`
    /// never enables `debug_mode`, so a recorded debug action would fail the
    /// `!state.debug_mode` gate in `apply` on playback and desync the replay.
    /// Recording must be invalidated instead, mirroring the CreateCard case.
    #[test]
    fn non_create_card_debug_action_invalidates_the_in_progress_recording() {
        clear_game_state();

        let state = GameState::new_two_player(13);
        REPLAY_LOG.with(|cell| {
            cell.set(Some(ReplayLog::new(ReplayHeader {
                format_config: state.format_config.clone(),
                match_config: state.match_config,
                player_count: state.players.len() as u8,
                first_player: Some(0),
                seed: state.rng_seed,
                deck_data: None,
            })))
        });
        assert!(has_replay_recording());

        let debug_action = GameAction::Debug(engine::types::actions::DebugAction::DrawCards {
            player_id: PlayerId(0),
            count: 1,
        });
        record_replay_action(true, PlayerId(0), debug_action);

        assert!(
            !has_replay_recording(),
            "a non-CreateCard debug action must invalidate any in-progress \
             recording too — replay reconstruction never enables debug_mode, \
             so recording it would produce a replay that desyncs on playback"
        );

        clear_game_state();
    }
}

/// Native coverage for the RNG-restore bridge wiring (issue #5466). The
/// `export`/`restore` entry points are plain Rust functions, so these run in
/// the standard `cargo test`/`nextest` shards — unlike the `wasm32`-gated
/// `mod tests`, whose assertions never execute in the native suite.
#[cfg(test)]
mod rng_restore_bridge_tests {
    use super::*;
    use rand::RngCore;

    #[test]
    fn export_then_restore_resumes_live_rng_stream_through_wasm_bridge() {
        // Issue #5466, end-to-end through the WASM boundary: `export_game_state_json`
        // must capture the live ChaCha20 offset and `restore_game_state` must
        // fast-forward the reseeded stream to it, so a restored game draws the
        // values that would have come NEXT — not a replay from origin. This test
        // drives the real bridge entry points (nothing calls the engine seam
        // directly): deleting `state.capture_rng_word_pos()` in export or
        // `state.rehydrate_rng()` in restore turns it red. Asserts on consumed
        // randomness, not the stored `rng_word_pos` integer.
        clear_game_state();

        // Seed a live game and consume randomness as gameplay would.
        let mut state = GameState::new_two_player(0x51A7_C0DE);
        for _ in 0..9 {
            state.rng.next_u32();
        }
        GAME_STATE.with(|cell| cell.set(Some(state)));

        // The four values the live stream will produce next, captured from a
        // clone taken at the pre-export offset.
        let mut expected = with_state(|s| s.rng.clone()).unwrap();
        let expected_draws: Vec<u32> = (0..4).map(|_| expected.next_u32()).collect();

        // Serialize through the real bridge entry point (captures rng_word_pos).
        let json = export_game_state_json().unwrap();

        // Advance the LIVE rng further so a rewind-to-origin restore would be
        // observable: without the offset, restore would replay from the seed's
        // origin (nine draws back), never matching `expected_draws`.
        with_state_mut(|s| {
            for _ in 0..3 {
                s.rng.next_u32();
            }
        })
        .unwrap();

        // Restore through the real bridge entry point (reseeds + fast-forwards).
        restore_game_state(&json).unwrap();

        // The restored stream must resume at the exported offset and produce the
        // continuation captured before export.
        let restored_draws: Vec<u32> =
            with_state_mut(|s| (0..4).map(|_| s.rng.next_u32()).collect()).unwrap();
        assert_eq!(
            restored_draws, expected_draws,
            "restored stream must resume where export left off, not rewind to origin"
        );

        clear_game_state();
    }
}
