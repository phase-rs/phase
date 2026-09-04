use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use engine::ai_support::{
    auto_pass_recommended, auto_pass_recommended_for_viewer, end_continuous_effect_offers,
    legal_actions_for_viewer, legal_actions_full, AiDecisionContract,
};
use engine::database::legality::{any_ai_difficulty_is_cedh, validate_cedh_bracket};
use engine::database::{CardDatabase, CardSearchQuery};
#[cfg(test)]
use engine::game::engine::apply;
use engine::game::engine::{
    apply_interaction_with_rejection, apply_with_rejection, preflight_debug_action_with_rejection,
    resume_restored_stack_automation, RestoredStackAutomationOutcome,
    RestoredStackAutomationPresentation,
};
use engine::game::interaction::{bind_interaction_authority, submit_interaction_with_rejection};
use engine::game::preview::{
    preview_action_with_rejection, preview_auto_payment_sources_with_rejection,
};
// Deep-path import by design: `engine::game::mod` re-exports `deck_validation`'s
// public surface, but this phase must not edit that file.
use engine::game::deck_validation::{draft_set_concessions_for, evaluate_deck_format_gate};
use engine::game::CardDbRehydrationFinalization;
use engine::game::{
    can_pair_commanders, companion_candidates, deck_copy_limit_for, estimate_bracket,
    evaluate_deck_compatibility, filter_state_for_viewer, is_brawl_commander_eligible,
    is_commander_eligible, is_tiny_leader_eligible, load_and_hydrate_decks, max_deck_copies,
    rehydrate_game_from_card_db_with_finalization, resolve_deck_list,
    signature_spell_selection_policy, start_game, start_game_with_starting_player,
    validate_name_deck_for_format_full, BracketEstimate, DeckCompatibilityRequest, DeckList,
    PlayerDeckList, ReplayPlayer,
};
use engine::types::actions::DebugAction;
use engine::types::custom_format::{CustomFormatDef, CustomFormatRules};
use engine::types::format::{DeckCopyLimit, FormatConfig, GameFormat};
use engine::types::game_state::{
    PersistedGameState, PersistedRestoreFinalization, PreparedPersistedGameState,
    TrustedGameStateEnvelope, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{InteractionSessionId, InteractionSubmission};
use engine::types::mana::ManaCost;
use engine::types::match_config::{MatchConfig, MatchType};
use engine::types::{
    ActionRejection, ActionRejectionCode, GameAction, GameState, PlayerId, ReplayHeader, ReplayLog,
};

use engine::game::resolve_player_deck_list;
use engine::starter_decks;
use phase_ai::choose_action_with_session_diagnostic;
use phase_ai::deck_profile::{ArchetypeClassification, DeckArchetype, DeckProfile};
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx, SeatMutation, SeatState};

/// Enrich local diagnostic receipts with names already known to the engine.
/// This remains at the WASM boundary: AI ranking stays state-agnostic, while
/// the display receives the exact card/permanent an action refers to.
fn attach_receipt_object_names(
    state: &GameState,
    receipt: &mut phase_ai::decision_receipt::AiDecisionDiagnosticReceipt,
) {
    for candidate in &mut receipt.candidates {
        let object_id = match &candidate.action {
            GameAction::CastSpell { object_id, .. }
            | GameAction::PlayLand { object_id, .. }
            | GameAction::Foretell { object_id, .. } => Some(*object_id),
            GameAction::ActivateAbility { source_id, .. } => Some(*source_id),
            _ => None,
        };
        candidate.object_name = object_id
            .and_then(|id| state.objects.get(&id))
            .map(|object| object.name.clone());
        candidate.details = serde_json::to_value(&candidate.action)
            .ok()
            .and_then(|action| {
                action
                    .get("data")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
            })
            .map(|data| {
                data.into_iter()
                    .map(
                        |(label, value)| phase_ai::decision_receipt::AiDecisionDiagnosticField {
                            label: humanize_diagnostic_field(&label),
                            value: format_diagnostic_value(&value),
                        },
                    )
                    .collect()
            })
            .unwrap_or_default();
    }
}

fn humanize_diagnostic_field(field: &str) -> String {
    field
        .split('_')
        .map(|word| match word {
            "id" => "ID".to_string(),
            _ => {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_diagnostic_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "None".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(format_diagnostic_value)
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(label, value)| {
                format!(
                    "{}: {}",
                    humanize_diagnostic_field(label),
                    format_diagnostic_value(value)
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

#[derive(Debug)]
struct PreparedRestoredGameState {
    state: PreparedPersistedGameState,
    debug_permitted_was_serialized: bool,
}

#[derive(Debug)]
struct DecodedRestoredGameState {
    state: GameState,
    debug_permitted_was_serialized: bool,
}

fn prepare_restored_game_state(json_str: &str) -> Result<PreparedRestoredGameState, String> {
    let serialized = serde_json::from_str::<serde_json::Value>(json_str)
        .map_err(|error| format!("Failed to deserialize GameState: {error}"))?;
    let state = serialized
        .get("state")
        .and_then(serde_json::Value::as_object)
        .or_else(|| serialized.as_object());
    let debug_permitted_was_serialized =
        state.is_some_and(|state| state.contains_key("debug_permitted"));
    let state = serde_json::from_value::<PersistedGameState>(serialized)
        .map_err(|error| format!("Failed to deserialize GameState: {error}"))?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|error| format!("Failed to restore GameState: {error}"))?;
    Ok(PreparedRestoredGameState {
        state,
        debug_permitted_was_serialized,
    })
}

/// Native-only decode helper for restore-boundary tests that do not need card
/// database rehydration. Production callers use `prepare_restored_game_state`
/// and finalize only after their card database is present.
#[cfg(test)]
fn decode_restored_game_state(json_str: &str) -> Result<DecodedRestoredGameState, String> {
    let restored = prepare_restored_game_state(json_str)?;
    let state = restored
        .state
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|error| format!("Failed to restore GameState: {error}"))?;
    Ok(DecodedRestoredGameState {
        state,
        debug_permitted_was_serialized: restored.debug_permitted_was_serialized,
    })
}

fn validate_external_format_config(config: &FormatConfig, player_count: u8) -> Result<(), String> {
    config.validate_for_player_count(player_count)?;
    config.reject_unimplemented_range_of_influence()
}

fn parse_initialize_format_config(
    decoded: Result<FormatConfig, String>,
) -> Result<FormatConfig, serde_json::Value> {
    decoded.map_err(|error| {
        serde_json::json!({
            "error": true,
            "reasons": [format!("Format config deserialization failed: {error}")],
        })
    })
}

#[cfg(test)]
mod external_format_config_tests {
    use std::collections::BTreeMap;

    use super::*;
    use engine::types::format::RangeOfInfluenceConfig;

    #[test]
    fn object_id_records_serialize_with_json_string_keys() {
        let record = object_id_record(HashMap::from([(ObjectId(42), "answer")]));

        assert_eq!(
            serde_json::to_value(record).expect("record serializes"),
            serde_json::json!({ "42": "answer" })
        );
    }

    #[test]
    fn external_initialization_rejects_limited_range_configuration() {
        let mut config = FormatConfig::standard();
        config.range_of_influence = Some(Box::new(RangeOfInfluenceConfig {
            default_range: 0,
            player_overrides: BTreeMap::new(),
        }));

        assert!(validate_external_format_config(&config, 2)
            .expect_err("limited range must remain disabled at the WASM boundary")
            .contains("not supported"));
    }

    #[test]
    fn malformed_initialize_format_config_returns_an_error_envelope() {
        let malformed_js_config = serde_json::json!(42);
        let decoded = serde_json::from_value::<FormatConfig>(malformed_js_config)
            .map_err(|error| error.to_string());

        let error = parse_initialize_format_config(decoded)
            .expect_err("malformed JS config must not fall back to Standard");

        assert_eq!(error["error"], true);
        assert!(error["reasons"][0]
            .as_str()
            .expect("error reason is a string")
            .contains("Format config deserialization failed"));
    }

    #[test]
    fn restored_state_with_limited_range_is_rejected_before_rehydration() {
        let mut state = GameState::new_two_player(42);
        state.format_config.range_of_influence = Some(Box::new(RangeOfInfluenceConfig {
            default_range: 0,
            player_overrides: BTreeMap::new(),
        }));
        let json = serde_json::to_string(&state).expect("state serializes");

        assert!(decode_restored_game_state(&json)
            .expect_err("limited range must remain disabled at the restore boundary")
            .contains("not supported"));
    }

    #[test]
    fn legacy_scalar_range_restore_reaches_the_feature_gate() {
        let mut serialized =
            serde_json::to_value(GameState::new_two_player(42)).expect("state serializes");
        serialized["format_config"]["range_of_influence"] = serde_json::json!(1);
        let json = serde_json::to_string(&serialized).expect("legacy state serializes");

        let error = decode_restored_game_state(&json)
            .expect_err("legacy enabled range must be rejected after migration");

        assert!(error.contains("not supported"));
        assert!(!error.contains("deserialize"));
    }
}

/// Bind the engine's interaction authority for the one game this module hosts.
///
/// Both `GameState::new` and the persisted decode leave `interaction_session_id`
/// as `None`, and while it is unset `derive_viewer_interaction` reports
/// `AuthorityUnbound` and returns no opportunities at all — so every interaction
/// surface goes dark. `ensure_interaction_authority` cannot repair this: it only
/// *maintains* an already-bound session, and clears the slots when there is none.
///
/// Always a fresh random id, never the one carried in a restored blob. The id is
/// the namespace of every minted `InteractionId` (`"{session}.{generation}.{serial}"`),
/// and re-binding the *same* session deliberately preserves the counters — so
/// reusing a snapshot's id after an undo would re-issue ids already handed out on
/// the abandoned branch. A new namespace makes that collision impossible, and
/// matches server-core's rule that a persisted blob must not drive live authority.
///
/// Failure needs no log here (unlike server-core, which has `tracing`): the only
/// way to get one is decimal-serial exhaustion, so this uses the same
/// `debug_assert` discipline as `ensure_interaction_authority` itself rather than
/// pulling `web_sys` into a size-optimized WASM artifact for an unreachable arm.
fn bind_interaction_session(state: &mut GameState) {
    let session = InteractionSessionId(format!("wasm-{:016x}", rand::rng().random::<u64>()));
    let bound = bind_interaction_authority(state, session);
    debug_assert!(
        bound.is_ok(),
        "interaction authority bind failed: {bound:?}"
    );
}

/// Result of `get_legal_actions_js` — bundles actions with the engine's auto-pass
/// recommendation so frontends don't need to classify action meaningfulness.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegalActionsResult {
    actions: Vec<GameAction>,
    auto_pass_recommended: bool,
    /// Ordered CR 116.2c offers already projected by the engine for display.
    end_continuous_effect_offers: Vec<GameAction>,
    /// Exact engine-authored actions for the deterministic mana-payment shortcut.
    mana_payment_shortcut_actions: Vec<GameAction>,
    /// Effective mana costs for castable spells, keyed by object_id.
    /// Reflects all cost modifiers (reductions, commander tax, alt costs).
    spell_costs: BTreeMap<String, ManaCost>,
    /// Engine-grouped subset of `actions` keyed by `GameAction::source_object()`.
    /// Frontend uses this for "what can I do with this card?" lookups so it
    /// doesn't have to introspect `GameAction` variants client-side.
    legal_actions_by_object: BTreeMap<String, Vec<engine::game::interaction::ObjectActionPayload>>,
    /// Engine-level progress-wedge diagnostic: non-fatal signal that an owed
    /// decision has no legal action for any authorized submitter (an engine
    /// anomaly, not a rules outcome). `None` normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    stuck_diagnostic: Option<engine::ai_support::StuckDecisionDiagnostic>,
    viewer_interaction: engine::types::interaction::ViewerInteraction,
}

/// Convert engine object IDs into the string-keyed records JSON requires at the
/// WASM boundary. The frontend already consumes these fields as `Record<string, _>`.
fn object_id_record<V>(values: HashMap<ObjectId, V>) -> BTreeMap<String, V> {
    values
        .into_iter()
        .map(|(object_id, value)| (object_id.0.to_string(), value))
        .collect()
}

/// Serialize a Rust value to a JS object via JSON.
///
/// Uses `serde_json` as the intermediary format, then `JSON.parse` on the JS side.
/// Callers must project numeric-keyed maps into string-keyed records before
/// crossing this boundary; JSON objects cannot encode numeric map keys.
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
    choose_action_with_session, score_candidates_for_parallel_worker,
    select_safe_action_from_scores, AiSession, SessionCache,
};
thread_local! {
    /// Game state uses Cell<Option<T>> with take/set to avoid RefCell borrow poisoning.
    /// In WASM, panics don't unwind (no RAII cleanup), so a RefCell::borrow_mut() that
    /// panics leaves the borrow flag permanently set — every subsequent call fails.
    /// Cell::take() + Cell::set() has no borrow guard, making it panic-resilient.
    static GAME_STATE: Cell<Option<GameState>> = const { Cell::new(None) };
    static CARD_DB: RefCell<Option<CardDatabase>> = const { RefCell::new(None) };
    /// When set, this engine is claimed by a multiplayer host session. The
    /// engine claims it itself, in the same call that installs the game
    /// (`initialize_multiplayer_host_game`, `resume_multiplayer_host_state`),
    /// so there is never a window in which the flag and the game it describes
    /// disagree. Undo-style state rollback is refused while it is set because
    /// rewinding a single client's view would desync from the authoritative
    /// game on the wire. See `restore_game_state`.
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
    /// Opaque AI proposals are capabilities issued by this live WASM authority.
    /// They deliberately do not serialize with `GameState`: a restore/new game
    /// starts a new generation even when the state revision happens to match.
    static AI_PROPOSALS: RefCell<AiProposalRegistry> = RefCell::new(AiProposalRegistry::default());
}

#[derive(Debug, Clone)]
struct StoredAiProposal {
    generation: u64,
    contract: AiDecisionContract,
}

#[derive(Default)]
struct AiProposalRegistry {
    generation: u64,
    serial: u64,
    proposals: HashMap<String, StoredAiProposal>,
}

impl AiProposalRegistry {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.proposals.clear();
    }

    fn insert(&mut self, contract: AiDecisionContract) -> String {
        self.serial = self.serial.wrapping_add(1);
        // A newer proposal supersedes this pending player's earlier token, but
        // concurrent decisions (such as simultaneous mulligans) each retain
        // their own bounded capability. There can be at most one live token
        // per semantic owner in this authority generation.
        self.proposals
            .retain(|_, proposal| proposal.contract.semantic_owner != contract.semantic_owner);
        let token = format!(
            "ai-{}-{}-{:016x}",
            self.generation,
            self.serial,
            rand::rng().random::<u64>()
        );
        self.proposals.insert(
            token.clone(),
            StoredAiProposal {
                generation: self.generation,
                contract,
            },
        );
        token
    }

    fn proposal(&self, token: &str) -> Option<&StoredAiProposal> {
        self.proposals
            .get(token)
            .filter(|proposal| proposal.generation == self.generation)
    }
}

fn invalidate_ai_proposals() {
    AI_PROPOSALS.with(|registry| registry.borrow_mut().invalidate());
}

#[cfg(test)]
mod ai_proposal_registry_tests {
    use super::*;

    fn contract() -> AiDecisionContract {
        AiDecisionContract {
            semantic_owner: PlayerId(0),
            authorized_actor: PlayerId(0),
            state_revision: 7,
            candidates: Vec::new(),
        }
    }

    #[test]
    fn invalidation_revokes_every_token_even_when_a_restored_state_reuses_its_revision() {
        let mut registry = AiProposalRegistry::default();
        let token = registry.insert(contract());
        assert!(registry.proposal(&token).is_some());

        // Restore/new-game boundaries advance the live authority generation;
        // the serialized GameState revision is deliberately irrelevant here.
        registry.invalidate();
        assert!(registry.proposal(&token).is_none());
    }

    #[test]
    fn token_is_an_opaque_capability_not_a_reusable_contract_key() {
        let mut registry = AiProposalRegistry::default();
        let first = registry.insert(contract());
        let second = registry.insert(contract());

        assert_ne!(first, second);
        assert!(registry.proposal(&first).is_none());
        assert!(registry.proposal(&second).is_some());
        assert_eq!(registry.proposals.len(), 1);
        assert!(registry.proposal("forged-token").is_none());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
enum AiProposalSubmission {
    Applied {
        result: Box<engine::types::game_state::ActionResult>,
    },
    Stale {
        reason: &'static str,
    },
    Rejected {
        rejection: ActionRejection,
    },
}

/// Private WASM boundary outcome for expected engine rejections. Serialization
/// keeps recoverable action failures distinct from raw WASM/runtime errors,
/// which continue to cross this boundary as strings or thrown `JsValue`s.
#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
enum ActionOutcome<T> {
    Applied { result: T },
    Rejected { rejection: ActionRejection },
}

fn action_outcome<T: Serialize>(result: Result<T, ActionRejection>) -> JsValue {
    to_js(&match result {
        Ok(result) => ActionOutcome::Applied { result },
        Err(rejection) => ActionOutcome::Rejected { rejection },
    })
}

fn rejected_action_outcome(rejection: ActionRejection) -> JsValue {
    to_js(&ActionOutcome::<()>::Rejected { rejection })
}

/// Set the multiplayer enforcement flag directly.
///
/// Entering multiplayer is *not* done here: the engine claims the flag itself,
/// in the same call that installs the game (`initialize_multiplayer_host_game`,
/// `resume_multiplayer_host_state`), so no client can leave the flag and the
/// game it describes out of step. This entry point serves the release side —
/// `releaseHostSession` clears the flag when a host session ends, so the next
/// local game on a shared worker may undo again.
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

/// Resolve the seat whose live prompt owns an AI decision. The requested seat
/// is retained for simultaneous prompts where it is still entitled to act.
fn ai_semantic_owner(state: &GameState, requested_ai: PlayerId) -> PlayerId {
    if state.waiting_for.acting_players().contains(&requested_ai) {
        requested_ai
    } else {
        state
            .waiting_for
            .acting_player()
            .or_else(|| state.waiting_for.acting_players().first().copied())
            .unwrap_or(requested_ai)
    }
}

/// Mint an opaque proposal only after the engine's current decision contract
/// accepts the selected action.
fn mint_ai_action_proposal(
    state: &GameState,
    semantic_owner: PlayerId,
    contract: AiDecisionContract,
    action: GameAction,
) -> JsValue {
    if !contract.contains_action(state, &action) {
        return JsValue::NULL;
    }
    let actor = contract.authorized_actor;
    let token = AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract));
    to_js(&serde_json::json!({
        "token": token,
        "semanticOwner": semantic_owner.0,
        "actor": actor.0,
        "action": action,
    }))
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
    invalidate_ai_proposals();
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

/// Build the bounded card corpus for parallel AI scoring workers. The live
/// main engine remains the only authority that owns the full card database.
#[wasm_bindgen]
pub fn build_ai_card_subset() -> Result<String, JsValue> {
    let result = CARD_DB.with(|db_cell| {
        let db = db_cell.borrow();
        GAME_STATE.with(|state_cell| {
            let state = state_cell.take();
            let result = engine::game::card_subset::build_ai_card_subset_or_full(
                state.as_ref(),
                db.as_ref(),
            );
            state_cell.set(state);
            result
        })
    });
    serde_json::to_string(&result).map_err(|error| JsValue::from_str(&error.to_string()))
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

/// CR 100.2a / CR 903.5b: How many copies of the named card a deck built under
/// `format_config` may legally contain across main deck, sideboard, and command
/// zone combined (CR 100.4a). Unlike `deckCopyLimit`, this is the *resolved*
/// ceiling — it already applies the basic-land exemption, the card's printed
/// override, and the format default, so the caller compares a count against it
/// directly.
///
/// `format_config` is a full `FormatConfig` JSON object (as published by
/// `getFormatRegistry`'s `default_config`), not a bare `GameFormat` string: only
/// the config carries the resolved `default_deck_copy_limit` a custom format
/// declares.
///
/// Serialized as the `DeckCopyLimit` tagged union (`{"type":"Unlimited"}` or
/// `{"type":"UpTo","data":N}`); switch on `.type`. Returns `{"type":"Unlimited"}`
/// when the card database isn't loaded, so a not-yet-hydrated frontend never
/// blocks a legal add.
#[wasm_bindgen(js_name = maxDeckCopies)]
pub fn max_deck_copies_for_format(name: &str, format_config: JsValue) -> JsValue {
    let Ok(format_config) = serde_wasm_bindgen::from_value::<FormatConfig>(format_config) else {
        return to_js(&DeckCopyLimit::Unlimited);
    };
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return to_js(&DeckCopyLimit::Unlimited);
        };
        to_js(&max_deck_copies(db, name, &format_config))
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
        // EXHAUSTIVE, deliberately: the `_ => false` this replaced is what made
        // `GameFormat::CommanderDraft` silently answer "no card can be your
        // commander" the moment the format became selectable. A wildcard here
        // re-arms that for the next format, so every arm is named.
        match format {
            GameFormat::Commander | GameFormat::DuelCommander => is_commander_eligible(face),
            GameFormat::PauperCommander => is_commander_eligible(face),
            // CR 903.3, unchanged by CR 903.13f: the CR 903.13f(3) grant
            // affects PAIRING, not eligibility, so Commander Draft uses
            // Commander's own predicate.
            GameFormat::CommanderDraft => is_commander_eligible(face),
            GameFormat::TinyLeaders => is_tiny_leader_eligible(face),
            GameFormat::Oathbreaker => face.is_oathbreaker,
            GameFormat::Brawl | GameFormat::HistoricBrawl => is_brawl_commander_eligible(face),
            // Formats with no command zone: nothing can be designated.
            GameFormat::Standard
            | GameFormat::Pioneer
            | GameFormat::Modern
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper
            | GameFormat::Momir
            | GameFormat::Planechase
            | GameFormat::Archenemy
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Limited => false,
            // Matches `evaluate_selected_format_summary`'s Custom arm: no
            // CustomFormatRules resolver exists yet, so eligibility cannot be
            // answered here. `false` is the fail-closed reading — a permissive
            // `true` would offer an unvalidated card as a commander.
            GameFormat::Custom(_) => false,
        }
    })
}

/// CR 702.124: Of `candidates`, which can legally pair with `first_commander`
/// as a co-commander? Applies the full partner family (generic Partner, Partner
/// with [Name], Friends Forever, Character Select, Doctor's Companion, Choose a
/// Background) via the engine's single-authority `can_pair_commanders`. The
/// frontend must not re-derive partner-pairing rules — it filters its candidate
/// list through this. Returns an empty array if the database isn't loaded.
///
/// `draft_set_codes` is every set whose draft boosters this deck's draft
/// CONTAINED, as an array — or `null`/`undefined`, which is read as the empty
/// array, i.e. constructed play. CR 903.13f(3)
/// conditions its partner grant on what the DRAFT contained, which is a session
/// property no pair of card names can express — so the caller supplies the set
/// codes and the ENGINE maps them to a grant. The client never learns which
/// sets grant what.
///
/// A LIST rather than one code, because CR 903.13f(3) asks about containment: a
/// mixed draft that opened Commander Masters and other boosters contained
/// Commander Masters, and the grant is in force. The engine takes the union.
///
/// It is a REQUIRED third parameter, and `JsValue` rather than
/// `Vec<String>`, on purpose: that matches this file's existing convention
/// for engine-typed arguments and makes a stale caller a compile error rather
/// than a silent `undefined`.
#[wasm_bindgen(js_name = commanderPartnerCandidates)]
pub fn commander_partner_candidates(
    first_commander: String,
    candidates: JsValue,
    draft_set_codes: JsValue,
) -> Result<JsValue, JsValue> {
    let candidates: Vec<String> = serde_wasm_bindgen::from_value(candidates)
        .map_err(|e| JsValue::from_str(&format!("Invalid candidate list: {e}")))?;
    // `Option`, not a bare `Vec`, so a caller with no draft behind the deck can
    // say so as `null` rather than having to construct an empty array — and so
    // a JS `undefined` degrades to constructed play instead of throwing. This
    // boundary is an in-process call with one typed TS caller, so it does not
    // need `deck_loading::deserialize_draft_set_codes`' legacy single-string
    // arm: no stored or wire payload reaches it.
    let draft_set_codes: Option<Vec<String>> = serde_wasm_bindgen::from_value(draft_set_codes)
        .map_err(|e| JsValue::from_str(&format!("Invalid draft set codes: {e}")))?;
    let draft_set_codes = draft_set_codes.unwrap_or_default();
    // CR 903.13f(3): an empty list means constructed play, which grants nothing.
    let grant = draft_set_concessions_for(draft_set_codes.iter().map(String::as_str)).partner_grant;
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Ok(to_js(&Vec::<String>::new()));
        };
        let eligible: Vec<String> = candidates
            .into_iter()
            .filter(|name| can_pair_commanders(db, &first_commander, name, grant))
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

/// CR 100.4a: Returns the sideboard policy stored on a `FormatConfig` as a
/// tagged union: `{"type": "Forbidden"}`, `{"type": "Limited", "data": 15}`,
/// or `{"type": "Unlimited"}`.
///
/// `format_config` is a full `FormatConfig` JSON object (as published by
/// `getFormatRegistry`'s `default_config`), not a bare `GameFormat` string: only
/// the config carries the resolved policy a custom format declares.
///
/// The frontend must exhaustive-switch on `.type` — unit variants (`Forbidden`,
/// `Unlimited`) emit no `data` field under `#[serde(tag, content)]`.
///
/// The engine is the single authority for format sideboard rules; the frontend
/// never hardcodes 15 or any other cap.
#[wasm_bindgen(js_name = sideboardPolicyForFormat)]
pub fn sideboard_policy_for_format(format_config: JsValue) -> Result<JsValue, JsValue> {
    let format_config: FormatConfig = serde_wasm_bindgen::from_value(format_config)
        .map_err(|e| JsValue::from_str(&format!("Invalid FormatConfig: {e}")))?;
    Ok(to_js(&format_config.sideboard_policy))
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

/// Always-definite deck/format gate for callers that ENFORCE rather than hint.
///
/// Returns `{ compatible: boolean, reasons: string[] }` — never a tri-state.
/// Backed by `evaluate_deck_format_gate`, a thin wrapper over the same
/// authoritative `validate_deck_for_format` the real game-creation boundary
/// runs, so a host's admission decision cannot disagree with the engine's own.
///
/// Its one intended caller is the P2P host's per-guest deck check
/// (`validateGuestDeck` in `client/src/adapter/p2p-adapter.ts`), which kicks a
/// guest whose deck is illegal for the room's format. UI-hint callers must keep
/// using `evaluate_deck_compatibility_js`: that one deliberately answers "no
/// opinion" (`selected_format_compatible: null`) for a Custom format, which is
/// the honest answer for a legality chip and an unacceptable one for a kick.
#[wasm_bindgen(js_name = evaluateDeckFormatGate)]
pub fn evaluate_deck_format_gate_js(request: JsValue) -> Result<JsValue, JsValue> {
    let request: DeckCompatibilityRequest = serde_wasm_bindgen::from_value(request)
        .map_err(|e| JsValue::from_str(&format!("Invalid compatibility request: {e}")))?;

    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err(JsValue::from_str(
                "Card database not loaded. Call load_card_database first.",
            ));
        };
        Ok(to_js(&evaluate_deck_format_gate(db, &request)))
    })
}

/// Axis A: capture a lobby's live, fully-resolved `FormatConfig` as a saved
/// custom-format DEFINITION (`CustomFormatDef`), which the client persists
/// locally. Never produces an active config — `formatConfigForCustomRules`
/// below is the reverse direction, applied when a player later selects a saved
/// definition.
///
/// Fallible, and the engine's own rejection message is surfaced verbatim: a
/// format whose `deck_loading.rs` behavior grants an auxiliary deck or
/// component keyed on the literal format (Planechase's shared planar deck,
/// Archenemy's scheme deck, Momir's game-start emblem) has no representation in
/// `StructuralRules` and would be silently lost, as would an already-`Custom`
/// source's own legality rules. An empty name is rejected too. The frontend
/// must not re-derive any of these conditions — it displays what the engine
/// says.
#[wasm_bindgen(js_name = customFormatFromLobbyConfig)]
pub fn custom_format_from_lobby_config(
    name: String,
    format_config: JsValue,
) -> Result<JsValue, JsValue> {
    let format_config: FormatConfig = serde_wasm_bindgen::from_value(format_config)
        .map_err(|e| JsValue::from_str(&format!("Invalid FormatConfig: {e}")))?;
    let def = CustomFormatDef::from_lobby_config(name, &format_config)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(to_js(&def))
}

/// The single authoritative `CustomFormatRules -> FormatConfig` resolver,
/// exposed for the lobby's "select a saved custom format" action. Total and
/// infallible: a `CustomFormatRules` carries every structural field the config
/// needs, so there is no unresolvable input.
///
/// The frontend must call this rather than assembling a `FormatConfig` from the
/// saved rules itself. `FormatConfig`'s own `Deserialize` re-derives the config
/// with this exact function and demands equality, so any hand-built config
/// would be rejected at the next boundary it crossed.
#[wasm_bindgen(js_name = formatConfigForCustomRules)]
pub fn format_config_for_custom_rules(custom_rules: JsValue) -> Result<JsValue, JsValue> {
    let rules: CustomFormatRules = serde_wasm_bindgen::from_value(custom_rules)
        .map_err(|e| JsValue::from_str(&format!("Invalid CustomFormatRules: {e}")))?;
    Ok(to_js(&FormatConfig::for_custom_rules(&rules)))
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

/// Which client-side session is installing this game. Selects the
/// debug-permission posture and whether the multiplayer flag is claimed in the
/// same call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitSessionKind {
    Local,
    MultiplayerHost,
}

/// Is a game installed in this engine right now?
///
/// `GAME_STATE` is a `Cell<Option<GameState>>` for panic-resilience (see the
/// thread-local's own doc) and `GameState` is not `Copy`, so take-peek-set is
/// the only way to read it.
fn game_state_present() -> bool {
    GAME_STATE.with(|cell| {
        let state = cell.take();
        let present = state.is_some();
        cell.set(state);
        present
    })
}

/// May a session of `kind` install a game into this engine right now?
///
/// Pure over the two thread-locals and free of `JsValue`, so it runs in the
/// native test suite.
fn init_guard(kind: InitSessionKind) -> Result<(), &'static str> {
    match kind {
        // On a memory-constrained device the P2P host shares the tab's single
        // engine worker with local play, so an unguarded local initialize would
        // silently destroy the hosted game. Mirrors `restore_game_state`'s
        // refusal on the same flag.
        InitSessionKind::Local if is_multiplayer_mode() => {
            Err("a multiplayer host session owns this engine")
        }
        // The other direction: refuse rather than overwrite a resident local
        // game.
        InitSessionKind::MultiplayerHost if game_state_present() => {
            Err("engine already holds a game")
        }
        // A local game may always replace another local game — that is how a
        // rematch starts, and nothing clears `GAME_STATE` in between.
        _ => Ok(()),
    }
}

/// Claim the engine for `kind`. Called immediately after the state install so
/// the flag and the game it describes are set in one uninterruptible step.
fn claim_engine_for(kind: InitSessionKind) {
    if kind == InitSessionKind::MultiplayerHost {
        MULTIPLAYER_MODE.with(|cell| cell.set(true));
    }
}

/// Envelope for an `init_guard` refusal. Carries the typed `engine_occupied`
/// discriminator — like `cedh_bracket_violation` below — so the adapter raises
/// a dedicated error instead of matching on a raw string substring.
fn occupied_refusal(reason: &str) -> JsValue {
    to_js(&serde_json::json!({
        "error": true,
        "engine_occupied": true,
        "reasons": [reason],
    }))
}

/// Initialize a new game for local (single-player / AI) play.
/// Accepts deck_data as a DeckList (name-only) or null/undefined for empty libraries.
/// format_config_js: optional FormatConfig JSON — defaults to Standard if null/undefined.
/// match_config_js: optional MatchConfig JSON — defaults to BO1 if null/undefined.
/// player_count: number of players — defaults to 2 if not provided.
/// first_player: 0 = human plays first (CR 103.1), 1 = opponent plays first, None = random.
/// Names are resolved against the card database loaded via load_card_database().
/// Returns the initial ActionResult (events + waiting_for).
///
/// Refuses with an `engine_occupied` envelope when a multiplayer host session
/// holds this engine — on a memory-constrained device that host shares this
/// very worker, and overwriting its game would destroy the authoritative state
/// its guests are playing against.
#[wasm_bindgen]
pub fn initialize_game(
    deck_data: JsValue,
    seed: Option<f64>,
    format_config_js: JsValue,
    match_config_js: JsValue,
    player_count: Option<u8>,
    first_player: Option<u8>,
) -> JsValue {
    if let Err(reason) = init_guard(InitSessionKind::Local) {
        return occupied_refusal(reason);
    }
    initialize_game_impl(
        deck_data,
        seed,
        format_config_js,
        match_config_js,
        player_count,
        first_player,
        InitSessionKind::Local,
    )
}

/// Initialize a new game *and* claim this engine for a multiplayer host
/// session, in one call.
///
/// Same parameters and same return envelope as `initialize_game`. The P2P host
/// uses this instead, for two reasons that only a single call can satisfy:
///
/// 1. **Refuses an occupied engine.** A hosted game must never start on top of
///    a live local game. A client-side probe followed by an install is two
///    round-trips with a window between them; this guard runs inside the same
///    synchronous worker task as the install, so nothing can interleave.
/// 2. **Atomic multiplayer-flag claim.** The flag is set on the line after the
///    state install (see `claim_engine_for`), so there is no window where a
///    stray `restore_game_state` (undo) would be accepted, and no window where
///    a failed init leaves the flag set on an engine it never took. Mirrors
///    `resume_multiplayer_host_state`, the resume-side sibling of this call.
#[wasm_bindgen]
pub fn initialize_multiplayer_host_game(
    deck_data: JsValue,
    seed: Option<f64>,
    format_config_js: JsValue,
    match_config_js: JsValue,
    player_count: Option<u8>,
    first_player: Option<u8>,
) -> JsValue {
    if let Err(reason) = init_guard(InitSessionKind::MultiplayerHost) {
        return occupied_refusal(reason);
    }
    initialize_game_impl(
        deck_data,
        seed,
        format_config_js,
        match_config_js,
        player_count,
        first_player,
        InitSessionKind::MultiplayerHost,
    )
}

/// Validate every seat's deck in a `DeckList` against the selected format,
/// returning `Some(reasons)` at the first refusal and `None` when every seat
/// passes. Each reason is labelled with the seat it came from, because the
/// caller returns them in a JS error envelope.
///
/// Extracted out of `initialize_game_impl`'s `CARD_DB` closure so it can be
/// driven from a native `#[cfg(test)]` module — the shells around it take
/// `JsValue`s and return through `to_js`, which calls the real `JSON.parse`
/// binding and panics outside a wasm32 runtime (see the note at the
/// `ai_scoring_rng_bridge_tests` module). Pure extraction: no behaviour change.
fn validate_deck_list_seats(
    db: &CardDatabase,
    deck_list: &DeckList,
    format_config: &FormatConfig,
    match_type: Option<MatchType>,
    player_count: usize,
) -> Option<Vec<String>> {
    // Fixed-deck formats (Momir's Madness) supply the deck from the engine for
    // every seat, so the client submits empty decks — there is nothing
    // client-side to validate. `load_and_hydrate_decks` fills each seat's
    // library with the engine-owned fixed deck. Gate on the engine predicate,
    // never a format literal.
    if !format_config.format.supplies_fixed_deck() {
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
                &deck_list.draft_set_codes,
                format_config,
                match_type,
                player_count,
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
                &deck_list.draft_set_codes,
                format_config,
                match_type,
                player_count,
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
    None
}

/// Shared body of both initialize entry points. The guard lives in the shells
/// (they are where `JsValue` envelopes are produced); this function assumes it
/// has already passed and installs unconditionally.
fn initialize_game_impl(
    deck_data: JsValue,
    seed: Option<f64>,
    format_config_js: JsValue,
    match_config_js: JsValue,
    player_count: Option<u8>,
    first_player: Option<u8>,
    kind: InitSessionKind,
) -> JsValue {
    let seed = seed.map(|s| s as u64).unwrap_or(42);

    let format_config = if !format_config_js.is_null() && !format_config_js.is_undefined() {
        match parse_initialize_format_config(
            serde_wasm_bindgen::from_value::<FormatConfig>(format_config_js)
                .map_err(|error| error.to_string()),
        ) {
            Ok(config) => config,
            Err(error) => return to_js(&error),
        }
    } else {
        FormatConfig::standard()
    };
    let count = player_count.unwrap_or(2);
    if let Err(reason) = validate_external_format_config(&format_config, count) {
        return to_js(&serde_json::json!({
            "error": true,
            "reasons": [reason],
        }));
    }

    let mut state = GameState::new(format_config.clone(), count, seed);
    // Read the posture from `kind`, not from `is_multiplayer_mode()`: the flag
    // is claimed *after* this install (see `claim_engine_for`), so the
    // thread-local is still clear here and a host game would otherwise be given
    // local debug permissions.
    initialize_debug_permissions(&mut state, kind == InitSessionKind::MultiplayerHost);
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

            if let Some(reasons) = validate_deck_list_seats(
                db,
                &deck_list,
                &format_config,
                Some(state.match_config.match_type),
                count as usize,
            ) {
                return Some(reasons);
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

    // After `start_game`, so the slots bound here match the pause the caller is
    // about to be handed — `bind_all_current_slots` binds for the *current*
    // `waiting_for`, and nothing re-derives it until the first action boundary.
    bind_interaction_session(&mut state);

    GAME_STATE.with(|cell| cell.set(Some(state)));
    // Adjacent to the install, exactly as `resume_multiplayer_host_state` does:
    // the flag and the game it describes are set in one uninterruptible step.
    claim_engine_for(kind);
    clear_ai_session_cache();
    invalidate_ai_proposals();

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
        Err(_) => {
            return rejected_action_outcome(ActionRejection::new(
                ActionRejectionCode::InvalidAction,
            ))
        }
    };
    let actor = PlayerId(actor);

    if let GameAction::Debug(debug_action) = &action {
        if debug_action.is_zero_count_create() {
            return match with_state(|state| {
                preflight_debug_action_with_rejection(state, actor, debug_action)?;
                Ok::<_, ActionRejection>(engine::types::game_state::ActionResult {
                    events: vec![],
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                })
            }) {
                Ok(result) => action_outcome(result),
                Err(error) => error,
            };
        }
    }

    if let GameAction::Debug(engine::types::actions::DebugAction::CreateCard {
        ref card_name,
        owner,
        zone,
        count,
        attach_to,
        run_etb,
        nonlegendary,
    }) = action
    {
        return handle_debug_create_card(DebugCreateCardRequest {
            actor,
            card_name,
            owner,
            zone,
            count,
            attach_to,
            run_etb,
            nonlegendary,
        });
    }

    // Cloned before `apply` consumes `action` — recorded into REPLAY_LOG only
    // on the success path below. CreateCard is handled above and never
    // reaches here.
    let action_for_replay = action.clone();
    let is_debug_action = matches!(action, GameAction::Debug(_));
    match with_state_mut(|state| match apply_with_rejection(state, actor, action) {
        Ok(result) => {
            record_replay_action(is_debug_action, actor, action_for_replay);
            invalidate_ai_proposals();
            action_outcome(Ok(result))
        }
        Err(rejection) => rejected_action_outcome(rejection),
    }) {
        Ok(val) => val,
        Err(e) => e,
    }
}

/// Submit one opaque, engine-authored interaction response. The browser never
/// materializes a `GameAction`; only a successful engine reducer result exposes
/// the exact action to the replay recorder.
#[wasm_bindgen]
pub fn submit_interaction_js(actor: u8, submission: JsValue) -> JsValue {
    let submission: InteractionSubmission = match serde_wasm_bindgen::from_value(submission) {
        Ok(submission) => submission,
        Err(_) => {
            return rejected_action_outcome(ActionRejection::new(
                ActionRejectionCode::InvalidInteractionResponse,
            ));
        }
    };
    let actor = PlayerId(actor);
    match with_state_mut(|state| submit_interaction_with_rejection(state, actor, submission)) {
        Ok(Ok(applied)) => {
            record_replay_action(false, actor, applied.action);
            invalidate_ai_proposals();
            action_outcome(Ok(applied.result))
        }
        Ok(Err(rejection)) => rejected_action_outcome(rejection),
        Err(error) => error,
    }
}

/// Record a successfully-applied action into REPLAY_LOG, or invalidate any
/// in-progress recording if it was a (non-CreateCard) debug action.
///
/// Every successful nonzero `GameAction::Debug` variant other than
/// `CreateCard` reaches this point (unlike CreateCard, they mutate state already tracked by
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

/// Record the AI-only verified-pass seam. This has a distinct typed replay
/// marker because replaying its visible `PassPriority` payload through the
/// ordinary reducer would omit the retained stack recheck session.
fn record_verified_ai_priority_pass(actor: PlayerId, semantic_owner: PlayerId) {
    REPLAY_LOG.with(|cell| {
        let mut log = cell.take();
        if let Some(log) = log.as_mut() {
            log.push_verified_ai_priority_pass(actor, semantic_owner);
        }
        cell.set(log);
    });
}

struct DebugCreateCardRequest<'a> {
    actor: PlayerId,
    card_name: &'a str,
    owner: PlayerId,
    zone: engine::types::zones::Zone,
    count: u32,
    attach_to: Option<engine::game::game_object::AttachTarget>,
    run_etb: bool,
    nonlegendary: bool,
}

fn handle_debug_create_card(request: DebugCreateCardRequest<'_>) -> JsValue {
    let debug_action = DebugAction::CreateCard {
        card_name: request.card_name.to_string(),
        owner: request.owner,
        zone: request.zone,
        count: request.count,
        attach_to: request.attach_to,
        run_etb: request.run_etb,
        nonlegendary: request.nonlegendary,
    };
    match with_state(|state| {
        preflight_debug_action_with_rejection(state, request.actor, &debug_action)
    }) {
        Ok(Err(rejection)) => return rejected_action_outcome(rejection),
        Ok(Ok(())) => {}
        Err(error) => return error,
    }
    match handle_debug_create_card_inner(request) {
        Ok(result) => action_outcome(Ok(result)),
        Err(msg) => JsValue::from_str(&msg),
    }
}

/// Mutation core of `handle_debug_create_card`, factored out so it can be
/// exercised by native unit tests — the `#[wasm_bindgen]`-facing wrapper's
/// success path calls `to_js`, which requires a JS runtime and panics under
/// plain `cargo test`. See `bracket_estimate_tests::estimate_bracket_inner`
/// for the same split.
fn handle_debug_create_card_inner(
    request: DebugCreateCardRequest<'_>,
) -> Result<engine::types::game_state::ActionResult, String> {
    let DebugCreateCardRequest {
        actor,
        card_name,
        owner,
        zone,
        count,
        attach_to,
        run_etb,
        nonlegendary,
    } = request;
    let debug_action = engine::types::actions::DebugAction::CreateCard {
        card_name: card_name.to_string(),
        owner,
        zone,
        count,
        attach_to,
        run_etb,
        nonlegendary,
    };
    let waiting_for = with_state(|state| {
        engine::game::preflight_debug_action(state, actor, &debug_action)
            .map_err(|error| format!("Engine error: {error}"))?;
        Ok(state.waiting_for.clone())
    })
    .unwrap_or_else(|_| Err(NOT_INITIALIZED_ERR.to_string()))?;
    if count == 0 {
        return Ok(engine::types::game_state::ActionResult {
            events: vec![],
            waiting_for,
            log_entries: vec![],
        });
    }
    let source = CARD_DB.with(|cell| {
        let db = cell.borrow();
        let Some(db) = db.as_ref() else {
            return Err("Engine error: card database not loaded".to_string());
        };
        match db.get_face_by_name(card_name) {
            Some(face) => Ok(engine::game::debug_card_entry_source(db, face)),
            None => Err("Engine error: card not found in database".to_string()),
        }
    })?;
    with_state_mut(|state| {
        let result = engine::game::create_debug_cards(
            state,
            engine::game::DebugCardCreateRequest {
                actor,
                source,
                owner,
                zone,
                count,
                attach_to,
                run_etb,
                nonlegendary,
            },
        )
        .map_err(|error| format!("Engine error: {error}"))?;
        // Debug-spawned cards are resolved against the WASM-local CARD_DB and
        // never recorded into REPLAY_LOG (unlike normal actions in
        // `submit_action`), so a faithful replay can't reconstruct this
        // mutation. Invalidate any in-progress recording here, the same way
        // `restore_game_state` invalidates on a history-breaking state swap,
        // so `export_replay_log` can't produce a log that silently omits a
        // debug spawn.
        REPLAY_LOG.with(|cell| cell.set(None));

        engine::game::public_state::bump_state_revision(state);
        engine::game::public_state::mark_public_state_all_dirty(state);
        engine::game::public_state::finalize_public_state(state);
        Ok(result)
    })
    .unwrap_or_else(|_| Err(NOT_INITIALIZED_ERR.to_string()))
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
/// Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<string, ManaCost> }`.
#[wasm_bindgen]
pub fn get_legal_actions_js() -> JsValue {
    match with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let (actions, spell_costs, legal_actions_by_object) = legal_actions_full(state);
        let auto_pass = auto_pass_recommended(state, &actions);
        let end_continuous_effect_offers = end_continuous_effect_offers(&actions);
        let mana_payment_shortcut_actions =
            engine::ai_support::mana_payment_shortcut_actions(state, &legal_actions_by_object);
        to_js(&LegalActionsResult {
            actions,
            auto_pass_recommended: auto_pass,
            end_continuous_effect_offers,
            mana_payment_shortcut_actions,
            spell_costs: object_id_record(spell_costs),
            legal_actions_by_object: object_id_record(
                engine::game::interaction::object_action_payloads(&legal_actions_by_object),
            ),
            stuck_diagnostic: engine::ai_support::stuck_decision_diagnostic(state),
            viewer_interaction: engine::game::interaction::derive_viewer_interaction(
                state,
                state,
                state.active_player,
            ),
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
struct ViewerSnapshot<'a> {
    state: engine::game::derived_views::ClientGameStateRef<'a>,
    actions: Vec<GameAction>,
    auto_pass_recommended: bool,
    end_continuous_effect_offers: Vec<GameAction>,
    mana_payment_shortcut_actions: Vec<GameAction>,
    spell_costs: BTreeMap<String, ManaCost>,
    legal_actions_by_object: BTreeMap<String, Vec<engine::game::interaction::ObjectActionPayload>>,
    /// Engine-level progress-wedge diagnostic: non-fatal signal that an owed
    /// decision has no legal action for any authorized submitter (an engine
    /// anomaly, not a rules outcome). `None` normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    stuck_diagnostic: Option<engine::ai_support::StuckDecisionDiagnostic>,
    viewer_interaction: engine::types::interaction::ViewerInteraction,
}

fn legal_actions_result_for_viewer(state: &GameState, viewer: PlayerId) -> LegalActionsResult {
    let (actions, spell_costs, legal_actions_by_object) = legal_actions_for_viewer(state, viewer);
    let auto_pass_recommended = auto_pass_recommended_for_viewer(state, viewer, &actions);
    let end_continuous_effect_offers = end_continuous_effect_offers(&actions);
    let mana_payment_shortcut_actions =
        engine::ai_support::mana_payment_shortcut_actions(state, &legal_actions_by_object);
    LegalActionsResult {
        actions,
        auto_pass_recommended,
        end_continuous_effect_offers,
        mana_payment_shortcut_actions,
        spell_costs: object_id_record(spell_costs),
        legal_actions_by_object: object_id_record(
            engine::game::interaction::object_action_payloads(&legal_actions_by_object),
        ),
        stuck_diagnostic: engine::ai_support::stuck_decision_diagnostic(state),
        viewer_interaction: engine::game::interaction::derive_viewer_interaction(
            state, state, viewer,
        ),
    }
}

#[cfg(test)]
mod viewer_priority_tests {
    use super::*;
    use engine::types::format::FormatConfig;
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

    #[test]
    fn local_debug_permission_is_explicit_but_non_sandbox_p2p_stays_empty() {
        let mut local = GameState::new(FormatConfig::standard(), 2, 42);
        initialize_debug_permissions(&mut local, false);
        assert!(local.debug_mode);
        assert!(local.debug_permitted.contains(&PlayerId(0)));
        assert!(!local.debug_permitted.contains(&PlayerId(1)));

        let mut p2p = GameState::new(FormatConfig::standard(), 2, 42);
        initialize_debug_permissions(&mut p2p, true);
        assert!(p2p.debug_mode);
        assert!(
            p2p.debug_permitted.is_empty(),
            "normal P2P must not receive the debug-library capability"
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
        let viewer_interaction =
            engine::game::interaction::derive_viewer_interaction(state, &filtered, viewer);
        to_js(&ViewerSnapshot {
            state: engine::game::derived_views::ClientGameStateRef::wrap_filtered(
                state,
                &filtered,
                Some(viewer),
            ),
            actions: legal.actions,
            auto_pass_recommended: legal.auto_pass_recommended,
            end_continuous_effect_offers: legal.end_continuous_effect_offers,
            mana_payment_shortcut_actions: legal.mana_payment_shortcut_actions,
            spell_costs: legal.spell_costs,
            legal_actions_by_object: legal.legal_actions_by_object,
            stuck_diagnostic: legal.stuck_diagnostic,
            viewer_interaction,
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
        Err(_) => {
            return rejected_action_outcome(ActionRejection::new(
                ActionRejectionCode::InvalidAction,
            ))
        }
    };
    let actor = PlayerId(actor);
    match with_state(|state| action_outcome(preview_action_with_rejection(state, actor, &action))) {
        Ok(outcome) => outcome,
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
        Err(_) => {
            return rejected_action_outcome(ActionRejection::new(
                ActionRejectionCode::InvalidAction,
            ))
        }
    };

    match with_state(|state| {
        preview_auto_payment_sources_with_rejection(state, PlayerId(actor), &action)
    }) {
        Ok(result) => action_outcome(result),
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

fn rehydrate_restored_state_from_card_db(state: &mut GameState) -> Result<(), String> {
    CARD_DB.with(|cell| {
        let db = cell.borrow();
        let db = db.as_ref().ok_or_else(|| {
            "Cannot restore game state: card database is not loaded. Call load_card_database first."
                .to_string()
        })?;
        rehydrate_game_from_card_db_with_finalization(
            state,
            db,
            CardDbRehydrationFinalization::Defer,
        );
        Ok(())
    })
}

fn decode_and_rehydrate_restored_game_state(
    json_str: &str,
    restore_runtime: impl FnOnce(&mut GameState),
) -> Result<DecodedRestoredGameState, String> {
    let restored = prepare_restored_game_state(json_str)?;
    let debug_permitted_was_serialized = restored.debug_permitted_was_serialized;
    let state = restored
        .state
        .finalize_after_rehydration(|state| {
            rehydrate_restored_state_from_card_db(state)?;
            // Combat declaration snapshots are display data derived from the rehydrated
            // live board. Rebuild them before this external state becomes interactive.
            engine::game::combat::refresh_combat_declaration_waiting_for(state);
            restore_runtime(state);
            Ok(())
        })
        .map_err(|error| format!("Failed to restore GameState: {error}"))?;
    let restored = DecodedRestoredGameState {
        state,
        debug_permitted_was_serialized,
    };
    Ok(restored)
}

/// Sets the explicit debug capability that client projections consume. Local
/// games authorize their perspective seat; multiplayer reserves debug access
/// for the sandbox permission set so ordinary P2P games cannot expose it.
fn initialize_debug_permissions(state: &mut GameState, multiplayer: bool) {
    state.debug_mode = true;
    if state.format_config.allow_debug_actions {
        state
            .debug_permitted
            .extend(state.players.iter().map(|player| player.id));
    } else if !multiplayer {
        state.debug_permitted.insert(PlayerId(0));
    }
}

/// Reconstructs the capability set omitted by saves created before
/// `debug_permitted` was persisted. Current saves carry the field even when
/// its intentionally empty, so their grant/revoke state remains authoritative.
fn backfill_legacy_debug_permissions(
    state: &mut GameState,
    debug_permitted_was_serialized: bool,
    multiplayer: bool,
) {
    if debug_permitted_was_serialized {
        return;
    }
    if state.format_config.allow_debug_actions {
        state
            .debug_permitted
            .extend(state.players.iter().map(|player| player.id));
    } else if !multiplayer {
        state.debug_permitted.insert(PlayerId(0));
    }
}

#[cfg(test)]
fn load_minimal_test_card_database() {
    CARD_DB.with(|cell| {
        *cell.borrow_mut() = Some(
            CardDatabase::from_json_str("{}")
                .expect("an empty test card database must deserialize"),
        );
    });
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
    restore_game_state_inner(json_str).map_err(|error| JsValue::from_str(&error))
}

/// The natively-callable body of [`restore_game_state`].
///
/// Split for the same reason — and in the same shape — as `scored_candidates_inner`:
/// the `#[wasm_bindgen]` shell may only run on
/// wasm32. Off-target, `JsValue::from_str` panics inside a function that cannot
/// unwind, so a shell that merely RETURNS an error aborts the whole process with
/// SIGABRT instead of failing the test. A native test that calls the shell is
/// therefore only safe while restore succeeds; the moment it errors, the failure
/// is unreadable. Tests call this function.
fn restore_game_state_inner(json_str: &str) -> Result<(), String> {
    if MULTIPLAYER_MODE.with(|cell| cell.get()) {
        return Err("restore_game_state refused: undo is disabled in multiplayer sessions".into());
    }
    let restored = decode_and_rehydrate_restored_game_state(json_str, GameState::rehydrate_rng)?;
    let mut state = restored.state;
    state.debug_mode = true;
    backfill_legacy_debug_permissions(&mut state, restored.debug_permitted_was_serialized, false);
    bind_interaction_session(&mut state);
    GAME_STATE.with(|cell| cell.set(Some(state)));
    // Restoring (undo, or resuming a save from a fresh worker that never saw
    // `initialize_game`) invalidates any in-progress recording — the restored
    // state's history no longer matches the recorded action sequence.
    REPLAY_LOG.with(|cell| cell.set(None));
    invalidate_ai_proposals();
    Ok(())
}

/// Explicitly drive any persisted stack automation after a successful restore.
///
/// [`restore_game_state`] deliberately remains an undo/decode boundary: it
/// installs a playable snapshot but never manufactures a priority pass. This
/// separately-invoked transition is the only WASM owner allowed to ask the
/// engine to resume a saved `StackResolutionSession` or legacy Ready latch.
/// Its bounded engine-authored presentation describes the automated burst;
/// callers read the final game snapshot through the normal state exports.
#[wasm_bindgen]
pub fn resume_restored_game_state() -> Result<JsValue, JsValue> {
    resume_loaded_stack_automation(false)
        .map(|presentation| to_js(&presentation))
        .map_err(|error| JsValue::from_str(&error))
}

/// Resume persisted stack automation from the state currently installed in
/// this WASM instance.
///
/// A normal local restore has already invalidated undo's abandoned replay and
/// proposal authority, so its no-op resume leaves those stores alone. A real
/// engine transition (including an authorization repair) invalidates them once
/// at the same boundary. Multiplayer host resume installs a new game identity,
/// so it requests that reset even for an ordinary-priority no-op.
fn resume_loaded_stack_automation(
    reset_on_noop: bool,
) -> Result<RestoredStackAutomationPresentation, String> {
    let resumed = GAME_STATE.with(|cell| {
        let mut state = cell.take().ok_or_else(|| NOT_INITIALIZED_ERR.to_string())?;
        let resumed = resume_restored_stack_automation(&mut state);
        cell.set(Some(state));
        Ok::<_, String>(resumed)
    })?;
    let changed = !matches!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::Noop
    );
    if changed || reset_on_noop {
        clear_ai_session_cache();
        REPLAY_LOG.with(|cell| cell.set(None));
        invalidate_ai_proposals();
    }
    Ok(resumed.presentation)
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
/// 1. **Fresh RNG seed.** `restore_game_state` re-seeds from the SAVED
///    `rng_seed` and fast-forwards to the saved `rng_word_pos`, so the
///    restored game continues the very stream the snapshot was taken on —
///    correct for undo, wrong for resume, where continued play must not
///    re-draw the values the pre-save timeline already committed to. This
///    function stamps a FRESH seed and resets `rng_word_pos` to 0 so the
///    resumed host diverges instead.
///
///    It does NOT rewind to position 0: that was true only before issue
///    #5466 taught the restore path to carry the offset, and it survives
///    today just for snapshots written back then, which carry
///    `rng_word_pos == 0`. Both the shared decode chokepoint
///    (`PersistedGameState::into_game_state`) and `restore_game_state`'s
///    own repeat call `rehydrate_rng`.
/// 2. **Atomic multiplayer-flag flip.** Sets `MULTIPLAYER_MODE` in the
///    same call that loads state, so there's no window where a stray
///    `restore_game_state` (undo) would be accepted on the resumed
///    session.
///
/// Refuses when the engine is already in use — this is a fresh-instance
/// entry point. Callers must clear any existing state first.
#[wasm_bindgen]
pub fn resume_multiplayer_host_state(json_str: &str) -> Result<JsValue, JsValue> {
    if MULTIPLAYER_MODE.with(|cell| cell.get()) {
        return Err(JsValue::from_str(
            "resume_multiplayer_host_state refused: multiplayer mode already set",
        ));
    }
    if game_state_present() {
        return Err(JsValue::from_str(
            "resume_multiplayer_host_state refused: engine already initialized; call clear_game_state first",
        ));
    }

    let restored = decode_and_rehydrate_restored_game_state(json_str, |state| {
        let fresh_seed: u64 = rand::rng().random();
        state.rng_seed = fresh_seed;
        state.rng = ChaCha20Rng::seed_from_u64(fresh_seed);
        state.rng_word_pos = 0;
    })
    .map_err(|error| JsValue::from_str(&error))?;
    let mut state = restored.state;
    backfill_legacy_debug_permissions(&mut state, restored.debug_permitted_was_serialized, true);

    bind_interaction_session(&mut state);

    GAME_STATE.with(|cell| cell.set(Some(state)));
    MULTIPLAYER_MODE.with(|cell| cell.set(true));
    // The fresh RNG state is installed before the engine evaluates the saved
    // session, and no caller can observe the hosted snapshot until this
    // returns its bounded engine presentation.
    resume_loaded_stack_automation(true)
        .map(|presentation| to_js(&presentation))
        .map_err(|error| JsValue::from_str(&error))
}

#[cfg(test)]
mod restored_card_db_requirements_tests {
    use super::*;

    #[test]
    fn decoded_restore_requires_a_card_database_before_state_mutation() {
        clear_game_state();
        set_multiplayer_mode(false);
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let json = serde_json::to_string(&GameState::new_two_player(17)).unwrap();

        let error = decode_and_rehydrate_restored_game_state(&json, |_| {})
            .expect_err("restore must require CARD_DB");
        assert!(error.contains("card database"));
        assert!(GAME_STATE.with(|cell| cell.replace(None).is_none()));
        assert!(!is_multiplayer_mode());
    }
}

#[cfg(test)]
mod combat_prompt_restore_tests {
    use super::*;
    use std::collections::HashSet;

    use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::game_state::WaitingFor;
    use engine::types::phase::Phase;

    #[test]
    fn restore_rebuilds_an_empty_declare_attackers_target_snapshot() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::DeclareAttackers);
        let pyrogoyf = scenario.add_creature(P0, "Pyrogoyf", 2, 3).id();
        let guide_of_souls = scenario.add_creature(P0, "Guide of Souls", 2, 2).id();
        let mut runner = scenario.build();
        runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
        let WaitingFor::DeclareAttackers {
            valid_attack_targets,
            valid_attack_targets_by_attacker,
            ..
        } = &mut runner.state_mut().waiting_for
        else {
            panic!("scenario must enter DeclareAttackers");
        };
        valid_attack_targets.clear();
        *valid_attack_targets_by_attacker = Some(std::collections::HashMap::from([
            (pyrogoyf, Vec::new()),
            (guide_of_souls, Vec::new()),
        ]));

        let json = serde_json::to_string(runner.state())
            .expect("stale externally exported prompt serializes");
        restore_game_state(&json).expect("external restore succeeds");

        with_state(|state| match &state.waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attack_targets,
                valid_attack_targets_by_attacker: Some(by_attacker),
                ..
            } => {
                assert_eq!(valid_attack_targets, &vec![AttackTarget::Player(P1)]);
                assert_eq!(
                    valid_attack_targets.iter().copied().collect::<HashSet<_>>(),
                    by_attacker.values().flatten().copied().collect(),
                    "aggregate targets remain the union of per-attacker support"
                );
                assert_eq!(
                    by_attacker.get(&pyrogoyf),
                    Some(&vec![AttackTarget::Player(P1)]),
                    "Pyrogoyf regains its engine-authored attack target after restore"
                );
                assert_eq!(
                    by_attacker.get(&guide_of_souls),
                    Some(&vec![AttackTarget::Player(P1)]),
                    "the restored prompt rebuilds every selected attacker's target support"
                );
            }
            waiting_for => panic!("expected DeclareAttackers after restore, got {waiting_for:?}"),
        })
        .expect("restored state remains available");
        with_state_mut(|state| {
            apply(
                state,
                P0,
                GameAction::DeclareAttackers {
                    attacks: vec![
                        (pyrogoyf, AttackTarget::Player(P1)),
                        (guide_of_souls, AttackTarget::Player(P1)),
                    ],
                    bands: vec![],
                },
            )
        })
        .expect("restored state remains available")
        .expect("restored target choices reach the declaration reducer");
        clear_game_state();
    }

    #[test]
    fn restore_rebuilds_an_empty_declare_blockers_target_snapshot() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::DeclareAttackers);
        let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
        let mut runner = scenario.build();
        runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("attacker enters combat before the blocker prompt");
        runner.pass_both_players();
        let WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } = &mut runner.state_mut().waiting_for
        else {
            panic!("combat must enter DeclareBlockers");
        };
        valid_blocker_ids.clear();
        valid_block_targets.clear();

        let json = serde_json::to_string(runner.state())
            .expect("stale externally exported blocker prompt serializes");
        restore_game_state(&json).expect("external restore succeeds");

        with_state(|state| match &state.waiting_for {
            WaitingFor::DeclareBlockers {
                valid_blocker_ids,
                valid_block_targets,
                ..
            } => {
                assert_eq!(valid_blocker_ids, &vec![blocker]);
                assert_eq!(valid_block_targets.get(&blocker), Some(&vec![attacker]));
            }
            waiting_for => panic!("expected DeclareBlockers after restore, got {waiting_for:?}"),
        })
        .expect("restored state remains available");
        with_state_mut(|state| {
            apply(
                state,
                P1,
                GameAction::DeclareBlockers {
                    assignments: vec![(blocker, attacker)],
                },
            )
        })
        .expect("restored state remains available")
        .expect("restored blocker choices reach the declaration reducer");
        clear_game_state();
    }
}

#[cfg(test)]
mod legacy_debug_permission_restore_tests {
    use super::*;

    fn legacy_save_without_debug_permissions(state: GameState) -> String {
        let mut serialized = serde_json::to_value(PersistedGameState::capture(state))
            .expect("persisted test state must serialize");
        serialized
            .get_mut("state")
            .and_then(serde_json::Value::as_object_mut)
            .expect("trusted persisted state must contain a state object")
            .remove("debug_permitted");
        serde_json::to_string(&serialized).expect("legacy persisted test state must serialize")
    }

    #[test]
    fn legacy_sandbox_save_backfills_every_seat() {
        let json = legacy_save_without_debug_permissions(GameState::new(
            FormatConfig::standard().with_sandbox(),
            2,
            42,
        ));

        let mut restored = decode_restored_game_state(&json).expect("legacy save must decode");
        assert!(!restored.debug_permitted_was_serialized);
        backfill_legacy_debug_permissions(&mut restored.state, false, false);

        assert_eq!(
            restored.state.debug_permitted,
            [PlayerId(0), PlayerId(1)].into_iter().collect(),
            "legacy sandbox saves predate per-seat grants and must retain sandbox access"
        );
    }

    #[test]
    fn current_empty_sandbox_permission_set_remains_revoked() {
        let mut state = GameState::new(FormatConfig::standard().with_sandbox(), 2, 42);
        state.debug_permitted.clear();
        let json = serde_json::to_string(&PersistedGameState::capture(state))
            .expect("current persisted state must serialize");

        let mut restored = decode_restored_game_state(&json).expect("current save must decode");
        assert!(restored.debug_permitted_was_serialized);
        backfill_legacy_debug_permissions(&mut restored.state, true, false);

        assert!(
            restored.state.debug_permitted.is_empty(),
            "an explicit empty set records intentional sandbox revocation"
        );
    }

    #[test]
    fn legacy_normal_p2p_save_does_not_gain_debug_access() {
        let json =
            legacy_save_without_debug_permissions(GameState::new(FormatConfig::standard(), 2, 42));

        let mut restored = decode_restored_game_state(&json).expect("legacy save must decode");
        backfill_legacy_debug_permissions(&mut restored.state, false, true);

        assert!(
            restored.state.debug_permitted.is_empty(),
            "normal P2P restores must not gain a debug projection"
        );
    }
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

/// Mint an opaque, authority-bound proposal for the AI's next action.
///
/// Callers must submit it through [`submit_ai_action_proposal`]. The registry
/// is local to this live WASM instance and is cleared
/// on every successful state mutation, restore, resume, reset, and new game.
#[wasm_bindgen]
pub fn get_ai_action_proposal(difficulty: &str, player_id: u8) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);
    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        // The caller identifies the AI configuration to use, but never the
        // decision slot. The live prompt is the sole authority for semantic
        // ownership; this matters when control effects make its authorized
        // submitter a different player.
        let semantic_owner = ai_semantic_owner(state, PlayerId(player_id));
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        let mut rng = rand::rng();
        let session = ai_session_for(state);
        let Some(action) =
            choose_action_with_session(state, semantic_owner, &config, &mut rng, &session)
        else {
            return Ok(JsValue::NULL);
        };

        Ok(mint_ai_action_proposal(
            state,
            semantic_owner,
            contract,
            action,
        ))
    })?
}

/// Mint a proposal using the existing tactical floor without entering
/// rollout search. This is the engine-owned escape for a timed-out optional
/// scorer; it still issues and validates the current decision contract.
#[wasm_bindgen]
pub fn get_ai_tactical_action_proposal(
    difficulty: &str,
    player_id: u8,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);
    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let semantic_owner = ai_semantic_owner(state, PlayerId(player_id));
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let mut config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        // A pre-expired search deadline selects the established tactical floor
        // while retaining the same engine-owned candidate and contract checks.
        config.search.time_budget_ms = Some(0);
        let mut rng = rand::rng();
        let session = ai_session_for(state);
        let Some(action) =
            choose_action_with_session(state, semantic_owner, &config, &mut rng, &session)
        else {
            return Ok(JsValue::NULL);
        };
        Ok(mint_ai_action_proposal(
            state,
            semantic_owner,
            contract,
            action,
        ))
    })?
}

/// Mint an ordinary opaque proposal together with a local-only diagnostic
/// receipt. The receipt is an observation of the minted capability, never an
/// additional action-selection API.
#[wasm_bindgen]
pub fn get_ai_action_proposal_with_diagnostics(
    difficulty: &str,
    player_id: u8,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);
    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let semantic_owner = ai_semantic_owner(state, PlayerId(player_id));
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        let mut rng = rand::rng();
        let session = ai_session_for(state);
        let selection = choose_action_with_session_diagnostic(
            state,
            semantic_owner,
            &config,
            &mut rng,
            &session,
        );
        let Some(action) = selection.action else {
            return Ok(JsValue::NULL);
        };
        if !contract.contains_action(state, &action) {
            return Ok(JsValue::NULL);
        }
        let actor = contract.authorized_actor;
        let mut receipt = selection
            .receipt
            .expect("diagnostic chooser must observe its selected action");
        attach_receipt_object_names(state, &mut receipt);
        let token = AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract));
        Ok(to_js(&serde_json::json!({
            "proposal": { "token": token, "semanticOwner": semantic_owner.0, "actor": actor.0, "action": action },
            "receipt": receipt,
        })))
    })?
}

/// Diagnostic counterpart of [`get_ai_tactical_action_proposal`].
#[wasm_bindgen]
pub fn get_ai_tactical_action_proposal_with_diagnostics(
    difficulty: &str,
    player_id: u8,
) -> Result<JsValue, JsValue> {
    let ai_difficulty = AiDifficulty::from_label(difficulty);
    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let semantic_owner = ai_semantic_owner(state, PlayerId(player_id));
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let mut config =
            create_config_for_players(ai_difficulty, Platform::Wasm, state.players.len() as u8);
        config.search.time_budget_ms = Some(0);
        let mut rng = rand::rng();
        let session = ai_session_for(state);
        let selection = choose_action_with_session_diagnostic(
            state,
            semantic_owner,
            &config,
            &mut rng,
            &session,
        );
        let Some(action) = selection.action else {
            return Ok(JsValue::NULL);
        };
        if !contract.contains_action(state, &action) {
            return Ok(JsValue::NULL);
        }
        let actor = contract.authorized_actor;
        let mut receipt = selection
            .receipt
            .expect("diagnostic chooser must observe its selected action");
        attach_receipt_object_names(state, &mut receipt);
        let token = AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract));
        Ok(to_js(&serde_json::json!({
            "proposal": { "token": token, "semanticOwner": semantic_owner.0, "actor": actor.0, "action": action },
            "receipt": receipt,
        })))
    })?
}

/// Score one parallel-worker sample against the thread-local state.
///
/// Split out of [`get_ai_scored_candidates`] so native tests can drive the real
/// scoring path: the `#[wasm_bindgen]` shell returns through `to_js`, which calls
/// the real `JSON.parse` binding and panics outside a wasm32 runtime (same reason
/// `scored_candidates_inner` exists).
fn scored_candidates_inner(
    state: &mut GameState,
    difficulty: AiDifficulty,
    ai_player: PlayerId,
    rng_seed: u64,
) -> Vec<(GameAction, f64)> {
    engine::game::layers::flush_layers(state);

    // A pool worker scores on its OWN entropy stream so root-parallel samples
    // diverge (`AiWorkerPool` passes `baseSeed + index`); `score_candidates_with_session`
    // names this the WASM divergence channel. `rng` is `#[serde(skip)]`, so
    // `rng_seed` + `rng_word_pos` are its only carriers: writing one without the
    // others splits the stream identity in two. A fresh ChaCha20 stream starts at
    // word 0, so a surviving high-water leaves `advance_rng_high_water` guarding a
    // position the live cursor is BEHIND and the next `capture_rng_word_pos`
    // `.expect`-panics `HighWaterRegression` — which every simulated library
    // shuffle performs, and so does `export_game_state_json`. Overwrite all three,
    // exactly as `resume_multiplayer_host_state` does.
    state.rng_seed = rng_seed;
    state.rng = ChaCha20Rng::seed_from_u64(rng_seed);
    state.rng_word_pos = 0;

    let config = create_config_for_players(difficulty, Platform::Wasm, state.players.len() as u8);
    let session = ai_session_for(state);
    score_candidates_for_parallel_worker(state, ai_player, &config, Some(&session))
}

/// Score candidates inside an isolated AI worker. These are plain,
/// serializable hints rather than capabilities: they cannot cross the action
/// boundary until the live main engine reissues an exact proposal.
#[wasm_bindgen]
pub fn get_ai_scored_candidates(
    difficulty: &str,
    player_id: u8,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let difficulty = AiDifficulty::from_label(difficulty);
    let scores = with_state_mut(|state| {
        scored_candidates_inner(state, difficulty, PlayerId(player_id), rng_seed)
    })?;
    Ok(to_js(&scores))
}

/// Convert score-only worker output into an authority-bound proposal.
///
/// The worker state may be old, from another game, or maliciously altered.
/// Consequently this endpoint always derives a new decision contract from the
/// main WASM state, discards every score whose action is not an exact member,
/// and only then mints an opaque proposal. There is intentionally no public
/// score-to-`GameAction` endpoint.
#[wasm_bindgen]
pub fn get_ai_action_proposal_from_scores(
    scores_json: &str,
    difficulty: &str,
    player_id: u8,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let scored: Vec<(GameAction, f64)> = serde_json::from_str(scores_json)
        .map_err(|error| JsValue::from_str(&format!("Failed to deserialize AI scores: {error}")))?;
    let difficulty = AiDifficulty::from_label(difficulty);

    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let requested_ai = PlayerId(player_id);
        let semantic_owner = if state.waiting_for.acting_players().contains(&requested_ai) {
            requested_ai
        } else {
            state
                .waiting_for
                .acting_player()
                .or_else(|| state.waiting_for.acting_players().first().copied())
                .unwrap_or(requested_ai)
        };
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let admissible_scores: Vec<(GameAction, f64)> = scored
            .into_iter()
            .filter(|(action, _)| contract.contains_action(state, action))
            .collect();
        let config =
            create_config_for_players(difficulty, Platform::Wasm, state.players.len() as u8);
        let mut rng = ChaCha20Rng::seed_from_u64(rng_seed);
        let Some(action) =
            select_safe_action_from_scores(state, &admissible_scores, config.temperature, &mut rng)
        else {
            return Ok(JsValue::NULL);
        };

        let actor = contract.authorized_actor;
        let token = AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract));
        Ok(to_js(&serde_json::json!({
            "token": token,
            "semanticOwner": semantic_owner.0,
            "actor": actor.0,
            "action": action,
        })))
    })?
}

/// Diagnostic counterpart of score-worker proposal rebinding. It preserves the
/// existing authority filter and selector; the returned receipt is local WASM
/// observability data bound to the same opaque token.
#[wasm_bindgen]
pub fn get_ai_action_proposal_from_scores_with_diagnostics(
    scores_json: &str,
    difficulty: &str,
    player_id: u8,
    rng_seed: u64,
) -> Result<JsValue, JsValue> {
    let scored: Vec<(GameAction, f64)> = serde_json::from_str(scores_json)
        .map_err(|error| JsValue::from_str(&format!("Failed to deserialize AI scores: {error}")))?;
    let difficulty = AiDifficulty::from_label(difficulty);
    with_state_mut(|state| {
        engine::game::layers::flush_layers(state);
        let requested_ai = PlayerId(player_id);
        let semantic_owner = if state.waiting_for.acting_players().contains(&requested_ai) {
            requested_ai
        } else {
            state
                .waiting_for
                .acting_player()
                .or_else(|| state.waiting_for.acting_players().first().copied())
                .unwrap_or(requested_ai)
        };
        let contract = AiDecisionContract::issue(state, semantic_owner);
        let admissible_scores: Vec<(GameAction, f64)> = scored
            .into_iter()
            .filter(|(action, _)| contract.contains_action(state, action))
            .collect();
        let config =
            create_config_for_players(difficulty, Platform::Wasm, state.players.len() as u8);
        let mut rng = ChaCha20Rng::seed_from_u64(rng_seed);
        let Some(selected_index) = phase_ai::select_safe_action_index_from_scores(
            state,
            &admissible_scores,
            config.temperature,
            &mut rng,
        ) else {
            return Ok(JsValue::NULL);
        };
        let action = admissible_scores[selected_index].0.clone();
        let actor = contract.authorized_actor;
        let mut receipt = phase_ai::decision_receipt::ranked_receipt(
            &contract,
            &admissible_scores,
            Some(selected_index),
            config.temperature,
            action.clone(),
        );
        attach_receipt_object_names(state, &mut receipt);
        let token = AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract));
        Ok(to_js(&serde_json::json!({
            "proposal": { "token": token, "semanticOwner": semantic_owner.0, "actor": actor.0, "action": action },
            "receipt": receipt,
        })))
    })?
}

/// Submit an action selected from an engine-issued AI proposal.
///
/// A stale or foreign proposal is a normal race outcome and is returned as a
/// tagged value. Rejected actions leave the proposal live for diagnostics or a
/// retry; only a successful apply invalidates the authority generation.
#[wasm_bindgen]
pub fn submit_ai_action_proposal(token: &str, actor: u8, action: JsValue) -> JsValue {
    let action: GameAction = match serde_wasm_bindgen::from_value(action) {
        Ok(action) => action,
        Err(_) => {
            return to_js(&AiProposalSubmission::Rejected {
                rejection: ActionRejection::new(ActionRejectionCode::InvalidAction),
            });
        }
    };
    let actor = PlayerId(actor);
    let Some(proposal) = AI_PROPOSALS.with(|registry| registry.borrow().proposal(token).cloned())
    else {
        return to_js(&AiProposalSubmission::Stale {
            reason: "unknown_or_invalidated_token",
        });
    };

    match with_state_mut(|state| {
        // Classification lives in the engine (`verified_ai_stack_pass_player`),
        // not in this adapter: it is the same call the callee gates on, so the
        // two cannot disagree. CLAUDE.md — transport layers hold zero game
        // logic.
        let is_stack_recheck_pass =
            engine::game::engine::verified_ai_stack_pass_player(state, &action).is_some();
        // A payment finalize is no longer misclassified, so it now passes
        // through `permits` — whose `state_revision` equality check can report
        // `Stale` for a proposal minted against a superseded state. The client
        // treats `Stale` as a benign race and re-queries without counting a
        // failure, which is the intended handling for every other action.
        if !is_stack_recheck_pass && !proposal.contract.permits(state, actor, &action) {
            return AiProposalSubmission::Stale {
                reason: "decision_changed_or_action_outside_issued_bounds",
            };
        }
        let applied = if is_stack_recheck_pass {
            engine::game::engine::apply_verified_ai_priority_pass_with_rejection(
                state,
                actor,
                &proposal.contract,
                action.clone(),
            )
        } else {
            apply_interaction_with_rejection(
                state,
                actor,
                proposal.contract.semantic_owner,
                action.clone(),
            )
        };
        match applied {
            Ok(result) => {
                if is_stack_recheck_pass {
                    record_verified_ai_priority_pass(actor, proposal.contract.semantic_owner);
                } else {
                    record_replay_action(false, actor, action);
                }
                invalidate_ai_proposals();
                AiProposalSubmission::Applied {
                    result: Box::new(result),
                }
            }
            Err(rejection) => AiProposalSubmission::Rejected { rejection },
        }
    }) {
        Ok(outcome) => to_js(&outcome),
        Err(_) => to_js(&AiProposalSubmission::Stale {
            reason: "state_unavailable",
        }),
    }
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

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::sync::Arc;

    use engine::game::deck_loading::create_object_from_card_face;
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ChosenAttribute,
        ContinuousModification, Duration, Effect, QuantityExpr, QuantityRef, ResolvedAbility,
        TargetFilter, TargetRef,
    };
    use engine::types::actions::{ResolveAllConsentDecision, ResolveAllScope};
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::counter::{CounterMatch, CounterType};
    use engine::types::game_state::{
        MulliganDecisionEntry, MulliganDecisionPhase, NamedChoiceSource, NamedChoiceSourceBinding,
        OpponentGuessOwner, OpponentGuessSource, PromptSourceBinding, ResolveAllConsentParticipant,
        ResolveAllConsentRun, ResolveAllPrioritySnapshot, StackEntry, StackEntryKind,
        StackResolutionBudget, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;

    use engine::types::zones::Zone;

    fn proposal_outcome(token: &str, actor: PlayerId, action: &GameAction) -> serde_json::Value {
        serde_wasm_bindgen::from_value(submit_ai_action_proposal(token, actor.0, to_js(action)))
            .expect("proposal outcome must serialize")
    }

    #[test]
    fn initialize_game_returns_error_for_malformed_format_config_without_standard_fallback() {
        clear_game_state();
        let malformed_format_config = serde_wasm_bindgen::to_value(&serde_json::json!(42))
            .expect("malformed JSON value converts to a JS input");

        let result = initialize_game(
            JsValue::NULL,
            Some(42.0),
            malformed_format_config,
            JsValue::NULL,
            Some(2),
            None,
        );
        let error: serde_json::Value =
            serde_wasm_bindgen::from_value(result).expect("initializer error is a JS object");

        assert_eq!(error["error"], true);
        assert!(error["reasons"][0]
            .as_str()
            .expect("error reason is a string")
            .contains("Format config deserialization failed"));
        assert!(GAME_STATE.with(|cell| cell.replace(None).is_none()));
    }

    /// Installs a real engine state and returns the production finite decision
    /// domain for `semantic_owner`. Tests must never fabricate a contract: the
    /// contract is the authority that derives every bound from `WaitingFor`.
    fn issue_contract(state: GameState, semantic_owner: PlayerId) -> AiDecisionContract {
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(state)));
        with_state(|state| AiDecisionContract::issue(state, semantic_owner))
            .expect("test state must remain installed")
    }

    /// Registers a production-issued contract only after proving `action` is
    /// within its engine-issued bounds. This mirrors the public proposal
    /// endpoint's issuance path without hand-authoring candidate metadata.
    fn install_issued_candidate(
        state: GameState,
        semantic_owner: PlayerId,
        action: &GameAction,
    ) -> String {
        let contract = issue_contract(state, semantic_owner);
        assert!(
            with_state(|state| contract.contains_action(state, action))
                .expect("test state must remain installed"),
            "action must come from the engine-issued domain: {action:?}"
        );
        AI_PROPOSALS.with(|registry| registry.borrow_mut().insert(contract))
    }

    fn install_issued_contract(state: GameState, semantic_owner: PlayerId) -> AiDecisionContract {
        issue_contract(state, semantic_owner)
    }

    fn issue_public_proposal(state: GameState, player: PlayerId) -> serde_json::Value {
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(state)));
        serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", player.0)
                .expect("the production issuer must not throw"),
        )
        .expect("proposal must serialize")
    }

    fn submit_public_proposal(proposal: &serde_json::Value) -> GameAction {
        let token = proposal["token"].as_str().expect("opaque token");
        let actor = proposal["actor"].as_u64().expect("proposal actor") as u8;
        let action: GameAction = serde_json::from_value(proposal["action"].clone())
            .expect("proposal action must be a GameAction");
        let outcome = serde_wasm_bindgen::from_value::<serde_json::Value>(
            submit_ai_action_proposal(token, actor, to_js(&action)),
        )
        .expect("submission outcome must serialize");
        assert_eq!(
            outcome["status"], "applied",
            "production-issued {action:?} must cross the public action boundary"
        );
        action
    }

    /// Exercises the actual public capability path rather than registering a
    /// test-only contract. This is the boundary used by both the browser AI
    /// controller and worker-score rebinding.
    fn issue_and_submit_public_proposal(state: GameState, player: PlayerId) -> GameAction {
        let proposal = issue_public_proposal(state, player);
        let action = submit_public_proposal(&proposal);
        clear_game_state();
        action
    }

    fn load_disruptor_flute_database() {
        load_card_database(
            r#"{
                "disruptor flute": {
                    "name": "Disruptor Flute",
                    "mana_cost": { "type": "NoCost" },
                    "card_type": { "supertypes": [], "core_types": ["Artifact"], "subtypes": [] },
                    "power": null,
                    "toughness": null,
                    "loyalty": null,
                    "defense": null,
                    "oracle_text": "Flash\\nAs this artifact enters, choose a card name.",
                    "abilities": [],
                    "triggers": [],
                    "static_abilities": [],
                    "replacements": [],
                    "keywords": []
                }
            }"#,
        )
        .expect("Disruptor Flute fixture database must load");
    }

    fn disruptor_flute_card_name_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(880),
            PlayerId(0),
            "Disruptor Flute".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source: None,
            persist_player: None,
        };
        state
    }

    fn fireball_final_target_state(pool: usize) -> (GameState, TargetRef) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first_target = scenario.add_creature(P1, "Fireball Target One", 3, 3).id();
        let final_target = scenario.add_creature(P1, "Fireball Target Two", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Fireball", true)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 0,
            })
            .with_strive_cost(ManaCost::Cost {
                shards: Vec::new(),
                generic: 1,
            })
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DealDamage {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid,
                        },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                )
                .sub_ability(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DealDamage {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid,
                        },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                )),
            )
            .id();
        scenario.with_mana_pool(
            P0,
            (0..pool)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, Vec::new()))
                .collect(),
        );

        let mut state = scenario.build().state().clone();
        engine::game::engine::apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(spell.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        )
        .expect("Fireball announcement must reach ChooseX");
        engine::game::engine::apply_as_current(&mut state, GameAction::ChooseX { value: 3 })
            .expect("Fireball X announcement must reach target selection");
        engine::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(first_target)),
            },
        )
        .expect("first Fireball target must leave the final target slot pending");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        (state, TargetRef::Object(final_target))
    }

    /// The contract is only useful if every member can cross the public
    /// proposal boundary. Reinstall the unchanged pre-decision state for each
    /// member because a successful submission invalidates its siblings.
    fn assert_every_issued_candidate_applies(state: &GameState, semantic_owner: PlayerId) {
        let contract = install_issued_contract(state.clone(), semantic_owner);
        assert!(
            !contract.candidates.is_empty(),
            "the real WaitingFor state must issue at least one candidate"
        );
        let actor = contract.authorized_actor;
        for candidate in contract.candidates {
            let token = install_issued_candidate(state.clone(), semantic_owner, &candidate.action);
            assert_eq!(
                proposal_outcome(&token, actor, &candidate.action)["status"],
                "applied",
                "every issued candidate must submit through the public boundary: {:?}",
                candidate.action
            );
        }
    }

    fn priority_state(player: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        state
    }

    fn install_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        loyalty: u32,
        abilities: Vec<AbilityDefinition>,
    ) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Proposal Walker".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&object_id).expect("created walker");
        object.card_types.core_types.push(CoreType::Planeswalker);
        object.loyalty = Some(loyalty);
        object
            .counters
            .insert(engine::types::counter::CounterType::Loyalty, loyalty);
        object.abilities = Arc::new(abilities);
        object_id
    }

    fn loyalty_ability(amount: i32, effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect)
            .cost(AbilityCost::Loyalty { amount })
            .sorcery_speed()
    }

    fn minus_x_loyalty_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect)
            .cost(AbilityCost::RemoveCounter {
                count: engine::types::ability::REMOVE_COUNTER_COST_X,
                counter_type: CounterMatch::OfType(CounterType::Loyalty),
                target: None,
                selection: engine::types::ability::CounterCostSelection::SingleObject,
            })
            .sorcery_speed()
    }

    fn card_predicate_guess_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "Predicate guess source".to_string(),
            Zone::Battlefield,
        );
        let context = engine::game::triggers::trigger_source_context_for_latch(
            &state,
            state.objects.get(&source_id).expect("created source"),
        );
        let predicates = ChoiceType::land_or_nonland_card_predicate_options();
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(1),
            choice_type: ChoiceType::CardPredicateGuess {
                options: predicates.clone(),
            },
            options: ChoiceType::card_predicate_labels(&predicates),
            source: Some(NamedChoiceSource::from_trigger_source(
                context,
                NamedChoiceSourceBinding::ResolutionContext,
            )),
            persist_player: None,
        };
        state
    }

    fn opponent_guess_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(911),
            PlayerId(1),
            "Opponent guess source".to_string(),
            Zone::Battlefield,
        );
        let context = engine::game::triggers::trigger_source_context_for_latch(
            &state,
            state.objects.get(&source_id).expect("created source"),
        );
        state.waiting_for = WaitingFor::OpponentGuess {
            player: PlayerId(0),
            options: vec!["greater".to_string(), "not greater".to_string()],
            choice_type: ChoiceType::Labeled {
                options: vec!["greater".to_string(), "not greater".to_string()],
            },
            source: OpponentGuessSource {
                prompt: PromptSourceBinding::from_trigger_source(&context),
            },
            owner: Some(OpponentGuessOwner {
                context,
                committed_choice: Some(ChosenAttribute::Number(7)),
            }),
            proposition_truth: Some(true),
        };
        state
    }

    #[test]
    fn restored_disruptor_flute_card_name_proposal_applies_after_rehydration() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_disruptor_flute_database();
        let json = serde_json::to_string(&disruptor_flute_card_name_state()).unwrap();

        restore_game_state(&json).expect("restore must rehydrate CardName metadata");
        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", PlayerId(0).0)
                .expect("public issuer must answer restored Flute prompt"),
        )
        .unwrap();
        assert!(matches!(
            serde_json::from_value::<GameAction>(proposal["action"].clone()),
            Ok(GameAction::ChooseOption { ref choice }) if choice == "Disruptor Flute"
        ));
        submit_public_proposal(&proposal);
        with_state(|state| assert!(matches!(state.waiting_for, WaitingFor::Priority { .. })))
            .expect("applied card-name choice must leave a live successor");
        clear_game_state();
    }

    #[test]
    fn resumed_disruptor_flute_card_name_proposal_applies_after_rehydration() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_disruptor_flute_database();
        let json = serde_json::to_string(&disruptor_flute_card_name_state()).unwrap();

        resume_multiplayer_host_state(&json).expect("resume must rehydrate CardName metadata");
        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", PlayerId(0).0)
                .expect("public issuer must answer resumed Flute prompt"),
        )
        .unwrap();
        assert!(matches!(
            serde_json::from_value::<GameAction>(proposal["action"].clone()),
            Ok(GameAction::ChooseOption { ref choice }) if choice == "Disruptor Flute"
        ));
        submit_public_proposal(&proposal);
        with_state(|state| assert!(matches!(state.waiting_for, WaitingFor::Priority { .. })))
            .expect("applied card-name choice must leave a live successor");
        assert!(is_multiplayer_mode());
        clear_game_state();
        set_multiplayer_mode(false);
    }

    #[test]
    fn public_fireball_final_target_filters_unpayable_surcharge_and_keeps_payable_sibling() {
        clear_game_state();
        // {X}{R} with X=3 costs four mana for one target. The final second
        // target adds the pinned Fireball/Strive-shaped {1} surcharge, so this
        // exact reducer transition is rejected from a four-mana pool.
        let (doomed_state, doomed_target) = fireball_final_target_state(4);
        let doomed_action = GameAction::ChooseTarget {
            target: Some(doomed_target.clone()),
        };
        let mut direct_doomed_state = doomed_state.clone();
        let error =
            engine::game::engine::apply_as_current(&mut direct_doomed_state, doomed_action.clone())
                .expect_err(
                    "reach guard: the final target must hit the unpayable payment boundary",
                );
        assert!(
            error.to_string().contains("Cannot pay mana cost"),
            "expected the production payment rejection, got {error}"
        );
        let doomed_contract = AiDecisionContract::issue(&doomed_state, P0);
        assert!(
            !doomed_contract.contains_action(&doomed_state, &doomed_action),
            "the unpayable final target must not enter the issued contract"
        );
        assert!(doomed_contract.contains_action(&doomed_state, &GameAction::CancelCast));
        assert!(
            !engine::ai_support::legal_actions(&doomed_state).contains(&doomed_action),
            "public legal actions must share the contract's filtered target domain"
        );
        GAME_STATE.with(|cell| cell.set(Some(doomed_state)));
        let doomed_proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", P0.0)
                .expect("public issuer must expose the issued cancellation"),
        )
        .expect("proposal must serialize");
        assert!(matches!(
            serde_json::from_value::<GameAction>(doomed_proposal["action"].clone()),
            Ok(GameAction::CancelCast)
        ));
        submit_public_proposal(&doomed_proposal);
        clear_game_state();

        let (payable_state, payable_target) = fireball_final_target_state(5);
        let payable_action = GameAction::ChooseTarget {
            target: Some(payable_target),
        };
        let payable_contract = AiDecisionContract::issue(&payable_state, P0);
        assert!(
            payable_contract.contains_action(&payable_state, &payable_action),
            "the same final target must remain issued once its target-dependent cost is payable"
        );
        assert!(engine::ai_support::legal_actions(&payable_state).contains(&payable_action));
        GAME_STATE.with(|cell| cell.set(Some(payable_state)));
        let payable_proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", P0.0)
                .expect("public issuer must retain the payable target"),
        )
        .expect("proposal must serialize");
        assert!(matches!(
            serde_json::from_value::<GameAction>(payable_proposal["action"].clone()),
            Ok(GameAction::ChooseTarget { target: Some(_) })
        ));
        submit_public_proposal(&payable_proposal);
        clear_game_state();
    }

    #[test]
    fn proposal_boundary_rejects_changed_x_target_and_payment_arguments() {
        let player = PlayerId(0);
        let mut x_state = priority_state(player);
        let x_walker = install_planeswalker(
            &mut x_state,
            player,
            3,
            vec![minus_x_loyalty_ability(Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                },
                player: TargetFilter::Controller,
            })],
        );
        engine::game::engine::apply_as_current(
            &mut x_state,
            GameAction::ActivateAbility {
                source_id: x_walker,
                ability_index: 0,
            },
        )
        .expect("real [-X] activation must issue an X prompt");
        let x_contract = install_issued_contract(x_state.clone(), player);
        assert_eq!(
            x_contract
                .candidates
                .iter()
                .filter(|candidate| matches!(candidate.action, GameAction::ChooseX { .. }))
                .count(),
            4,
            "the real X prompt must expose its inclusive [0, 3] domain"
        );
        assert_every_issued_candidate_applies(&x_state, player);
        let issued_x = GameAction::ChooseX { value: 1 };
        let token = install_issued_candidate(x_state, player, &issued_x);
        let outcome = proposal_outcome(&token, player, &GameAction::ChooseX { value: 4 });
        assert_eq!(outcome["status"], "stale");
        assert_eq!(
            outcome["reason"],
            "decision_changed_or_action_outside_issued_bounds"
        );

        let mut target_state = priority_state(player);
        let target_walker = install_planeswalker(
            &mut target_state,
            player,
            3,
            vec![loyalty_ability(
                -1,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                },
            )],
        );
        engine::game::engine::apply_as_current(
            &mut target_state,
            GameAction::ActivateAbility {
                source_id: target_walker,
                ability_index: 0,
            },
        )
        .expect("real targeted loyalty activation must issue a target prompt");
        let target_contract = install_issued_contract(target_state.clone(), player);
        let issued_target = target_contract
            .candidates
            .iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Player(PlayerId(1)))
                    }
                )
            })
            .expect("the target prompt must bind player 1 as an actual candidate")
            .action
            .clone();
        assert_every_issued_candidate_applies(&target_state, player);
        let token = install_issued_candidate(target_state, player, &issued_target);
        let outcome = proposal_outcome(
            &token,
            player,
            &GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Player(player)),
            },
        );
        assert_eq!(outcome["status"], "stale");
        assert_eq!(
            outcome["reason"],
            "decision_changed_or_action_outside_issued_bounds"
        );

        let mut payment_state = GameState::new_two_player(42);
        payment_state.players[0].energy = 3;
        let payment_ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(800),
            player,
        );
        payment_state.stack.push_back(StackEntry {
            id: ObjectId(801),
            source_id: ObjectId(800),
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(800),
                ability: Box::new(payment_ability),
            },
        });
        engine::game::stack::resolve_top(&mut payment_state, &mut Vec::new());
        let payment_contract = install_issued_contract(payment_state.clone(), player);
        let issued_payment = payment_contract
            .candidates
            .iter()
            .find(|candidate| matches!(candidate.action, GameAction::SubmitPayAmount { amount: 1 }))
            .expect("the real energy payment must expose amount 1")
            .action
            .clone();
        assert_every_issued_candidate_applies(&payment_state, player);
        let token = install_issued_candidate(payment_state, player, &issued_payment);
        let outcome = proposal_outcome(&token, player, &GameAction::SubmitPayAmount { amount: 4 });
        assert_eq!(outcome["status"], "stale");
        assert_eq!(
            outcome["reason"],
            "decision_changed_or_action_outside_issued_bounds"
        );
        clear_game_state();
    }

    #[test]
    fn proposal_boundary_rejects_wrong_actor_and_restore_invalidates_same_revision() {
        let player = PlayerId(0);
        let action = GameAction::PassPriority;
        let token = install_issued_candidate(priority_state(player), player, &action);
        assert_eq!(
            proposal_outcome(&token, PlayerId(1), &action)["status"],
            "stale"
        );

        let token = install_issued_candidate(priority_state(player), player, &action);
        let state_json = export_game_state_json().expect("live state exports");
        restore_game_state(&state_json).expect("same-revision restore succeeds");
        assert_eq!(proposal_outcome(&token, player, &action)["status"], "stale");
        clear_game_state();
    }

    #[test]
    fn proposal_boundary_binds_semantic_owner_and_controlled_turn_actor() {
        let owner = PlayerId(1);
        let controller = PlayerId(0);
        let action = GameAction::PassPriority;
        let mut controlled = priority_state(owner);
        controlled.turn_decision_controller = Some(controller);
        controlled.priority_player = controller;
        let token = install_issued_candidate(controlled, owner, &action);

        // The authorized controller may act for the controlled semantic owner.
        assert_eq!(
            proposal_outcome(&token, controller, &action)["status"],
            "applied"
        );

        // A proposal binds its semantic slot even when the actor is allowed to
        // make decisions for another player: P0 cannot repurpose this token
        // for P0's own prompt.
        let mut controlled = priority_state(owner);
        controlled.turn_decision_controller = Some(controller);
        controlled.priority_player = controller;
        let token = install_issued_candidate(controlled, owner, &action);
        GAME_STATE.with(|cell| {
            let mut state = cell.take().expect("test state");
            state.waiting_for = WaitingFor::Priority { player: controller };
            cell.set(Some(state));
        });
        assert_eq!(
            proposal_outcome(&token, controller, &action)["status"],
            "stale"
        );

        // Authorization is also live state, not a property the original
        // submitter may retain after the turn-control mapping changes.
        let mut controlled = priority_state(owner);
        controlled.turn_decision_controller = Some(controller);
        controlled.priority_player = controller;
        let token = install_issued_candidate(controlled, owner, &action);
        GAME_STATE.with(|cell| {
            let mut state = cell.take().expect("test state");
            state.turn_decision_controller = Some(PlayerId(1));
            state.priority_player = PlayerId(1);
            cell.set(Some(state));
        });
        assert_eq!(
            proposal_outcome(&token, controller, &action)["status"],
            "stale",
            "an actor-remap race must invalidate the old controller's proposal"
        );
        clear_game_state();
    }

    #[test]
    fn simultaneous_mulligan_proposals_are_scoped_to_the_named_pending_owner() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![
                MulliganDecisionEntry {
                    player: PlayerId(0),
                    mulligan_count: 0,
                    phase: MulliganDecisionPhase::Declare,
                },
                MulliganDecisionEntry {
                    player: PlayerId(1),
                    mulligan_count: 0,
                    phase: MulliganDecisionPhase::Declare,
                },
            ],
            free_first_mulligan: false,
        };
        let keep = GameAction::MulliganDecision {
            choice: engine::types::actions::MulliganChoice::Keep,
        };

        let p0_token = install_issued_candidate(state.clone(), PlayerId(0), &keep);
        assert_eq!(
            proposal_outcome(&p0_token, PlayerId(1), &keep)["status"],
            "stale"
        );

        let p1_token = install_issued_candidate(state.clone(), PlayerId(1), &keep);
        assert!(
            AI_PROPOSALS.with(|registry| registry.borrow().proposal(&p1_token).is_some()),
            "each simultaneous decision keeps one independently-live proposal"
        );
        assert_eq!(
            proposal_outcome(&p0_token, PlayerId(0), &keep)["status"],
            "applied",
            "issuing a proposal for another simultaneous decision must not revoke this one"
        );

        let p1_token = install_issued_candidate(state, PlayerId(1), &keep);
        assert_eq!(
            proposal_outcome(&p1_token, PlayerId(1), &keep)["status"],
            "applied"
        );
        clear_game_state();
    }

    #[test]
    fn proposal_boundary_applies_an_issued_priority_candidate() {
        let player = PlayerId(0);
        let state = priority_state(player);
        let contract = install_issued_contract(state.clone(), player);
        assert!(
            !contract.candidates.is_empty(),
            "priority must issue a finite domain"
        );

        for candidate in contract.candidates {
            let token = install_issued_candidate(state.clone(), player, &candidate.action);
            assert_eq!(
                proposal_outcome(&token, player, &candidate.action)["status"],
                "applied"
            );
        }
        clear_game_state();
    }

    #[test]
    fn stack_pass_proposal_uses_the_verified_recheck_seam() {
        let player = PlayerId(0);
        let mut state = priority_state(player);
        state
            .stack
            .push_back(no_op_stack_entry(70_101, PlayerId(1)));
        add_non_mana_recheck_action(&mut state, PlayerId(1));
        let action = GameAction::PassPriority;
        let token = install_issued_candidate(state, player, &action);

        assert_eq!(
            proposal_outcome(&token, player, &action)["status"],
            "applied"
        );
        with_state(|state| {
            assert_eq!(
                state
                    .stack_resolution_session
                    .as_ref()
                    .map(|session| session.policy),
                Some(engine::types::game_state::StackResolutionPolicy::RecheckNoMeaningfulPriorityAction),
                "the WASM proposal boundary must not downgrade a verified stack pass"
            );
        })
        .expect("test state must remain installed");
        clear_game_state();
    }

    #[test]
    fn public_proposal_issuer_mints_a_submitable_priority_capability() {
        let player = PlayerId(0);
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(priority_state(player))));

        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", player.0)
                .expect("the production issuer must return a priority proposal"),
        )
        .expect("proposal must serialize");
        assert_eq!(proposal["semanticOwner"], player.0);
        assert_eq!(proposal["actor"], player.0);
        assert_eq!(proposal["action"]["type"], "PassPriority");
        let outcome =
            serde_wasm_bindgen::from_value::<serde_json::Value>(submit_ai_action_proposal(
                proposal["token"].as_str().expect("opaque token"),
                player.0,
                to_js(&proposal["action"]),
            ))
            .expect("submission outcome must serialize");
        assert_eq!(outcome["status"], "applied");
        clear_game_state();
    }

    #[test]
    fn tactical_proposal_issuer_mints_a_submitable_priority_capability() {
        let player = PlayerId(0);
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(priority_state(player))));

        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_tactical_action_proposal("VeryHard", player.0)
                .expect("the tactical issuer must return a priority proposal"),
        )
        .expect("tactical proposal must serialize");
        assert_eq!(proposal["semanticOwner"], player.0);
        assert_eq!(proposal["actor"], player.0);
        assert_eq!(proposal["action"]["type"], "PassPriority");
        let outcome =
            serde_wasm_bindgen::from_value::<serde_json::Value>(submit_ai_action_proposal(
                proposal["token"].as_str().expect("opaque token"),
                player.0,
                to_js(&proposal["action"]),
            ))
            .expect("submission outcome must serialize");
        assert_eq!(outcome["status"], "applied");
        clear_game_state();
    }

    #[test]
    fn public_proposal_issuer_submits_special_and_fallback_decision_families() {
        let player = PlayerId(0);

        // Tribute is a phase-ai special decision, not the generic planner.
        let mut tribute = GameState::new_two_player(42);
        tribute.active_player = player;
        let tribute_source = create_object(
            &mut tribute,
            CardId(900),
            PlayerId(1),
            "Tribute source".to_string(),
            Zone::Battlefield,
        );
        tribute.waiting_for = WaitingFor::TributeChoice {
            player,
            source_id: tribute_source,
            count: 1,
        };
        assert!(matches!(
            issue_and_submit_public_proposal(tribute, player),
            GameAction::DecideOptionalEffect { .. }
        ));

        // Search has its own hidden-zone chooser. The selection must be a
        // bounded engine candidate before it can reach the action boundary.
        let mut search = GameState::new_two_player(42);
        let card = create_object(
            &mut search,
            CardId(901),
            player,
            "Search card".to_string(),
            Zone::Library,
        );
        search.waiting_for = WaitingFor::SearchChoice {
            player,
            library_owner: None,
            cards: vec![card],
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: Default::default(),
            ordering_hint: Default::default(),
            split: None,
        };
        assert!(matches!(
            issue_and_submit_public_proposal(search, player),
            GameAction::SelectCards { .. }
        ));

        // Combat bypasses the priority planner. Its deterministic empty-attack
        // fallback remains a real bounded declaration, never a fabricated pass.
        let mut combat = GameState::new_two_player(42);
        combat.phase = Phase::DeclareAttackers;
        combat.active_player = player;
        combat.waiting_for = WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![engine::game::combat::AttackTarget::Player(PlayerId(1))],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        assert!(matches!(
            issue_and_submit_public_proposal(combat, player),
            GameAction::DeclareAttackers { .. }
        ));
    }

    #[test]
    fn public_proposal_issuer_submits_random_card_predicate_guess() {
        let proposal = issue_public_proposal(card_predicate_guess_state(), PlayerId(1));
        assert_eq!(proposal["semanticOwner"], 1);
        assert!(matches!(
            serde_json::from_value::<GameAction>(proposal["action"].clone()),
            Ok(GameAction::ChooseOption { ref choice }) if choice == "Land" || choice == "Nonland"
        ));
        submit_public_proposal(&proposal);
        clear_game_state();
    }

    #[test]
    fn public_proposal_issuer_submits_opponent_guess() {
        let proposal = issue_public_proposal(opponent_guess_state(), PlayerId(0));
        assert_eq!(proposal["semanticOwner"], 0);
        assert!(matches!(
            serde_json::from_value::<GameAction>(proposal["action"].clone()),
            Ok(GameAction::ChooseOption { ref choice }) if choice == "greater" || choice == "not greater"
        ));
        submit_public_proposal(&proposal);
        clear_game_state();
    }

    #[test]
    fn empty_worker_scores_fall_back_to_an_authoritative_public_proposal() {
        let player = PlayerId(0);
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(priority_state(player))));

        // An empty worker result has no action payload that the caller could
        // dispatch. The adapter must obtain a fresh capability from the live
        // authority instead of fabricating a fallback GameAction in TypeScript.
        assert!(
            get_ai_action_proposal_from_scores("[]", "VeryHard", player.0, 7)
                .expect("empty score payload is valid")
                .is_null()
        );

        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", player.0)
                .expect("authoritative fallback proposal issues"),
        )
        .expect("proposal serializes");
        assert_eq!(proposal["action"]["type"], "PassPriority");
        submit_public_proposal(&proposal);
        clear_game_state();
    }

    #[test]
    fn public_proposals_do_not_survive_session_supersession_and_reissue_for_controlled_turns() {
        let owner = PlayerId(1);
        let controller = PlayerId(0);
        let mut controlled = priority_state(owner);
        controlled.turn_decision_controller = Some(controller);
        controlled.priority_player = controller;

        let proposal = issue_public_proposal(controlled.clone(), owner);
        assert_eq!(proposal["semanticOwner"], owner.0);
        assert_eq!(proposal["actor"], controller.0);

        // Replacing the live game is a new authority session even when the
        // replacement happens to serialize to the same revision and prompt.
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(controlled)));
        let old_action: GameAction =
            serde_json::from_value(proposal["action"].clone()).expect("old action serializes");
        assert_eq!(
            proposal_outcome(
                proposal["token"].as_str().expect("opaque token"),
                controller,
                &old_action,
            )["status"],
            "stale"
        );

        let reissued: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal("Medium", owner.0)
                .expect("the controlled decision reissues for its semantic owner"),
        )
        .expect("reissued proposal serializes");
        assert_eq!(reissued["semanticOwner"], owner.0);
        assert_eq!(reissued["actor"], controller.0);
        submit_public_proposal(&reissued);
        clear_game_state();
    }

    #[test]
    fn public_proposal_issuer_submits_planeswalker_target_continuation() {
        let player = PlayerId(0);
        let mut state = priority_state(player);
        let walker = install_planeswalker(
            &mut state,
            player,
            2,
            vec![loyalty_ability(
                -1,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                },
            )],
        );
        engine::game::engine::apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: walker,
                ability_index: 0,
            },
        )
        .expect("real loyalty activation must issue a target continuation");
        assert!(matches!(
            issue_and_submit_public_proposal(state, player),
            GameAction::ChooseTarget { .. }
        ));
    }

    #[test]
    fn score_only_workers_require_main_authority_rebinding_before_dispatch() {
        let player = PlayerId(0);
        clear_game_state();
        GAME_STATE.with(|cell| cell.set(Some(priority_state(player))));

        // A worker can return arbitrary serialized data, but a nonmember is
        // discarded before any capability is minted; it has no dispatch path.
        let foreign = serde_json::to_string(&vec![(GameAction::ChooseX { value: 99 }, 99.0)])
            .expect("score tuple serializes");
        assert!(
            get_ai_action_proposal_from_scores(&foreign, "VeryHard", player.0, 7)
                .expect("score rebind handles a foreign score")
                .is_null()
        );
        assert_eq!(
            proposal_outcome(
                "fabricated-worker-token",
                player,
                &GameAction::ChooseX { value: 99 }
            )["status"],
            "stale"
        );

        // The same score becomes actionable only after the live main engine
        // recognizes it as a current exact candidate and mints a new token.
        let valid = serde_json::to_string(&vec![(GameAction::PassPriority, 1.0)])
            .expect("score tuple serializes");
        let proposal: serde_json::Value = serde_wasm_bindgen::from_value(
            get_ai_action_proposal_from_scores(&valid, "VeryHard", player.0, 8)
                .expect("main authority rebind succeeds"),
        )
        .expect("proposal serializes");
        assert_eq!(proposal["semanticOwner"], player.0);
        assert_eq!(proposal["action"]["type"], "PassPriority");
        assert_eq!(
            serde_wasm_bindgen::from_value::<serde_json::Value>(submit_ai_action_proposal(
                proposal["token"].as_str().expect("opaque token"),
                player.0,
                to_js(&proposal["action"]),
            ))
            .expect("proposal result serializes")["status"],
            "applied"
        );
        clear_game_state();
    }

    #[test]
    fn planeswalker_proposals_apply_plus_and_targeted_minus_once_and_with_bounds() {
        let player = PlayerId(0);
        let mut state = priority_state(player);
        let walker = install_planeswalker(
            &mut state,
            player,
            3,
            vec![
                loyalty_ability(
                    1,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
                loyalty_ability(
                    -2,
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Any,
                    },
                ),
            ],
        );

        let initial_contract = install_issued_contract(state.clone(), player);
        let plus = initial_contract
            .candidates
            .iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ActivateAbility { source_id, ability_index: 0 } if source_id == walker
                )
            })
            .expect("real issuer must offer the plus loyalty ability")
            .action
            .clone();
        let token = install_issued_candidate(state.clone(), player, &plus);
        assert_eq!(proposal_outcome(&token, player, &plus)["status"], "applied");

        // The action boundary must not re-offer either loyalty ability after
        // one was activated this turn (CR 606.3).
        let after_plus: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();
        assert!(!install_issued_contract(after_plus, player)
            .candidates
            .iter()
            .any(|candidate| matches!(candidate.action, GameAction::ActivateAbility { source_id, .. } if source_id == walker)));

        let mut minus_state = priority_state(player);
        let minus_walker = install_planeswalker(
            &mut minus_state,
            player,
            2,
            vec![loyalty_ability(
                -2,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                },
            )],
        );
        let minus_contract = install_issued_contract(minus_state.clone(), player);
        let minus = minus_contract
            .candidates
            .iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ActivateAbility { source_id, ability_index: 0 } if source_id == minus_walker
                )
            })
            .expect("real issuer must offer the affordable targeted minus")
            .action
            .clone();
        let token = install_issued_candidate(minus_state, player, &minus);
        assert_eq!(
            proposal_outcome(&token, player, &minus)["status"],
            "applied"
        );
        let target_state: GameState = serde_wasm_bindgen::from_value(get_game_state()).unwrap();
        let target_contract = install_issued_contract(target_state.clone(), player);
        let target = target_contract
            .candidates
            .iter()
            .find(|candidate| matches!(candidate.action, GameAction::ChooseTarget { .. }))
            .expect("targeted loyalty ability must issue bounded target choices")
            .action
            .clone();
        let token = install_issued_candidate(target_state, player, &target);
        assert_eq!(
            proposal_outcome(&token, player, &target)["status"],
            "applied"
        );

        let mut insufficient = priority_state(player);
        let insufficient_walker = install_planeswalker(
            &mut insufficient,
            player,
            1,
            vec![loyalty_ability(
                -2,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );
        assert!(!install_issued_contract(insufficient, player)
            .candidates
            .iter()
            .any(|candidate| matches!(candidate.action, GameAction::ActivateAbility { source_id, .. } if source_id == insufficient_walker)));
        clear_game_state();
    }

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

    fn legacy_ready_state_json() -> String {
        let mut state = GameState::new_two_player(7);
        const EPOCH: u64 = 70_200;
        state.waiting_for = WaitingFor::ResolveAllReady { epoch: EPOCH };
        state.priority_player = PlayerId(0);
        state
            .stack
            .push_back(no_op_stack_entry(70_200, PlayerId(1)));
        state.resolve_all_consent_run = Some(ResolveAllConsentRun {
            epoch: EPOCH,
            max_resolutions: StackResolutionBudget::default(),
            // A table-wide run: both seats are participants and both granted.
            scope: ResolveAllScope::Shared,
            priority_snapshot: ResolveAllPrioritySnapshot {
                waiting_player: PlayerId(0),
                priority_player: PlayerId(0),
                priority_pass_count: 0,
                priority_passes: Default::default(),
            },
            participants: vec![
                ResolveAllConsentParticipant {
                    representative: PlayerId(0),
                    authorized_submitter: PlayerId(0),
                    granted: true,
                },
                ResolveAllConsentParticipant {
                    representative: PlayerId(1),
                    authorized_submitter: PlayerId(1),
                    granted: true,
                },
            ],
            // `None` and the empty live preference map identify the real
            // persisted Ready representation, not a contemporary consent run.
            auto_pass_baseline: None,
        });
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ResolveAllReady { epoch } if epoch == EPOCH
        ));
        assert!(state.auto_pass.is_empty());
        assert!(state
            .resolve_all_consent_run
            .as_ref()
            .is_some_and(|run| run.auto_pass_baseline.is_none()));
        serde_json::to_string(&state).expect("legacy Ready state serializes")
    }

    fn seed_replay_recording() {
        REPLAY_LOG.with(|cell| {
            cell.set(Some(ReplayLog::new(ReplayHeader {
                format_config: FormatConfig::standard(),
                match_config: MatchConfig::default(),
                player_count: 2,
                first_player: Some(0),
                seed: 7,
                deck_data: None,
            })));
        });
        assert!(has_replay_recording());
    }

    fn add_non_mana_recheck_action(state: &mut GameState, controller: PlayerId) {
        let object_id = create_object(
            state,
            CardId(70_100),
            controller,
            "Wasm Recheck Action".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&object_id)
            .expect("created battlefield object");
        object.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
    }

    #[test]
    fn restore_is_decode_only_until_explicit_stack_automation_resume() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();
        let json = legacy_ready_state_json();

        restore_game_state(&json).expect("generic restore installs the snapshot");
        with_state(|state| {
            assert!(matches!(
                state.waiting_for,
                WaitingFor::ResolveAllReady { .. }
            ));
            assert_eq!(state.stack.len(), 1, "restore must not resolve an entry");
        })
        .expect("restored state remains installed");

        let presentation: RestoredStackAutomationPresentation = serde_wasm_bindgen::from_value(
            resume_restored_game_state().expect("explicit resume enters the engine seam"),
        )
        .expect("explicit resume presentation deserializes");
        assert_eq!(
            presentation.outcome,
            RestoredStackAutomationOutcome::Progressed
        );
        with_state(|state| assert!(state.stack.is_empty()))
            .expect("resumed state remains installed");
        clear_game_state();
    }

    #[test]
    fn explicit_resume_is_one_shot_and_revokes_proposals_on_progress() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();
        let json = legacy_ready_state_json();
        restore_game_state(&json).expect("generic restore installs the snapshot");

        let cached_before = with_state(ai_session_for).expect("restored state seeds the AI cache");
        seed_replay_recording();

        let token = AI_PROPOSALS.with(|registry| {
            registry.borrow_mut().insert(AiDecisionContract {
                semantic_owner: PlayerId(0),
                authorized_actor: PlayerId(0),
                state_revision: 0,
                candidates: Vec::new(),
            })
        });
        assert!(AI_PROPOSALS.with(|registry| registry.borrow().proposal(&token).is_some()));
        let generation_before = AI_PROPOSALS.with(|registry| registry.borrow().generation);

        let first = resume_loaded_stack_automation(false).expect("resume progresses once");
        assert_eq!(first.outcome, RestoredStackAutomationOutcome::Progressed);
        assert!(AI_PROPOSALS.with(|registry| registry.borrow().proposal(&token).is_none()));
        assert_eq!(
            AI_PROPOSALS.with(|registry| registry.borrow().generation),
            generation_before.wrapping_add(1),
            "a progressed resume revokes proposal authority exactly once"
        );
        assert!(
            !has_replay_recording(),
            "a progressed resume drops the abandoned replay recording"
        );
        let cached_after = with_state(ai_session_for).expect("resumed state rebuilds the AI cache");
        assert!(
            !Arc::ptr_eq(&cached_before, &cached_after),
            "the resumed state must not retain its pre-resume AI session"
        );

        let second = resume_loaded_stack_automation(false).expect("second resume is harmless");
        assert_eq!(second.outcome, RestoredStackAutomationOutcome::Noop);
        assert_eq!(
            AI_PROPOSALS.with(|registry| registry.borrow().generation),
            generation_before.wrapping_add(1),
            "a no-op local resume must not reset authority again"
        );
        clear_game_state();
    }

    #[test]
    fn multiplayer_host_resume_returns_post_automation_presentation() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();
        let json = legacy_ready_state_json();

        let presentation: RestoredStackAutomationPresentation = serde_wasm_bindgen::from_value(
            resume_multiplayer_host_state(&json).expect("host resume succeeds"),
        )
        .expect("host resume presentation deserializes");
        assert_eq!(
            presentation.outcome,
            RestoredStackAutomationOutcome::Progressed
        );
        assert!(is_multiplayer_mode());
        with_state(|state| assert!(state.stack.is_empty()))
            .expect("post-resume host state remains installed");
        clear_game_state();
        set_multiplayer_mode(false);
    }

    #[test]
    fn multiplayer_host_noop_resume_resets_each_live_authority_store_once() {
        clear_game_state();
        set_multiplayer_mode(false);
        load_minimal_test_card_database();
        let stale_state = GameState::new_two_player(17);
        let cached_before = AI_SESSION_CACHE.with(|cell| {
            let mut cache = cell.take();
            let session = cache.get_or_build(&stale_state);
            cell.set(cache);
            session
        });
        seed_replay_recording();
        let token = AI_PROPOSALS.with(|registry| {
            registry.borrow_mut().insert(AiDecisionContract {
                semantic_owner: PlayerId(0),
                authorized_actor: PlayerId(0),
                state_revision: 0,
                candidates: Vec::new(),
            })
        });
        let generation_before = AI_PROPOSALS.with(|registry| registry.borrow().generation);

        let ordinary = serde_json::to_string(&GameState::new_two_player(23))
            .expect("ordinary host state serializes");
        let presentation: RestoredStackAutomationPresentation = serde_wasm_bindgen::from_value(
            resume_multiplayer_host_state(&ordinary).expect("ordinary host resume succeeds"),
        )
        .expect("host no-op presentation deserializes");

        assert_eq!(presentation.outcome, RestoredStackAutomationOutcome::Noop);
        assert!(AI_PROPOSALS.with(|registry| registry.borrow().proposal(&token).is_none()));
        assert_eq!(
            AI_PROPOSALS.with(|registry| registry.borrow().generation),
            generation_before.wrapping_add(1),
            "a host identity change revokes proposals exactly once even without automation"
        );
        assert!(!has_replay_recording());
        let cached_after = with_state(ai_session_for).expect("host state rebuilds the AI cache");
        assert!(
            !Arc::ptr_eq(&cached_before, &cached_after),
            "a no-op host resume must not inherit a previous session cache"
        );
        clear_game_state();
        set_multiplayer_mode(false);
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
        load_minimal_test_card_database();
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
        load_minimal_test_card_database();

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
        load_minimal_test_card_database();

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
        load_minimal_test_card_database();
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
        load_minimal_test_card_database();

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
                    "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
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

        let result = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "Test Card",
            owner: PlayerId(0),
            zone: engine::types::zones::Zone::Hand,
            count: 2,
            attach_to: None,
            run_etb: true,
            nonlegendary: true,
        })
        .expect("debug create-card should succeed in this fixture");
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    engine::types::events::GameEvent::DebugActionUsed { .. }
                ))
                .count(),
            1,
            "the engine source-bound creator owns the audit event"
        );
        assert_eq!(
            result.log_entries.len(),
            1,
            "the engine source-bound creator resolves the local audit log entry"
        );
        with_state(|state| {
            assert_eq!(
                state
                    .objects
                    .values()
                    .filter(|object| object.name == "Test Card")
                    .count(),
                2,
                "a non-battlefield debug CreateCard batch materializes each card"
            );
            let card = state
                .objects
                .values()
                .find(|object| object.name == "Test Card")
                .expect("debug-created card should exist");
            assert!(!card
                .card_types
                .supertypes
                .contains(&engine::types::card_type::Supertype::Legendary));
            assert!(!card
                .base_card_types
                .supertypes
                .contains(&engine::types::card_type::Supertype::Legendary));
        })
        .expect("game state should remain initialized");

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

    #[test]
    fn debug_create_card_battlefield_batch_uses_the_engine_entry_pipeline() {
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
        CARD_DB.with(|cell| *cell.borrow_mut() = Some(db));

        let mut state = GameState::new_two_player(19);
        state.debug_mode = true;
        GAME_STATE.with(|cell| cell.set(Some(state)));

        let result = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "Test Card",
            owner: PlayerId(0),
            zone: engine::types::zones::Zone::Battlefield,
            count: 2,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
        })
        .expect("a real battlefield debug batch should succeed");

        assert!(matches!(
            result.waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ));
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    engine::types::events::GameEvent::DebugActionUsed { .. }
                ))
                .count(),
            1
        );
        with_state(|state| {
            assert_eq!(
                state
                    .objects
                    .values()
                    .filter(|object| {
                        object.name == "Test Card"
                            && object.zone == engine::types::zones::Zone::Battlefield
                    })
                    .count(),
                2
            );
            assert!(state.resolution_stack.is_empty());
        })
        .expect("game state should remain initialized");

        clear_game_state();
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
    }

    #[test]
    fn debug_create_card_zero_preserves_replay_recording_without_card_database() {
        clear_game_state();
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let mut state = GameState::new_two_player(17);
        state.debug_mode = true;
        let revision = state.state_revision;
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

        let result = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "not loaded",
            owner: PlayerId(0),
            zone: engine::types::zones::Zone::Hand,
            count: 0,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
        })
        .expect("an authorized zero request is a no-op without a card database");
        assert!(result.events.is_empty());
        assert!(has_replay_recording());
        with_state(|state| {
            assert_eq!(state.state_revision, revision);
            assert!(state.objects.is_empty());
        })
        .expect("game state should remain initialized");

        clear_game_state();
    }

    #[test]
    fn debug_create_card_preflight_runs_before_card_database_lookup() {
        clear_game_state();
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let mut state = GameState::new_two_player(23);
        state.debug_mode = true;
        state.waiting_for = WaitingFor::GameOver { winner: None };
        let revision = state.state_revision;
        let public_state_dirty = state.public_state_dirty.clone();
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

        let owner_error = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "not loaded",
            owner: PlayerId(9),
            zone: engine::types::zones::Zone::Hand,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
        })
        .expect_err("an invalid owner must fail before database access");
        assert!(owner_error.contains("invalid owner player id"));
        assert!(!owner_error.contains("database"));

        let priority_error = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "not loaded",
            owner: PlayerId(0),
            zone: engine::types::zones::Zone::Battlefield,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
        })
        .expect_err("a real entry off Priority must fail before database access");
        assert!(priority_error.contains("Priority window"));
        assert!(!priority_error.contains("database"));

        let lookup_error = handle_debug_create_card_inner(DebugCreateCardRequest {
            actor: PlayerId(0),
            card_name: "not loaded",
            owner: PlayerId(0),
            zone: engine::types::zones::Zone::Hand,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
        })
        .expect_err("a missing database must reject a valid nonzero request");
        assert!(lookup_error.contains("card database not loaded"));

        assert!(has_replay_recording());
        with_state(|state| {
            assert_eq!(state.state_revision, revision);
            assert_eq!(state.public_state_dirty, public_state_dirty);
            assert!(state.objects.is_empty());
        })
        .expect("game state should remain initialized");
        clear_game_state();
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
        // directly). Asserts on consumed randomness, not the stored
        // `rng_word_pos` integer.
        //
        // REVERT-PROBES, all four RUN, not reasoned:
        //   * delete `state.capture_rng_word_pos()` in `export_game_state_json`
        //     ⇒ RED. That is the single-deletion discriminator.
        //   * the restore-side rehydration is DOUBLE-COVERED and therefore has
        //     no single-deletion discriminator: `restore_game_state` calls
        //     `rehydrate_rng` itself AND its `decode_restored_game_state` now
        //     routes through `PersistedGameState::into_game_state`, which
        //     rehydrates first. Deleting the bridge's own call ⇒ GREEN;
        //     deleting the chokepoint's ⇒ GREEN; deleting BOTH ⇒ RED.
        // The bridge's own call is thus a harmless idempotent repeat, kept
        // because `rehydrate_rng` is two absolute assignments from persisted
        // fields. Do not read this test as covering it in isolation.
        clear_game_state();
        load_minimal_test_card_database();

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

/// Native coverage for the AI-scoring bridge's per-worker RNG re-seed.
///
/// These are `#[cfg(test)]`, not `#[cfg(all(test, target_arch = "wasm32"))]`: the
/// `wasm32`-gated `mod tests` never executes in the native suite, and no Tilt
/// resource or CI job runs `wasm-pack test`. They drive `scored_candidates_inner`
/// rather than the `#[wasm_bindgen]` shell because the shell returns through
/// `to_js`, which calls the real `JSON.parse` binding and panics outside a wasm32
/// runtime.
///
/// The seam under test: `get_ai_scored_candidates` re-seeds the worker's entropy
/// stream. `rng` is `#[serde(skip)]`, so `rng_seed` + `rng_word_pos` are its only
/// carriers across a snapshot — writing one without the others splits the stream
/// identity in two, and the resulting high-water regression `.expect`-panics in
/// `GameState::capture_rng_word_pos`, which both `export_game_state_json` and
/// every simulated library shuffle perform.
#[cfg(test)]
mod ai_scoring_rng_bridge_tests {
    use super::*;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ResolvedAbility};
    use engine::types::game_state::{StackEntry, StackEntryKind};
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;
    use rand::RngCore;

    /// Carried over verbatim from `server-core`'s `GameSession::from_persisted`
    /// rows: deliberately NOT block-aligned (ChaCha20 block 18, word 3), so a
    /// fast-forward that only lands on block boundaries cannot pass by accident.
    const SAVED_WORD_POS: u128 = 291;
    const ORIGINAL_SEED: u64 = 0x0C0D_5EED;
    const WORKER_SEED: u64 = 0x0C0E_5EED;
    /// Equal seeds would make the C1/C2 rows vacuous. Compile-time, at module
    /// scope, so no row can bypass it by skipping a helper.
    const _: () = assert!(ORIGINAL_SEED != WORKER_SEED);

    /// Steps 1-7 of the fixture: plant the exact state a pool worker is handed.
    /// Deliberately performs **no** scoring call, so the `#[should_panic]` row can
    /// reuse it by omitting a call rather than by reconstructing setup.
    fn plant_restored_worker_state() {
        clear_game_state();

        let mut state = GameState::new_two_player(ORIGINAL_SEED);
        for offset in 0..3u64 {
            engine::game::zones::create_object(
                &mut state,
                CardId(900 + offset),
                PlayerId(0),
                format!("Planted Library Card {offset}"),
                Zone::Library,
            );
        }

        // Premise 1: the planted high-water must be something a re-seed can
        // regress past, or the rows below cannot discriminate.
        assert!(
            state.rng_word_pos < SAVED_WORD_POS,
            "premise: a fresh state must start below the planted high-water"
        );

        // Plant it the way a shuffle does — advance the live stream, then capture
        // it. Never a raw field write.
        state.rng.set_word_pos(SAVED_WORD_POS);
        state.capture_rng_word_pos();

        // Scoreable position. This reproduces `resolve_all_tests::priority_state`'s
        // recipe rather than calling it: that helper is a private `fn`, so a
        // sibling test module cannot name it.
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);

        GAME_STATE.with(|cell| cell.set(Some(state)));

        // Restore REFUSES without a card database (`rehydrate_restored_state_from_card_db`
        // errors on absence alone), and these rows are about the RNG triple, not card
        // data: `rehydrate_game_from_card_db` returns `()` and treats an unknown name as
        // a no-op, so an EMPTY database satisfies the requirement without inventing card
        // rows this module would then have to keep true. `restored_card_db_requirements_tests`
        // is the row that pins the requirement itself.
        CARD_DB.with(|cell| {
            *cell.borrow_mut() = Some(
                engine::database::CardDatabase::from_json_str("{}")
                    .expect("an empty card database must parse"),
            );
        });

        // The exact shipped plant: `AiWorkerPool` calls `worker.restoreState(..)`
        // before every scoring call, and `restore_game_state` rehydrates the full
        // triple.
        let json = export_game_state_json().expect("planting must be exportable");
        clear_game_state();
        // The INNER body, not the `#[wasm_bindgen]` shell: off-wasm32 the shell's
        // error path builds a `JsValue` inside a non-unwinding fn and SIGABRTs, so
        // calling it here would turn any restore failure into an unreadable abort.
        restore_game_state_inner(&json).expect("planting must be restorable");

        // Premise 2, measured: the production restore resumed the saved position,
        // so a zero observed below is this entry point's own policy rather than a
        // lost serde field.
        with_state(|state| {
            assert_eq!(
                state.rng_word_pos, SAVED_WORD_POS,
                "premise: restore must resume the saved high-water"
            );
            assert_eq!(
                state.rng.get_word_pos(),
                state.rng_word_pos,
                "premise: restore must leave the live cursor on the saved high-water"
            );
        })
        .expect("GAME_STATE must be initialized after restore");
    }

    /// Step 8 and nothing else: drive the real scoring path.
    fn drive_scoring() -> Vec<(GameAction, f64)> {
        with_state_mut(|state| {
            scored_candidates_inner(state, AiDifficulty::VeryHard, PlayerId(0), WORKER_SEED)
        })
        .expect("GAME_STATE must be initialized by plant_restored_worker_state")
    }

    /// Row A. Revert-probe (RUN): deleting `state.rng_word_pos = 0;` — or the
    /// whole commit — reds this row with
    /// `HighWaterRegression { current: 291, requested: 0 }`.
    #[test]
    fn scoring_leaves_a_state_that_can_still_export() {
        plant_restored_worker_state();
        drive_scoring();

        // A production entry point on the very worker objects the pool holds:
        // `exportState` is a live `EngineWorkerClient` message type.
        export_game_state_json().expect("a scored worker must still be exportable");

        clear_game_state();
    }

    /// Row B. Same mutant column as Row A by construction — this row buys the
    /// *second* production seam (the route the AI simulation itself takes), not
    /// extra discrimination. It is its own `#[test]` on fresh state because both
    /// seams reach the same `.expect`-ing `capture_rng_word_pos`: sharing a test,
    /// whichever ran first would abort the other.
    #[test]
    fn scoring_leaves_a_state_that_can_still_shuffle() {
        plant_restored_worker_state();
        drive_scoring();

        with_state_mut(|state| {
            assert!(
                !state.players[0].library.is_empty(),
                "reach-guard: the shuffle below must have a library to act on"
            );
            engine::game::library::resolve_and_apply_library_shuffle(
                state,
                PlayerId(0),
                &mut Vec::new(),
            )
            .expect("a scored worker must be able to shuffle");
        })
        .expect("GAME_STATE must be initialized by plant_restored_worker_state");

        clear_game_state();
    }

    /// Row C1. Behavioral (consumed randomness), not a field read, so writing the
    /// field without moving the stream cannot satisfy it.
    ///
    /// Revert-probe (RUN): this row's probe is the **partial** revert — deleting
    /// `state.rng = ..` or all three statements. It is GREEN under the
    /// whole-commit revert, which leaves the live stream at `WORKER_SEED`@0. Do
    /// not read it as whole-commit coverage.
    #[test]
    fn the_caller_supplied_seed_reaches_the_live_stream() {
        plant_restored_worker_state();
        drive_scoring();

        // `score_candidates_for_parallel_worker` takes `&GameState` and `GameState`
        // carries no interior mutability, so nothing at or below the scoring call
        // can advance the live stream: this reads back exactly what the entry
        // point last wrote.
        let mut expected = ChaCha20Rng::seed_from_u64(WORKER_SEED);
        let expected_draws: Vec<u32> = (0..4).map(|_| expected.next_u32()).collect();

        let live_draws: Vec<u32> =
            with_state_mut(|state| (0..4).map(|_| state.rng.next_u32()).collect::<Vec<_>>())
                .expect("GAME_STATE must be initialized by plant_restored_worker_state");

        assert_eq!(
            live_draws, expected_draws,
            "the live stream must be the caller's seed from origin, not the restored snapshot's"
        );

        clear_game_state();
    }

    /// Row C2 — the universal discriminator: RED on every mutant and on the
    /// whole-commit revert. Both C rows compare against a stream freshly built
    /// from `WORKER_SEED`, never against a clone of the post-scoring live stream:
    /// a live-vs-restored comparison only proves internal consistency, which the
    /// "delete all three" mutant also satisfies.
    #[test]
    fn the_scored_triple_round_trips_through_the_bridge() {
        plant_restored_worker_state();
        drive_scoring();

        let mut expected = ChaCha20Rng::seed_from_u64(WORKER_SEED);
        let expected_draws: Vec<u32> = (0..4).map(|_| expected.next_u32()).collect();

        let json = export_game_state_json().expect("a scored worker must still be exportable");
        clear_game_state();
        restore_game_state(&json).expect("a scored worker's export must be restorable");

        let restored_draws: Vec<u32> =
            with_state_mut(|state| (0..4).map(|_| state.rng.next_u32()).collect::<Vec<_>>())
                .expect("GAME_STATE must be initialized after restore");

        assert_eq!(
            restored_draws, expected_draws,
            "the round-tripped stream must be the caller's seed from origin"
        );

        clear_game_state();
    }

    /// Row 2 — the paired reach-guard. It does **not** red when the fix is
    /// reverted and is not meant to: its job is to prove the panic is genuinely
    /// reachable through the production shuffle seam from a triple of exactly this
    /// shape, so the rows above are evidence rather than assertions about a call
    /// that could never have failed.
    ///
    /// It deliberately omits `drive_scoring()` — calling the scoring entry point
    /// first would let a panic from *that* call satisfy the `should_panic`.
    /// Residual, stated rather than engineered away: `#[should_panic]` still
    /// cannot prove which line panicked; the four sibling rows are what detect a
    /// regression in the shared helper.
    #[test]
    #[should_panic(expected = "HighWaterRegression")]
    fn an_incoherent_worker_triple_panics_on_its_next_shuffle() {
        plant_restored_worker_state();

        // Literally the pre-fix line, applied to the restored state.
        with_state_mut(|state| state.rng = ChaCha20Rng::seed_from_u64(WORKER_SEED))
            .expect("GAME_STATE must be initialized by plant_restored_worker_state");

        with_state_mut(|state| {
            engine::game::library::resolve_and_apply_library_shuffle(
                state,
                PlayerId(0),
                &mut Vec::new(),
            )
            .expect("unreachable: the incoherent triple must panic before this");
        })
        .expect("GAME_STATE must be initialized by plant_restored_worker_state");
    }

    /// Row 3's extra fixture shape, applied to the already-restored state between
    /// the plant and the scoring call. Ends by re-asserting the RNG triple is
    /// untouched — object creation must not have moved the stream, or Row 3's
    /// premise is gone.
    fn shape_for_in_call_reach() {
        with_state_mut(|state| {
            state.active_player = PlayerId(0);
            state.priority_passes.clear();

            // The opponent needs a library for the resolved shuffle to act on.
            for offset in 0..3u64 {
                engine::game::zones::create_object(
                    state,
                    CardId(910 + offset),
                    PlayerId(1),
                    format!("Opponent Library Card {offset}"),
                    Zone::Library,
                );
            }

            // Two player-0 battlefield permanents, each carrying one zero-cost
            // activated `Effect::NoOp` ability. Three issued candidates keeps
            // `deterministic_choice`'s `actions.len() == 1` arm from firing.
            for offset in 0..2u64 {
                let id = engine::game::zones::create_object(
                    state,
                    CardId(920 + offset),
                    PlayerId(0),
                    format!("Idle Permanent {offset}"),
                    Zone::Battlefield,
                );
                if let Some(object) = state.objects.get_mut(&id) {
                    object.abilities = Arc::new(vec![AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::NoOp,
                    )]);
                }
            }

            // The stack entry is OPPONENT-controlled: with an AI-owned stack,
            // `low_value_priority_pass_from_actions` computes
            // `owns_entire_stack == true` and `score_candidates_core` returns
            // `[(PassPriority, 1.0)]` before any simulation runs.
            let source_id = engine::game::zones::create_object(
                state,
                CardId(930),
                PlayerId(1),
                "Opponent Shuffle Source".to_string(),
                Zone::Battlefield,
            );
            state.stack = vec![StackEntry {
                id: source_id,
                source_id,
                controller: PlayerId(1),
                kind: StackEntryKind::ActivatedAbility {
                    source_id,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::Shuffle {
                            target: engine::types::ability::TargetFilter::Controller,
                        },
                        vec![],
                        source_id,
                        PlayerId(1),
                    )),
                },
            }]
            .into_iter()
            .collect();

            assert_eq!(
                state.rng_word_pos, SAVED_WORD_POS,
                "premise: shaping the fixture must not move the saved high-water"
            );
            assert_eq!(
                state.rng.get_word_pos(),
                state.rng_word_pos,
                "premise: shaping the fixture must not move the live cursor"
            );
        })
        .expect("GAME_STATE must be initialized by plant_restored_worker_state");
    }

    /// Row 3 — the in-call reach: the panic fires *inside* the scoring call, which
    /// is what makes the shipped symptom (a silently degraded AI via the worker
    /// pool's failure fallback) real rather than a trap for the next caller.
    #[test]
    fn scoring_itself_survives_a_simulated_opponent_shuffle() {
        plant_restored_worker_state();
        shape_for_in_call_reach();

        let issued = with_state_mut(|state| {
            // Measure the list `score_candidates_core` will see, not the one it
            // would have seen a flush ago: `scored_candidates_inner`'s FIRST
            // statement is `flush_layers`, and `score_candidates_core` binds
            // `build_decision_context_for_semantic_owner` downstream of it.
            // `flush_layers` is idempotent (its `mem::replace` leaves the lattice
            // `Clean`, and no arm re-dirties), so `drive_scoring()`'s own flush is
            // a provable no-op and cannot move the candidate set between the two.
            engine::game::layers::flush_layers(state);
            engine::ai_support::build_decision_context_for_semantic_owner(state, PlayerId(0))
                .candidates
                .len()
        })
        .expect("GAME_STATE must be initialized by plant_restored_worker_state");
        assert!(
            issued >= 2,
            "premise: gate #10's `actions.len() == 1` arm must not fire; engine issued {issued} candidates"
        );

        drive_scoring();

        clear_game_state();
    }
}

/// Native coverage for the engine-claim guard. `init_guard` and
/// `claim_engine_for` are plain Rust functions over the two thread-locals, so —
/// unlike the `wasm32`-gated `mod tests`, whose assertions never execute in the
/// native suite — these really run under `cargo test`/nextest. The
/// `#[wasm_bindgen]` shells that call them take and return `JsValue` and cannot
/// run natively; the frontend suite covers that wiring.
///
/// Each case establishes both thread-locals it reads: nextest's
/// process-per-test execution keeps them isolated, and the setup makes each
/// case independent of ordering regardless.
#[cfg(test)]
mod engine_claim_guard_tests {
    use super::*;

    fn install_resident_game() {
        GAME_STATE.with(|cell| cell.set(Some(GameState::new_two_player(0x0C1A_13ED))));
    }

    #[test]
    fn a_multiplayer_host_is_refused_when_the_engine_already_holds_a_game() {
        clear_game_state();
        set_multiplayer_mode(false);
        install_resident_game();

        assert_eq!(
            init_guard(InitSessionKind::MultiplayerHost),
            Err("engine already holds a game"),
            "a hosted game must never overwrite the live local game it shares a worker with"
        );

        clear_game_state();
    }

    #[test]
    fn a_multiplayer_host_may_claim_an_empty_engine() {
        clear_game_state();
        set_multiplayer_mode(false);

        assert_eq!(init_guard(InitSessionKind::MultiplayerHost), Ok(()));
    }

    #[test]
    fn a_local_game_is_refused_while_a_multiplayer_host_owns_the_engine() {
        clear_game_state();
        set_multiplayer_mode(true);

        assert_eq!(
            init_guard(InitSessionKind::Local),
            Err("a multiplayer host session owns this engine"),
            "starting local play on the host's shared worker would destroy the hosted game"
        );

        set_multiplayer_mode(false);
    }

    #[test]
    fn a_local_game_may_still_replace_another_local_game() {
        clear_game_state();
        set_multiplayer_mode(false);
        install_resident_game();

        // The rematch guarantee: a local rematch is a fresh `initialize_game`
        // with no intervening `clear_game_state`, so refusing an occupied
        // engine here would break ordinary single-player play.
        assert_eq!(init_guard(InitSessionKind::Local), Ok(()));

        clear_game_state();
    }

    #[test]
    fn only_a_multiplayer_host_claims_the_engine() {
        clear_game_state();
        set_multiplayer_mode(false);

        claim_engine_for(InitSessionKind::Local);
        assert!(
            !is_multiplayer_mode(),
            "a local game must leave the flag clear, or undo would be refused for the rest of the tab"
        );

        claim_engine_for(InitSessionKind::MultiplayerHost);
        assert!(
            is_multiplayer_mode(),
            "the host claim is what refuses a later local initialize and undo"
        );

        set_multiplayer_mode(false);
    }
}

/// PF2 row 8 — the bridge's per-seat validation loop passes the draft set code
/// at EVERY seat, not only the first two.
///
/// `#[cfg(test)]`, deliberately NOT
/// `#[cfg(all(test, target_arch = "wasm32"))]`: the `wasm32`-gated `mod tests`
/// in this file never executes in the native suite and no CI job runs
/// `wasm-pack test`, so a row placed there would never run at all. These drive
/// `validate_deck_list_seats` — extracted for exactly this reason — rather than
/// `initialize_game_impl`, whose `JsValue` shell returns through `to_js` and
/// panics outside a wasm32 runtime.
///
/// The card database here is fully SYNTHETIC: no name below is a real card, so
/// there is no real-card premise to fabricate and no drift when card data is
/// regenerated. What each face must satisfy is the PRODUCTION predicate
/// `partner_types_for` reads for CR 903.13f(3): legendary creature ("can be a
/// player's commander by itself"), colour identity of one or fewer colours, and
/// no PRINTED partner keyword.
#[cfg(test)]
mod deck_list_seat_validation_tests {
    use super::*;
    use engine::database::CardDatabase;
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType, Supertype};
    use engine::types::mana::ManaColor;
    use std::collections::BTreeMap;

    const LEGEND_A: &str = "Mono Legend A";
    const LEGEND_B: &str = "Mono Legend B";

    fn legendary_creature(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            },
            color_identity: vec![ManaColor::White],
            ..CardFace::default()
        }
    }

    fn plains() -> CardFace {
        CardFace {
            name: "Plains".to_string(),
            card_type: CardType {
                supertypes: vec![Supertype::Basic],
                core_types: vec![CoreType::Land],
                subtypes: vec!["Plains".to_string()],
            },
            color_identity: vec![ManaColor::White],
            ..CardFace::default()
        }
    }

    fn test_db() -> CardDatabase {
        let faces = vec![
            legendary_creature(LEGEND_A),
            legendary_creature(LEGEND_B),
            plains(),
        ];
        let mut entries = BTreeMap::new();
        for f in faces {
            let mut obj = serde_json::to_value(&f).unwrap();
            obj.as_object_mut().unwrap().insert(
                "legalities".to_string(),
                serde_json::json!({ "commander": "legal" }),
            );
            entries.insert(f.name.to_lowercase(), obj);
        }
        CardDatabase::from_json_str(&serde_json::to_string(&entries).unwrap()).unwrap()
    }

    /// CR 903.13f(1): at least 60 cards. Commanders-INSIDE, so
    /// `total_cards == main_deck.len()`.
    fn grant_dependent_seat() -> PlayerDeckList {
        let mut main_deck = vec![LEGEND_A.to_string(), LEGEND_B.to_string()];
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 58));
        PlayerDeckList {
            main_deck,
            commander: vec![LEGEND_A.to_string(), LEGEND_B.to_string()],
            ..Default::default()
        }
    }

    /// Three seats — `player`, `opponent` and one `ai_decks` entry — all
    /// carrying the SAME grant-dependent pair, so a seat the loop skips is a
    /// seat that reds.
    fn three_seat_list(draft_set_codes: &[&str]) -> DeckList {
        DeckList {
            player: grant_dependent_seat(),
            opponent: grant_dependent_seat(),
            ai_decks: vec![grant_dependent_seat()],
            draft_set_codes: draft_set_codes.iter().map(|c| (*c).to_string()).collect(),
            ..Default::default()
        }
    }

    /// REVERT-PROBE for this row: pass `&deck_list.draft_set_codes` in
    /// the `player`/`opponent` loop of `validate_deck_list_seats` but leave the
    /// `ai_decks` loop on `&[]` — the realistic partial-implementation defect.
    /// The first two seats are then accepted and `ai_decks[0]` is refused, so
    /// the returned value is `Some(["AI player 2 deck: Invalid partner
    /// pairing: …"])` instead of `None`.
    #[test]
    fn every_seat_gets_the_draft_set_codes_not_just_the_first_two() {
        let db = test_db();

        // NEGATIVE CONTROL FIRST, and it is the reach guard: without the set
        // code this pair is grant-dependent at EVERY seat, so the fixture is
        // not trivially legal and the `None` below is a real acceptance rather
        // than a loop that never ran. The FIRST reason names the `"Player"`
        // seat, which also pins that the extraction preserved the
        // short-circuit-at-first-refusal shape.
        let refused = validate_deck_list_seats(
            &db,
            &three_seat_list(&[]),
            &FormatConfig::commander_draft(),
            None,
            4,
        )
        .expect("no draft set code: the pair does not pair (CR 702.124)");
        assert!(
            refused[0].starts_with("Player deck:"),
            "expected the Player seat to refuse first, got {refused:?}"
        );
        assert!(
            refused[0].contains("partner"),
            "expected the pairing reason specifically, got {refused:?}"
        );

        // REVERT-FAILING. Asserted as the whole `Option` being `None`: a bare
        // "the ai seat is fine" cannot distinguish "the loop passed the code"
        // from "the loop never ran".
        assert_eq!(
            validate_deck_list_seats(
                &db,
                &three_seat_list(&["CMM"]),
                &FormatConfig::commander_draft(),
                None,
                4,
            ),
            None,
            "CR 903.13f(3): every seat validates under the same grant"
        );
    }

    /// Second hostile fixture — the paired negative for the one branch the
    /// extraction moves. `supplies_fixed_deck()` is `matches!(self,
    /// GameFormat::Momir)`, so `CommanderDraft` enters the block (proven by the
    /// row above) and `Momir` skips it entirely: a fixed-deck format supplies
    /// every seat's deck from the engine, so there is nothing client-side to
    /// validate.
    #[test]
    fn a_fixed_deck_format_skips_seat_validation_entirely() {
        let db = test_db();
        assert_eq!(
            validate_deck_list_seats(&db, &three_seat_list(&[]), &FormatConfig::momir(), None, 4),
            None,
        );
    }
}
