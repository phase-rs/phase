mod admin;
mod data_bootstrap;
mod draft_pools;
mod logging;
mod persistence;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Request, State, WebSocketUpgrade};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use engine::ai_support::{
    auto_pass_recommended_for_viewer as engine_auto_pass_for_viewer,
    end_continuous_effect_offers as engine_end_continuous_effect_offers,
    legal_actions_full as engine_legal_actions_full,
    mana_payment_shortcut_actions as engine_mana_payment_shortcut_actions,
};
use engine::database::CardDatabase;
use engine::game::derived_views::derive_filtered_views;
use engine::game::interaction::{derive_viewer_interaction, object_action_payloads};
use engine::game::validate_name_deck_for_format_full;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::interaction::InteractionSubmission;
use engine::types::player::PlayerId;
use engine::types::GameLogEntry;
use http::{HeaderMap, HeaderValue};
use lobby_broker::{
    check_build_commit, conn_holds_reservation, Broker, BrokerEnv, BuildCommitCheck, ConnState,
    Outbound, NOT_OWNED_RESERVATION,
};
use rand::Rng;
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx};
use server_core::ai_seats_wire_guard::{guard_create_ai_seats, MAX_FULL_GAME_PLAYER_COUNT};
use server_core::client_hello_guard::guard_client_hello;
use server_core::client_message_wire_guard::{
    guard_broker_projection_inbound, guard_client_message_before_dispatch, wire_rejection_message,
};
use server_core::draft_action_payload_guard::guard_draft_action_payload;
use server_core::draft_session::{draft_seats_needing_auto_pick, DraftSessionManager};
use server_core::draft_wire_guard::{
    guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
    guard_reconnect_draft,
};
use server_core::emote_guard::guard_emote;
use server_core::game_action_payload_guard::guard_game_action_payload;
use server_core::game_reconnect_guard::guard_game_reconnect;
use server_core::game_state_snapshot_wire_guard::{
    guard_game_state_for_broadcast, guard_state_snapshot_broadcast, StateSnapshotParts,
    MAX_RESOLVE_ALL_LOG_ENTRIES,
};
use server_core::interaction_payload_guard::guard_interaction_submission_payload;
use server_core::legacy_deck_guard::guard_legacy_deck;
use server_core::legacy_join_guard::guard_legacy_join_game;
use server_core::lobby::RegisterGameRequest;
use server_core::lobby_subscriber_wire_guard::guard_lobby_subscriber_capacity;
use server_core::protocol::{
    build_commit, ClientMessage, RankedPlayerResult, ServerMessage, ServerMode,
    LOBBY_MIN_SUPPORTED_PROTOCOL, LOBBY_PROTOCOL_VERSION, MIN_SUPPORTED_LOBBY_PROTOCOL,
    MIN_SUPPORTED_PROTOCOL, PROTOCOL_VERSION,
};
use server_core::resolve_deck;
use server_core::seat_mutation_wire_guard::guard_seat_mutation;
use server_core::session::{
    ActionResult, FullRuntime, GameSession, RevisionedActionResult, SessionManager,
};
use server_core::spectator_wire_guard::{
    guard_draft_spectator_capacity, guard_game_spectator_capacity, guard_spectate_draft,
    guard_spectator_join,
};
use server_core::takeback::RewindOption;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, info_span, warn, Instrument};
use url::Url;

type SharedState = Arc<Mutex<SessionManager>>;
type SharedConnections =
    Arc<Mutex<HashMap<String, HashMap<PlayerId, mpsc::UnboundedSender<ServerMessage>>>>>;
type SharedDb = Arc<CardDatabase>;
/// The lobby registry, wrapped in the WASM-safe [`Broker`]. LobbyOnly broker
/// dispatch goes through `Broker::handle`/`on_disconnect`/`reap_expired`;
/// Full-mode and draft lobby-listing operations call through
/// `broker.lobby_mut()` (still the same `LobbyManager`, just owned by the
/// broker).
type SharedLobby = Arc<Mutex<Broker>>;
type SharedLobbySubscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
type SharedPlayerCount = Arc<AtomicU32>;
type SharedGameDb = Arc<persistence::GameDb>;
type SharedDraftState = Arc<Mutex<DraftSessionManager>>;
const SPECTATOR_PLAYER_ID: PlayerId = PlayerId(u8::MAX);
/// Stack size for every thread that can run the engine: the runtime *owner*
/// thread spawned in `main`, plus Tokio's worker and blocking threads.
///
/// Rust's default thread stack is 2 MiB and a single WebSocket action already
/// spends most of it: `handle_socket`'s async state machine plus the engine
/// and AI call chain under `run_ai` measured at ~1.35 MiB on a *turn-3*
/// four-player Commander game. `GameState` is moved by value through that
/// chain (`AiActionResult::state`, every `state.clone()`), so the budget is
/// roughly "how many `GameState` values are live on the stack at once" — it is
/// near-constant in board size, which is why an early game overruns it just as
/// readily as a late one. Overrunning is not a catchable panic, so
/// `panic = "unwind"` in `[profile.server-release]` cannot contain it: the
/// process aborts and every player loses the game.
///
/// `GameState`'s inline size has since been cut from 30,112 B to 12,464 B
/// (see `engine/src/types/game_state_size.rs`, which pins it), and 32 MiB is
/// retained anyway, deliberately:
///
///   * the measured high-water does **not** fall in proportion to the struct.
///     On the equivalent bisected fixture the struct shrank 2.42x while the
///     stack high-water fell only ~1.36x. The residual is **unattributed** — it
///     was not instrumented, so treat what follows as the leading candidate,
///     not a finding. Boxing covered every `ResolvedAbility` *storage* site but
///     none of the by-value *parameter* sites (**41 production-reachable**; 48
///     in `crates/` total, of which 7 are test-only, and 13 of the 48 are in
///     `engine/src/game/casting_costs.rs`; population and counting method are
///     stated in `engine/tests/integration/game_state_stack_budget.rs`, and
///     both figures are lower bounds because grep undercounts this shape). The
///     production figure is the relevant one here: this is a claim about
///     production stack frames, and a test-only parameter never appears in
///     one. Those nest two
///     deep on the ordinary cast path, so part of the residual plausibly still
///     scales with `ResolvedAbility`. Either way, no static size fix is proven
///     to bound it;
///   * AI search depth is data-driven, so no static size fix bounds
///     `depth x chain_depth x sizeof`;
///   * `[profile.server-release]` (`opt-level = 2`, `lto = "thin"`,
///     `codegen-units = 16`) uses measurably more stack than `ai_commander`'s
///     profile;
///   * the cost is reserved *address space*, not committed memory. Note the
///     multiplier: `thread_stack_size` also sizes Tokio's **blocking** pool,
///     whose default cap is 512 threads, so the worst-case reservation for that
///     pool goes from ~1 GiB to ~16 GiB. Blocking threads are spawned on demand
///     and 512 is a cap rather than a steady state, and on 64-bit this is
///     address space only — but if this server ever runs somewhere with strict
///     VA-commit accounting, `max_blocking_threads` is the knob to reach for.
///
/// 32 MiB matches what `ai_commander` and `duel_suite` already use for this
/// same engine recursion.
///
/// Side effect worth knowing: a **debug** `phase-server` used to abort on any
/// WebSocket connect, because a debug frame chain did not fit Tokio's 2 MiB
/// default worker stack. Sizing the owner and worker threads here fixed that, so
/// the debug binary is now a usable smoke target — `cargo run -p phase-server`,
/// connect a client, and the handshake plus lobby path complete without an
/// abort. Verified once by hand after this constant landed; if you are looking
/// for a cheap end-to-end check of a server change, that is now available.
const RUNTIME_THREAD_STACK_BYTES: usize = 32 * 1024 * 1024;
type SharedDraftPools = Arc<draft_pools::DraftPools>;
/// Spectator senders keyed by draft_code. Each spectator has a visibility + sender.
type SharedDraftSpectators = Arc<
    Mutex<
        HashMap<
            String,
            Vec<(
                draft_core::types::SpectatorVisibility,
                mpsc::UnboundedSender<ServerMessage>,
            )>,
        >,
    >,
>;
/// Spectator senders keyed by game code (live games only).
type SharedGameSpectators = Arc<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<ServerMessage>>>>>;

/// Deserializing a persisted session is deeply nested and stack-hungry — and
/// boxing a field does not help here, because `Box<T>::deserialize` still
/// builds `T` on the stack before moving it into the allocation. It needs a
/// large stack, and it now has one without a platform fork: the sole caller
/// runs inside `serve()`, which `main` drives on the `phase-server-runtime`
/// thread at `RUNTIME_THREAD_STACK_BYTES`. The former `#[cfg(windows)]` arm
/// hopped onto a purpose-sized 16 MiB thread; against a 32 MiB runtime owner
/// that is a *downgrade* on the one platform that reported the overflow, so
/// both arms are gone and the restore runs inline.
fn restore_persisted_session(json: &str, db: SharedDb) -> Result<GameSession, String> {
    let persisted = serde_json::from_str::<server_core::PersistedSession>(json)
        .map_err(|error| error.to_string())?;
    GameSession::from_persisted(persisted, db.as_ref())
}

async fn reserve_lobby_subscriber_slot(
    lobby_subscribers: &SharedLobbySubscribers,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<(), String> {
    let mut subs = lobby_subscribers.lock().await;
    subs.retain(|sender| !sender.is_closed());

    if subs.iter().any(|sender| sender.same_channel(tx)) {
        return Ok(());
    }

    guard_lobby_subscriber_capacity(subs.len())?;
    subs.push(tx.clone());
    Ok(())
}

async fn remove_game_spectator_sender(
    game_spectators: &SharedGameSpectators,
    game_code: &str,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let mut specs = game_spectators.lock().await;
    if let Some(spectators) = specs.get_mut(game_code) {
        spectators.retain(|sender| !sender.same_channel(tx) && !sender.is_closed());
        if spectators.is_empty() {
            specs.remove(game_code);
        }
    }
}

async fn reserve_game_spectator_slot(
    game_spectators: &SharedGameSpectators,
    game_code: &str,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<(), String> {
    let mut specs = game_spectators.lock().await;
    let spectators = specs.entry(game_code.to_string()).or_default();
    spectators.retain(|sender| !sender.is_closed());

    if spectators.iter().any(|sender| sender.same_channel(tx)) {
        return Ok(());
    }

    guard_game_spectator_capacity(spectators.len())?;
    spectators.push(tx.clone());
    Ok(())
}

async fn switch_game_spectator_slot(
    game_spectators: &SharedGameSpectators,
    previous_game_code: Option<&str>,
    game_code: &str,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<(), String> {
    reserve_game_spectator_slot(game_spectators, game_code, tx).await?;
    if previous_game_code != Some(game_code) {
        if let Some(previous_game_code) = previous_game_code {
            remove_game_spectator_sender(game_spectators, previous_game_code, tx).await;
        }
    }
    Ok(())
}

async fn prune_game_connections<'a>(
    connections: &SharedConnections,
    game_codes: impl IntoIterator<Item = &'a str>,
) {
    let mut conns = connections.lock().await;
    for game_code in game_codes {
        conns.remove(game_code);
    }
}

async fn remove_draft_spectator_sender(
    draft_spectators: &SharedDraftSpectators,
    draft_code: &str,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let mut specs = draft_spectators.lock().await;
    if let Some(spectators) = specs.get_mut(draft_code) {
        spectators.retain(|(_, sender)| !sender.same_channel(tx) && !sender.is_closed());
        if spectators.is_empty() {
            specs.remove(draft_code);
        }
    }
}

async fn reserve_draft_spectator_slot(
    draft_spectators: &SharedDraftSpectators,
    draft_code: &str,
    visibility: draft_core::types::SpectatorVisibility,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<(), String> {
    let mut specs = draft_spectators.lock().await;
    let spectators = specs.entry(draft_code.to_string()).or_default();
    spectators.retain(|(_, sender)| !sender.is_closed());

    if let Some((existing_visibility, _)) = spectators
        .iter_mut()
        .find(|(_, sender)| sender.same_channel(tx))
    {
        *existing_visibility = visibility;
        return Ok(());
    }

    guard_draft_spectator_capacity(spectators.len())?;
    spectators.push((visibility, tx.clone()));
    Ok(())
}

async fn switch_draft_spectator_slot(
    draft_spectators: &SharedDraftSpectators,
    previous_draft_code: Option<&str>,
    draft_code: &str,
    visibility: draft_core::types::SpectatorVisibility,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<(), String> {
    reserve_draft_spectator_slot(draft_spectators, draft_code, visibility, tx).await?;
    if previous_draft_code != Some(draft_code) {
        if let Some(previous_draft_code) = previous_draft_code {
            remove_draft_spectator_sender(draft_spectators, previous_draft_code, tx).await;
        }
    }
    Ok(())
}

/// Derive presentation state for any server transport after viewer filtering.
/// Rules authority must always come from the pre-filter snapshot: search-control
/// provenance is intentionally absent from some viewer-safe states.
fn derive_transport_views(
    authoritative_state: &GameState,
    filtered_state: &GameState,
    viewer: Option<PlayerId>,
) -> engine::game::derived_views::DerivedViews {
    derive_filtered_views(authoritative_state, filtered_state, viewer)
}

/// Build the `GameStarted` message for a single seat.
///
/// `events` carries the engine's start-of-game events (the d20 first-player
/// contest's `StartingPlayerContest` event). Only the INITIAL post-start
/// fan-out (`build_game_started_messages`) passes a non-empty batch; late
/// joiners and reconnects pass an empty `Vec` so they never re-see the contest.
/// The events go to every seat unchanged — the contest is public (no
/// `visibility.rs` redaction), so this deliberately does NOT apply the
/// `is_actor` gating used for `legal_actions`.
fn build_game_started_message(
    session: &GameSession,
    player: PlayerId,
    player_token: Option<String>,
    events: Vec<GameEvent>,
) -> ServerMessage {
    let (legal_actions, spell_costs_all, by_object_all) = engine_legal_actions_full(&session.state);
    let is_actor = server_core::is_acting(&session.state, player);
    let auto_pass = engine_auto_pass_for_viewer(&session.state, player, &legal_actions);
    let end_continuous_effect_offers = if is_actor {
        engine_end_continuous_effect_offers(&legal_actions)
    } else {
        Vec::new()
    };
    let mana_payment_shortcut_actions = if is_actor {
        engine_mana_payment_shortcut_actions(&session.state, &by_object_all)
    } else {
        Vec::new()
    };
    let filtered = server_core::filter_state_for_player(&session.state, player);
    let opponent_name = engine::game::players::opponents(&session.state, player)
        .first()
        .and_then(|opp| {
            let name = &session.display_names[opp.0 as usize];
            if name.is_empty() {
                None
            } else {
                Some(name.clone())
            }
        });
    let derived = derive_transport_views(&session.state, &filtered, Some(player));
    let viewer_interaction = derive_viewer_interaction(&session.state, &filtered, player);

    ServerMessage::GameStarted {
        state_revision: session.state_revision,
        state: filtered,
        your_player: player,
        opponent_name,
        player_names: session.display_names.clone(),
        legal_actions: if is_actor { legal_actions } else { Vec::new() },
        auto_pass_recommended: auto_pass,
        end_continuous_effect_offers,
        mana_payment_shortcut_actions,
        spell_costs: if is_actor {
            spell_costs_all
        } else {
            HashMap::new()
        },
        legal_actions_by_object: if is_actor {
            object_action_payloads(&by_object_all)
        } else {
            HashMap::new()
        },
        derived,
        viewer_interaction,
        player_token,
        full_key: session
            .full_runtime
            .as_ref()
            .map(|runtime| runtime.key.clone()),
        events: server_core::filter_events_for_player(&events, &session.state, player),
        // Read from the session rather than taken as a parameter: every caller
        // already hands this function the authoritative session, and there is
        // no caller that should publish anything else. A parameter every site
        // fills identically from an argument it already passes is a hazard,
        // not a choice. Populating `GameStarted` (not just `StateUpdate`) is
        // what makes a reconnect mid-game see the list immediately.
        rewind_targets: session.rewind_options(),
    }
}

/// Initial post-start fan-out. DRAINS `session.start_events` so the first-player
/// contest is sent exactly once — every subsequent `GameStarted` build
/// (late joiners, reconnects) sees an empty batch and never re-shows the
/// contest. Every seat receives the contest event (public; not actor-gated).
fn build_game_started_messages(session: &mut GameSession) -> Vec<(PlayerId, ServerMessage)> {
    let start_events = std::mem::take(&mut session.start_events);
    (0..session.player_count)
        .map(PlayerId)
        .filter(|player| !session.ai_seats.contains(player))
        .map(|player| {
            (
                player,
                build_game_started_message(session, player, None, start_events.clone()),
            )
        })
        .collect()
}

/// `rewind_targets` is a parameter here, unlike in
/// `build_game_started_message`, because this builder has no `GameSession` to
/// read it from — the caller captures `session.rewind_options()` under the same
/// lock as the transition and threads it through.
fn build_state_update_message(
    result: &ActionResult,
    state_revision: u64,
    player: PlayerId,
    rewind_targets: Vec<RewindOption>,
) -> Result<ServerMessage, String> {
    let (
        raw_state,
        events,
        legal_actions,
        log_entries,
        _auto_pass,
        spell_costs,
        legal_actions_by_object,
    ) = result;
    guard_state_snapshot_broadcast(StateSnapshotParts {
        state: raw_state,
        events,
        log_entries,
        legal_actions,
        legal_actions_by_object,
        spell_costs,
    })?;
    let is_actor = server_core::is_acting(raw_state, player);
    let filtered = server_core::filter_state_for_player(raw_state, player);
    let derived = derive_transport_views(raw_state, &filtered, Some(player));
    let viewer_interaction = derive_viewer_interaction(raw_state, &filtered, player);
    let mana_payment_shortcut_actions = if is_actor {
        engine_mana_payment_shortcut_actions(raw_state, legal_actions_by_object)
    } else {
        Vec::new()
    };
    let end_continuous_effect_offers = if is_actor {
        engine_end_continuous_effect_offers(legal_actions)
    } else {
        Vec::new()
    };

    Ok(ServerMessage::StateUpdate {
        state_revision,
        state: filtered,
        events: server_core::filter_events_for_player(events, raw_state, player),
        legal_actions: if is_actor {
            legal_actions.clone()
        } else {
            Vec::new()
        },
        auto_pass_recommended: engine_auto_pass_for_viewer(raw_state, player, legal_actions),
        end_continuous_effect_offers,
        mana_payment_shortcut_actions,
        eliminated_players: Vec::new(),
        log_entries: log_entries.clone(),
        spell_costs: if is_actor {
            spell_costs.clone()
        } else {
            HashMap::new()
        },
        legal_actions_by_object: if is_actor {
            object_action_payloads(legal_actions_by_object)
        } else {
            HashMap::new()
        },
        derived,
        viewer_interaction,
        rewind_targets,
    })
}

/// Resolving the batch and then resuming normal AI play are one authoritative
/// transition. Retain their engine-authored logs in that order while keeping
/// the compact final snapshot bounded by
/// [`MAX_RESOLVE_ALL_LOG_ENTRIES`], which `server-core` also applies to a batch
/// its own AI hand-off collapses.
fn resolve_all_log_tail(
    batch_log_entries: &[GameLogEntry],
    ai_results: &[RevisionedActionResult],
) -> Vec<GameLogEntry> {
    fn append_tail(tail: &mut Vec<GameLogEntry>, entries: &[GameLogEntry]) {
        if entries.len() >= MAX_RESOLVE_ALL_LOG_ENTRIES {
            tail.clear();
            tail.extend_from_slice(&entries[entries.len() - MAX_RESOLVE_ALL_LOG_ENTRIES..]);
            return;
        }

        let overflow = tail
            .len()
            .saturating_add(entries.len())
            .saturating_sub(MAX_RESOLVE_ALL_LOG_ENTRIES);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(entries);
    }

    let mut tail = Vec::with_capacity(MAX_RESOLVE_ALL_LOG_ENTRIES);
    append_tail(&mut tail, batch_log_entries);
    for (_, (_, _, _, log_entries, _, _, _)) in ai_results {
        append_tail(&mut tail, log_entries);
    }
    tail
}

#[allow(clippy::too_many_arguments)]
fn build_resolve_all_state_update_message(
    raw_state: &GameState,
    log_entries: &[GameLogEntry],
    legal_actions: &[GameAction],
    spell_costs: &HashMap<engine::types::identifiers::ObjectId, engine::types::mana::ManaCost>,
    legal_actions_by_object: &HashMap<engine::types::identifiers::ObjectId, Vec<GameAction>>,
    state_revision: u64,
    player: PlayerId,
    eliminated_players: Vec<PlayerId>,
    rewind_targets: Vec<RewindOption>,
) -> ServerMessage {
    let is_actor = server_core::is_acting(raw_state, player);
    let filtered = server_core::filter_state_for_player(raw_state, player);
    let end_continuous_effect_offers = if is_actor {
        engine_end_continuous_effect_offers(legal_actions)
    } else {
        Vec::new()
    };
    let mana_payment_shortcut_actions = if is_actor {
        engine_mana_payment_shortcut_actions(raw_state, legal_actions_by_object)
    } else {
        Vec::new()
    };

    ServerMessage::StateUpdate {
        state_revision,
        state: filtered.clone(),
        events: Vec::new(),
        legal_actions: if is_actor {
            legal_actions.to_vec()
        } else {
            Vec::new()
        },
        auto_pass_recommended: engine_auto_pass_for_viewer(raw_state, player, legal_actions),
        end_continuous_effect_offers,
        mana_payment_shortcut_actions,
        eliminated_players,
        log_entries: log_entries.to_vec(),
        spell_costs: if is_actor {
            spell_costs.clone()
        } else {
            HashMap::new()
        },
        legal_actions_by_object: if is_actor {
            object_action_payloads(legal_actions_by_object)
        } else {
            HashMap::new()
        },
        derived: derive_transport_views(raw_state, &filtered, Some(player)),
        viewer_interaction: derive_viewer_interaction(raw_state, &filtered, player),
        rewind_targets,
    }
}

/// Build the public spectator view for an in-progress game.
///
/// Spectators are modeled as a non-seat viewer (`PlayerId(u8::MAX)`), which
/// keeps all seat-private data redacted and guarantees no legal-action payload.
fn build_spectator_game_started_message(session: &GameSession) -> Result<ServerMessage, String> {
    guard_game_state_for_broadcast(&session.state)?;
    let filtered = server_core::filter_state_for_player(&session.state, SPECTATOR_PLAYER_ID);
    let derived = derive_transport_views(&session.state, &filtered, None);
    let viewer_interaction =
        derive_viewer_interaction(&session.state, &filtered, SPECTATOR_PLAYER_ID);

    Ok(ServerMessage::GameStarted {
        state_revision: session.state_revision,
        state: filtered,
        your_player: SPECTATOR_PLAYER_ID,
        opponent_name: None,
        player_names: session.display_names.clone(),
        legal_actions: Vec::new(),
        auto_pass_recommended: false,
        end_continuous_effect_offers: Vec::new(),
        mana_payment_shortcut_actions: Vec::new(),
        spell_costs: HashMap::new(),
        legal_actions_by_object: HashMap::new(),
        derived,
        viewer_interaction,
        player_token: None,
        full_key: session
            .full_runtime
            .as_ref()
            .map(|runtime| runtime.key.clone()),
        events: Vec::new(),
        // Always empty for spectators, deliberately — NOT `rewind_options()`.
        // A spectator is a read-only viewer with no rollback affordance, and
        // the list would only advertise targets they cannot request.
        rewind_targets: Vec::new(),
    })
}

fn build_spectator_state_update_message(
    raw_state: &GameState,
    events: &[GameEvent],
    log_entries: &[GameLogEntry],
    state_revision: u64,
) -> Result<ServerMessage, String> {
    guard_state_snapshot_broadcast(StateSnapshotParts {
        state: raw_state,
        events,
        log_entries,
        legal_actions: &[],
        legal_actions_by_object: &HashMap::new(),
        spell_costs: &HashMap::new(),
    })?;
    let filtered = server_core::filter_state_for_player(raw_state, SPECTATOR_PLAYER_ID);
    let derived = derive_transport_views(raw_state, &filtered, None);
    let viewer_interaction = derive_viewer_interaction(raw_state, &filtered, SPECTATOR_PLAYER_ID);
    let eliminated_players = raw_state.eliminated_players.clone();

    Ok(ServerMessage::StateUpdate {
        state_revision,
        state: filtered,
        events: server_core::filter_events_for_player(events, raw_state, SPECTATOR_PLAYER_ID),
        legal_actions: Vec::new(),
        auto_pass_recommended: false,
        end_continuous_effect_offers: Vec::new(),
        mana_payment_shortcut_actions: Vec::new(),
        eliminated_players,
        log_entries: log_entries.to_vec(),
        spell_costs: HashMap::new(),
        legal_actions_by_object: HashMap::new(),
        derived,
        viewer_interaction,
        // Empty for the same reason as the spectator `GameStarted` builder
        // above: a spectator has no rollback affordance. This builder also
        // takes a raw state rather than a session, so `rewind_options()` is
        // not even in scope here.
        rewind_targets: Vec::new(),
    })
}

/// Server's advertised role, selected at startup via `--lobby-only`. Copied
/// into every handler so the dispatch path can gate disabled messages in
/// lobby-only mode without re-parsing CLI state.
type Mode = ServerMode;

/// Server-wide limits to prevent resource exhaustion and abuse.
const MAX_CONNECTIONS: u32 = 200;
const MAX_GAMES: usize = 100;
// The lobby-only broker capacity cap (`MAX_LOBBY_ENTRIES`) now lives in
// `lobby_broker::broker` — the broker enforces it inside `handle`.
const RATE_LIMIT_MESSAGES: u32 = 30;
const RATE_LIMIT_WINDOW_SECS: u64 = 1;
// A native Play-vs-AI setup carries the host deck and every AI deck in one
// CreateGameWithSettings frame. Keep a bounded transport limit while allowing
// a full multiplayer table's ordinary deck lists to reach the input guards.
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024; // 64 KB

/// Native [`BrokerEnv`] implementation: wall clock via `SystemTime`, tokens /
/// codes via the `server_core` generators (which stay in `server-core` — they
/// are the native randomness source and must not move into the WASM leaf).
struct SysEnv;

impl BrokerEnv for SysEnv {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    fn new_token(&self) -> String {
        server_core::generate_player_token()
    }
    fn new_game_code(&self) -> String {
        server_core::generate_game_code()
    }
}

/// Simple per-socket token bucket rate limiter.
struct RateLimiter {
    count: u32,
    window_start: Instant,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            count: 0,
            window_start: Instant::now(),
        }
    }

    /// Returns `true` if the message is allowed, `false` if rate-limited.
    fn check(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start).as_secs() >= RATE_LIMIT_WINDOW_SECS {
            self.count = 0;
            self.window_start = now;
        }
        self.count += 1;
        self.count <= RATE_LIMIT_MESSAGES
    }
}

/// phase-server: multiplayer game server for phase.rs
#[derive(Parser)]
#[command(
    name = "phase-server",
    version,
    about = "Multiplayer game server for phase.rs"
)]
struct Cli {
    /// Port to listen on
    #[arg(short, long, default_value = "9374", env = "PORT")]
    port: u16,

    /// Address to bind. Defaults to all interfaces for LAN and tunnel hosting.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// Exit cleanly when stdin closes. Used by the desktop shell so an orphaned
    /// native server terminates after its parent process dies.
    #[arg(long)]
    exit_on_stdin_close: bool,

    /// Accept WebSocket handshakes only from this Origin when one is supplied.
    /// Clients without an Origin header remain accepted for self-hosted tooling.
    #[arg(long)]
    allowed_origin: Option<String>,

    /// Path to card data directory (must contain card-data.json)
    #[arg(short, long, default_value = "data", env = "PHASE_DATA_DIR")]
    data_dir: PathBuf,

    /// Path to the SQLite game-persistence database. Defaults to
    /// `<data_dir>/games.db`. The desktop shell points this at a
    /// version-independent location so saved games survive native-engine
    /// updates — the versioned `data_dir` is recreated per engine version, so a
    /// games.db living inside it would be orphaned on every update.
    #[arg(long, env = "PHASE_GAMES_DB")]
    games_db: Option<PathBuf>,

    /// Single-user local instance (the desktop shell). There is no seat
    /// contention to reclaim here, so the two online-tuned session policies do
    /// not apply: persisted sessions are never stale-purged, and reconnects
    /// never expire. Together these let a suspended solo game stay resumable
    /// until the player starts a new one.
    #[arg(long, env = "PHASE_SINGLE_USER")]
    single_user: bool,

    /// Signed data-manifest URL for bootstrapping a missing PHASE_DATA_DIR.
    /// This overrides the manifest resolved from the binary's embedded channel.
    #[arg(long, env = "PHASE_DATA_MANIFEST_URL")]
    data_manifest_url: Option<Url>,

    /// Refuse to download missing startup data. Intended for air-gapped hosts
    /// with a pre-provisioned PHASE_DATA_DIR.
    #[arg(long)]
    no_data_download: bool,

    /// Allowed CORS origin (use '*' for permissive, or a specific URL)
    #[arg(long, env = "PHASE_CORS_ORIGIN")]
    cors_origin: Option<String>,

    /// Emit logs as JSON (for production log aggregation)
    #[arg(long, env = "PHASE_LOG_JSON")]
    log_json: bool,

    /// Directory for log files. When set, logs to files instead of stdout.
    /// Main log: <dir>/phase-server.log, per-game logs: <dir>/games/<code>.log
    #[arg(long, env = "PHASE_LOG_DIR")]
    log_dir: Option<String>,

    /// Run as a lobby-only matchmaking broker for P2P games. In this mode
    /// the server rejects game-state messages (CreateGame, Action, Reconnect,
    /// Concede, Emote, SpectatorJoin) and only brokers PeerJS peer IDs via
    /// CreateGameWithSettings / JoinGameWithPassword / SubscribeLobby. The
    /// engine and game state never run server-side, eliminating engine/build
    /// drift between host and server.
    #[arg(long, env = "PHASE_LOBBY_ONLY")]
    lobby_only: bool,

    /// Public base URL to advertise to clients for sharing join codes (e.g.
    /// `https://play.example.com` when running behind a TLS reverse proxy or
    /// tunnel). Clients surface `<code>@<host>` so friends can join without the
    /// host reading server logs. When the `ngrok` feature is built and
    /// `NGROK_AUTHTOKEN` is set, the live tunnel URL is used when this is unset.
    #[arg(long, env = "PUBLIC_URL")]
    public_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CardDataSource {
    Export(PathBuf),
    DevFixture(PathBuf),
}

fn dev_fixture_enabled() -> bool {
    matches!(std::env::var("PHASE_DEV_FIXTURE"), Ok(value) if value == "1")
}

fn select_card_data_source(data_dir: &Path, dev_fixture: bool) -> Result<CardDataSource, String> {
    let export_path = data_dir.join("card-data.json");
    if export_path.is_file() {
        return Ok(CardDataSource::Export(export_path));
    }
    if dev_fixture {
        return Ok(CardDataSource::DevFixture(
            data_dir.join("mtgjson/test_fixture.json"),
        ));
    }
    Err(format!(
        "card-data.json is missing from {}; startup data bootstrap did not provide it",
        data_dir.display()
    ))
}

fn bootstrap_required(data_dir: &Path, dev_fixture: bool) -> bool {
    !dev_fixture || data_dir.join("card-data.json").is_file()
}

fn fatal_startup(message: impl std::fmt::Display) -> ! {
    eprintln!("phase-server startup failed: {message}");
    std::process::exit(1);
}

/// Per-socket state tracking which game/player this connection belongs to.
struct SocketIdentity {
    game_code: Option<String>,
    player_id: Option<PlayerId>,
    player_token: Option<String>,
    lobby_subscribed: bool,
    /// Span for field inheritance — all events within this connection inherit game + player fields.
    session_span: Option<tracing::Span>,
    /// Set after a successful `ClientHello`. Until this is `Some`, only
    /// `ClientMessage::ClientHello` is accepted. Carries the client's build
    /// identity so downstream handlers (`CreateGameWithSettings`,
    /// `JoinGameWithPassword`) can stamp / compare against host builds.
    client_hello: Option<ClientHelloInfo>,
    /// Set in lobby-only mode when this socket registered a lobby entry as
    /// host. On disconnect (or explicit `UnregisterLobby`) the server drops
    /// the matching lobby entry so abandoned rooms don't linger until the
    /// 5-minute expiry. Empty in `Full` mode (handled via `game_code` +
    /// `SessionManager` cleanup).
    lobby_host_game: Option<String>,
    seat_reservations: Vec<(String, String)>,
    lobby_reservations: Vec<(String, String)>,
    /// Set when this socket is participating in a draft session.
    draft_code: Option<String>,
    draft_seat: Option<usize>,
    draft_token: Option<String>,
    /// Set when this socket is spectating a draft (T-60-09: action handler
    /// checks draft_seat.is_some() before processing, rejecting spectators).
    spectator_draft_code: Option<String>,
    spectator_visibility: Option<draft_core::types::SpectatorVisibility>,
    /// Set when this socket is spectating a live game. Kept separate from
    /// `game_code`/`player_id` so spectator sockets remain read-only.
    spectator_game_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientHelloInfo {
    client_version: String,
    build_commit: String,
}

/// Outcome of evaluating the handshake gate against an incoming message.
/// Extracted into a pure function so the gate's invariants can be unit-tested
/// without spinning up a real WebSocket.
#[derive(Debug, PartialEq, Eq)]
enum HelloGateOutcome {
    /// First ClientHello on this socket, compatible protocol — store the info
    /// and continue the message loop (no further processing for this frame).
    Accept(ClientHelloInfo),
    /// ClientHello arrived but declares an incompatible protocol version.
    /// Send Error with this (client, server) pair and drop the frame.
    RejectProtocol { client: u32, server: u32 },
    /// ClientHello fields failed wire validation. Send Error with this reason.
    RejectInvalidHello(String),
    /// A non-hello frame arrived before the handshake completed. Send Error
    /// ("ClientHello required before any other message") and drop.
    RejectHandshakeRequired,
    /// Handshake already completed and another ClientHello arrived. Ignore
    /// silently — this is a harmless misbehavior, not an error.
    IgnoreRedundantHello,
    /// Handshake already completed and a regular frame arrived — let the
    /// downstream match in `handle_client_message` handle it.
    PassThrough,
}

/// Which protocol surface a server gates its handshake on.
///
/// A bare `RangeInclusive<u32>` could only express "between X and Y on
/// `protocol_version`". The lobby needs a different shape entirely — a floor,
/// no ceiling, read off a *different* wire field — so the policy is a type
/// rather than a range.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HelloAcceptance {
    /// Full-game surface: `protocol_version` must land in this inclusive range.
    /// Both ends matter — `GameState` and `GameAction` payloads are not
    /// forward- or backward-compatible across a bump.
    FullGame(std::ops::RangeInclusive<u32>),
    /// Lobby surface. Gates on the client's `lobby_protocol_version` against
    /// `lobby_floor` with **no ceiling**: a client newer than this broker can
    /// only fail by sending a lobby variant the broker does not know, which
    /// `parse_lobby_client_message` already rejects per-frame as an unknown
    /// tag. Clients that predate the field fall back to `legacy_range` on
    /// `protocol_version`, preserving the pre-existing behavior exactly.
    Lobby {
        lobby_floor: u32,
        legacy_range: std::ops::RangeInclusive<u32>,
    },
}

impl HelloAcceptance {
    /// `None` when the hello is acceptable; `Some((client, server))` naming the
    /// two versions to report when it is not.
    fn reject(
        &self,
        protocol_version: u32,
        lobby_protocol_version: Option<u32>,
    ) -> Option<(u32, u32)> {
        match self {
            Self::FullGame(range) => {
                (!range.contains(&protocol_version)).then(|| (protocol_version, *range.end()))
            }
            Self::Lobby {
                lobby_floor,
                legacy_range,
            } => match lobby_protocol_version {
                Some(client_lobby) => {
                    (client_lobby < *lobby_floor).then_some((client_lobby, LOBBY_PROTOCOL_VERSION))
                }
                None => (!legacy_range.contains(&protocol_version))
                    .then(|| (protocol_version, *legacy_range.end())),
            },
        }
    }
}

fn classify_hello_gate(
    hello_received: bool,
    msg: &ClientMessage,
    acceptance: HelloAcceptance,
) -> HelloGateOutcome {
    match (hello_received, msg) {
        (
            false,
            ClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version,
                lobby_protocol_version,
            },
        ) => {
            // The `server` field on RejectProtocol surfaces the version this
            // server speaks on whichever surface it gated, so the error message
            // tells the client what to upgrade (or downgrade) to.
            if let Some((client, server)) =
                acceptance.reject(*protocol_version, *lobby_protocol_version)
            {
                HelloGateOutcome::RejectProtocol { client, server }
            } else if let Err(reason) = guard_client_hello(client_version, build_commit) {
                HelloGateOutcome::RejectInvalidHello(reason)
            } else {
                HelloGateOutcome::Accept(ClientHelloInfo {
                    client_version: client_version.clone(),
                    build_commit: build_commit.clone(),
                })
            }
        }
        (false, _) => HelloGateOutcome::RejectHandshakeRequired,
        (true, ClientMessage::ClientHello { .. }) => HelloGateOutcome::IgnoreRedundantHello,
        (true, _) => HelloGateOutcome::PassThrough,
    }
}

fn hello_acceptance(mode: ServerMode) -> HelloAcceptance {
    match mode {
        ServerMode::Full => HelloAcceptance::FullGame(MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION),
        ServerMode::LobbyOnly => HelloAcceptance::Lobby {
            lobby_floor: MIN_SUPPORTED_LOBBY_PROTOCOL,
            legacy_range: LOBBY_MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        },
    }
}

/// Returns `Some(error_message)` when `msg` is disabled under the current
/// server `mode`. Called at the top of dispatch so each handler below can
/// assume the message reached it legitimately.
///
/// **Exhaustive by design.** Every `ClientMessage` variant is explicitly
/// listed so adding a new variant is a compile error until the author
/// decides its mode policy. A catch-all `_ => None` would default-allow
/// future variants in both modes, which is the wrong default for a
/// security-relevant gate.
fn reject_if_disabled(msg: &ClientMessage, mode: ServerMode) -> Option<&'static str> {
    const LOBBY_ONLY_REJECTION: &str =
        "Server is in lobby-only mode — this message is not supported";
    const FULL_MODE_REJECTION: &str = "UnregisterLobby is only valid on lobby-only servers";

    match msg {
        // Always allowed — handshake, lobby subscription, ping.
        ClientMessage::ClientHello { .. }
        | ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Ping { .. } => None,

        // Game-state messages — disabled in lobby-only mode because the
        // server doesn't run a session in that mode.
        ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Action { .. }
        | ClientMessage::ResolveAll { .. }
        | ClientMessage::Interaction { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::AbandonGame
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::Concede
        | ClientMessage::ConcedeMatch
        | ClientMessage::BootstrapTerminalDelivery { .. }
        | ClientMessage::ReadTerminalResult { .. }
        | ClientMessage::AckTerminalDelivery { .. }
        | ClientMessage::Emote { .. }
        | ClientMessage::SpectatorJoin { .. }
        | ClientMessage::RequestTakeback(_)
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => match mode {
            ServerMode::Full => None,
            ServerMode::LobbyOnly => Some(LOBBY_ONLY_REJECTION),
        },

        // Broker messages — re-purposed in lobby-only mode, still valid in
        // Full mode (the Full-mode handler path uses them for hosting/joining
        // normal server-run games).
        ClientMessage::CreateGameWithSettings { .. }
        | ClientMessage::JoinGameWithPassword { .. }
        | ClientMessage::LookupJoinTarget { .. } => None,

        // Draft messages — Full-only (draft sessions are server-hosted).
        ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. } => match mode {
            ServerMode::Full => None,
            ServerMode::LobbyOnly => Some(LOBBY_ONLY_REJECTION),
        },

        // Lobby-only-exclusive.
        ClientMessage::UpdateLobbyMetadata { .. } | ClientMessage::UnregisterLobby { .. } => {
            match mode {
                ServerMode::Full => Some(FULL_MODE_REJECTION),
                ServerMode::LobbyOnly => None,
            }
        }
    }
}

fn guard_full_create_game_settings_inbound(
    fields: lobby_broker::CreateGameSettingsInbound<'_>,
    ai_seats: &[server_core::protocol::AiSeatRequest],
) -> Result<u8, String> {
    let pc = fields.player_count.clamp(2, MAX_FULL_GAME_PLAYER_COUNT);
    lobby_broker::validate_create_game_settings_inbound_fields(&fields)?;
    if let Some(format_config) = fields.format_config {
        format_config.validate_for_player_count(pc)?;
        format_config.reject_unimplemented_range_of_influence()?;
    }
    guard_create_ai_seats(ai_seats, pc)?;
    lobby_broker::validate_deck_payload("deck", fields.deck)?;
    Ok(pc)
}

/// Returns `Some(reason)` if `action` cannot legitimately come from a client
/// over the WebSocket draft protocol, or `None` if it is a valid client action.
///
/// **Exhaustive by design.** Every `DraftAction` variant is explicitly listed
/// so adding a new variant is a compile error until the author decides its
/// client-trust policy. A catch-all `_ => None` would default-allow future
/// variants, which is the wrong default for a security-relevant gate.
///
/// Rejected variants:
/// - `GeneratePairings`: draft match pairings are server-internal; accepting this
///   from clients would let a player force pairing generation out of sequence.
/// - `SetSeatConnected`: engine state plumbing. The server-internal runtime in
///   `server-core/src/draft_session.rs` broadcasts connection state via
///   `draft_core::session::apply` directly. Accepting it from a client would
///   let a malicious authenticated player forge another seat's connection
///   state (GH #1254). Caller-binding at `draft_session.rs:247-249` resolves
///   the authenticated seat from the token but discards it (`let _seat = ...`),
///   so the payload's `seat: u8` is otherwise unchecked.
fn client_forbidden_draft_action_reason(action: &draft_core::types::DraftAction) -> Option<String> {
    use draft_core::types::DraftAction;
    match action {
        DraftAction::GeneratePairings => {
            Some("GeneratePairings is server-internal; not allowed from client".to_string())
        }
        DraftAction::SetSeatConnected { .. } => {
            Some("SetSeatConnected is server-internal; not allowed from client".to_string())
        }
        DraftAction::StartDraft
        | DraftAction::Pick { .. }
        | DraftAction::PickWithDraftEffect { .. }
        | DraftAction::SubmitDeck { .. }
        | DraftAction::ReportMatchResult { .. }
        | DraftAction::AdvanceRound
        | DraftAction::ReplaceSeatWithBot { .. } => None,
    }
}

impl SocketIdentity {
    /// Set identity and create a tracing span for field inheritance.
    fn set_session(&mut self, game_code: String, player_id: PlayerId, player_token: String) {
        self.session_span = Some(tracing::info_span!(
            "game_session",
            game = %game_code,
            player = ?player_id,
        ));
        self.game_code = Some(game_code);
        self.player_id = Some(player_id);
        self.player_token = Some(player_token);
    }

    /// Project the shell's per-socket identity into the broker's [`ConnState`]
    /// view immediately before a broker call. `SocketIdentity` remains the
    /// single per-socket store; the broker mutates a transient view that the
    /// shell syncs back with [`SocketIdentity::absorb_conn_state`].
    fn to_conn_state(&self) -> ConnState {
        ConnState {
            client_hello: self
                .client_hello
                .as_ref()
                .map(|h| lobby_broker::ClientHelloInfo {
                    client_version: h.client_version.clone(),
                    build_commit: h.build_commit.clone(),
                }),
            subscribed: self.lobby_subscribed,
            host_game: self.lobby_host_game.clone(),
            reservations: self.lobby_reservations.clone(),
        }
    }

    /// Write the broker's [`ConnState`] mutations back into the shell identity
    /// after a broker call. `client_hello` is shell-owned (set by the handshake
    /// gate, never by the broker in the native shell) so it is not copied back.
    fn absorb_conn_state(&mut self, conn: ConnState) {
        self.lobby_subscribed = conn.subscribed;
        self.lobby_host_game = conn.host_game;
        self.lobby_reservations = conn.reservations;
    }
}

/// `thread_stack_size` governs Tokio's worker and blocking threads, but
/// `block_on` polls the root future on the **calling** thread — so `serve()`'s
/// own body (including the persisted-session restore) would run on the process
/// primary thread with whatever stack the OS handed it. `#[tokio::main]`
/// expands to exactly the same `build().block_on(..)` shape, so this is a
/// pre-existing gap rather than a regression: close it by owning the runtime
/// from a thread whose stack we chose.
fn main() {
    std::thread::Builder::new()
        .name("phase-server-runtime".to_owned())
        .stack_size(RUNTIME_THREAD_STACK_BYTES)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(RUNTIME_THREAD_STACK_BYTES)
                .build()
                .expect("failed to build the Tokio runtime")
                .block_on(serve());
        })
        .expect("spawn phase-server runtime thread")
        .join()
        .expect("phase-server runtime thread panicked");
}

async fn serve() {
    let cli = Cli::parse();

    let _log_guard = logging::init_logging(cli.log_dir.as_deref(), cli.log_json);
    let mode: Mode = if cli.lobby_only {
        ServerMode::LobbyOnly
    } else {
        ServerMode::Full
    };
    info!(?mode, "server mode selected");
    let data_path = cli.data_dir.as_path();
    let dev_fixture = dev_fixture_enabled();
    if bootstrap_required(data_path, dev_fixture) {
        let identity = data_bootstrap::ChannelIdentity::embedded()
            .unwrap_or_else(|error| fatal_startup(error));
        let options = data_bootstrap::BootstrapOptions {
            manifest_url_override: cli.data_manifest_url.clone(),
            no_data_download: cli.no_data_download,
        };
        if let Err(error) =
            data_bootstrap::bootstrap_missing_data(data_path, &options, identity.as_ref()).await
        {
            fatal_startup(error);
        }
    } else {
        warn!(
            path = %data_path.display(),
            "using PHASE_DEV_FIXTURE=1 test fixture; startup data bootstrap is disabled"
        );
    }
    let card_data_source = select_card_data_source(data_path, dev_fixture)
        .unwrap_or_else(|message| fatal_startup(message));
    let card_db = match card_data_source {
        CardDataSource::Export(path) => CardDatabase::from_export(&path).unwrap_or_else(|error| {
            fatal_startup(format!("failed to load {}: {error}", path.display()))
        }),
        CardDataSource::DevFixture(path) => {
            CardDatabase::from_mtgjson(&path).unwrap_or_else(|error| {
                fatal_startup(format!(
                    "PHASE_DEV_FIXTURE=1 was set but failed to load {}: {error}",
                    path.display()
                ))
            })
        }
    };
    info!(cards = card_db.card_count(), "card database loaded");
    let db: SharedDb = Arc::new(card_db);

    // Initialize SQLite persistence. `games_db` overrides the in-data-dir
    // default so the shell can keep saved games outside the per-version data
    // dir (which is recreated on every native-engine update). `Connection::open`
    // does not create parent dirs, so ensure the target's directory exists.
    let game_db_path = cli
        .games_db
        .clone()
        .unwrap_or_else(|| data_path.join("games.db"));
    if let Some(parent) = game_db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).expect("Failed to create game database directory");
    }
    let retention = if cli.single_user {
        persistence::SessionRetention::SingleUser
    } else {
        persistence::SessionRetention::Multiplayer
    };
    let game_db: SharedGameDb = Arc::new(
        persistence::GameDb::open(&game_db_path, retention).expect("Failed to open game database"),
    );
    // Clean up stale sessions (>24 hours old). Skipped for a single-user local
    // instance, where the one suspended solo game must survive until replaced.
    if !cli.single_user {
        if let Ok(deleted) = game_db.delete_stale(86400) {
            if deleted > 0 {
                info!(count = deleted, "cleaned up stale persisted sessions");
            }
        }
    }

    // A single-user instance has no other players whose seats a grace period
    // would free, so reconnects never expire — a game suspended for any length
    // of time stays resumable. `single_user` sets the reconnect window;
    // ten years is effectively unbounded without risking overflow in `now + grace`.
    // It also stamps `HostingMode::SingleUser` on every session this manager
    // owns, which is what grants the desktop sidecar its debug capability.
    let state: SharedState = Arc::new(Mutex::new(if cli.single_user {
        SessionManager::single_user(Duration::from_secs(10 * 365 * 24 * 60 * 60))
    } else {
        SessionManager::new()
    }));
    let draft_sessions: SharedDraftState = Arc::new(Mutex::new(DraftSessionManager::new()));
    let draft_pools_path = data_path.join("draft-pools.json");
    let draft_pools: SharedDraftPools = match draft_pools::DraftPools::from_path(&draft_pools_path)
    {
        Ok(pools) => {
            info!(sets = pools.len(), "draft pools loaded");
            Arc::new(pools)
        }
        Err(e) => {
            warn!(
                path = %draft_pools_path.display(),
                error = %e,
                "draft pools unavailable; server-hosted drafts cannot start"
            );
            Arc::new(draft_pools::DraftPools::default())
        }
    };
    let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
    let draft_spectators: SharedDraftSpectators = Arc::new(Mutex::new(HashMap::new()));
    let game_spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
    let lobby: SharedLobby = Arc::new(Mutex::new(Broker::new()));
    let lobby_subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
    let player_count: SharedPlayerCount = Arc::new(AtomicU32::new(0));

    // Restore persisted game sessions from disk. In lobby-only mode the
    // server runs no engine, so persisted GameState snapshots can't be
    // replayed — skip the restore pass entirely and let SQLite ignore the
    // stale rows until operators clean them up manually.
    if matches!(mode, ServerMode::Full) {
        match game_db.load_active_full_sessions() {
            Ok(persisted_games) => {
                let mut mgr = state.lock().await;
                let mut lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby_mut();
                let mut restored = 0u32;

                for snapshot in &persisted_games {
                    let game_code = &snapshot.key.game_code;
                    let json = match serde_json::to_string(&snapshot.persisted) {
                        Ok(json) => json,
                        Err(error) => {
                            warn!(game = %game_code, %error, "failed to serialize restored Full session");
                            continue;
                        }
                    };
                    info!(game = %game_code, bytes = json.len(), "restoring persisted session");
                    match restore_persisted_session(&json, db.clone()) {
                        Ok(mut session) => {
                            session.full_runtime = Some(FullRuntime {
                                key: snapshot.key.clone(),
                                activation_epoch: snapshot.activation_epoch,
                            });
                            let lobby_meta = session.lobby_meta.clone();
                            let is_started = session.game_started;

                            // Register all non-AI human players as disconnected
                            // to start the 120s grace period from now
                            let default_grace = mgr.reconnect.grace_period;
                            for (i, token) in session.player_tokens.iter().enumerate() {
                                let pid = PlayerId(i as u8);
                                if !token.is_empty() && !session.ai_seats.contains(&pid) {
                                    mgr.reconnect.record_disconnect(
                                        &session.game_code,
                                        pid,
                                        default_grace,
                                    );
                                }
                            }

                            // Restore lobby entry if game hasn't started.
                            // Persisted sessions pre-date version metadata;
                            // restored lobbies appear without a version badge.
                            if let Some(meta) = lobby_meta {
                                if !is_started {
                                    lob.register_game(
                                        game_code,
                                        RegisterGameRequest {
                                            host_name: meta.host_name,
                                            public: meta.public,
                                            password: meta.password,
                                            timer_seconds: meta.timer_seconds,
                                            current_players: session.current_player_count(),
                                            max_players: session.player_count as u32,
                                            format_config: Some(
                                                session.state.format_config.clone(),
                                            ),
                                            match_config: session.state.match_config,
                                            ..Default::default()
                                        },
                                        &SysEnv,
                                    );
                                }
                            }

                            mgr.restore_session(session);
                            restored += 1;
                        }
                        Err(e) => {
                            warn!(game = %game_code, error = %e, "failed to restore active session; retaining fenced row for recovery");
                        }
                    }
                }

                if restored > 0 {
                    info!(count = restored, "restored active games from disk");
                }
            }
            Err(e) => {
                error!(error = %e, "failed to load persisted sessions");
            }
        }

        // Restore persisted draft sessions from disk
        match game_db.load_all_drafts() {
            Ok(persisted_drafts) => {
                let mut dsm = draft_sessions.lock().await;
                let mut lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby_mut();
                let mut restored_drafts = 0u32;
                for (draft_code, json) in &persisted_drafts {
                    match serde_json::from_str::<server_core::persist::PersistedDraftSession>(json)
                    {
                        Ok(ps) => {
                            let register_req =
                                server_core::persist::restored_draft_lobby_register_request(&ps);
                            let timer_ms = ps.timer_remaining_ms;
                            if let Err(error) = dsm.restore_persisted_session(ps) {
                                warn!(draft = %draft_code, error = %error, "invalid persisted draft session, deleting");
                                let _ = game_db.delete_draft_session(draft_code);
                                continue;
                            }
                            if let Some(req) = register_req {
                                lob.register_game(draft_code, req, &SysEnv);
                            }
                            if let Some(ms) = timer_ms {
                                info!(draft = %draft_code, remaining_ms = ms, "draft session has pending timer");
                            }
                            restored_drafts += 1;
                        }
                        Err(e) => {
                            warn!(draft = %draft_code, error = %e, "failed to restore draft session, deleting");
                            let _ = game_db.delete_draft_session(draft_code);
                        }
                    }
                }
                if restored_drafts > 0 {
                    info!(count = restored_drafts, "restored draft sessions from disk");
                }
            }
            Err(e) => error!(error = %e, "failed to load persisted draft sessions"),
        }
    }

    // Spawn background task for grace period and lobby expiry
    let bg_state = state.clone();
    let bg_draft_state = draft_sessions.clone();
    let bg_connections = connections.clone();
    let bg_draft_spectators = draft_spectators.clone();
    let bg_game_spectators = game_spectators.clone();
    let bg_lobby = lobby.clone();
    let bg_lobby_subs = lobby_subscribers.clone();
    let bg_game_db = game_db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;

            // Check reconnect grace period expiry
            let expired = {
                let mut mgr = bg_state.lock().await;
                mgr.reconnect.check_expired()
            };
            if !expired.is_empty() {
                let terminal_candidates = {
                    let mgr = bg_state.lock().await;
                    expired
                        .iter()
                        .filter_map(|game_code| {
                            let session = mgr.sessions.get(game_code)?;
                            session
                                .game_started
                                .then(|| {
                                    terminal_artifact(
                                        session,
                                        None,
                                        "Opponent disconnected (grace period expired)".to_string(),
                                        None,
                                    )
                                    .map(|artifact| (game_code.clone(), artifact))
                                })?
                                .ok()
                        })
                        .collect::<Vec<_>>()
                };
                let mut prepared = HashMap::new();
                for (game_code, artifact) in terminal_candidates {
                    match prepare_full_terminal(&bg_game_db, artifact).await {
                        Ok(deliveries) => {
                            prepared.insert(game_code, deliveries);
                        }
                        Err(error) => {
                            error!(game = %game_code, %error, "disconnect terminal preparation failed")
                        }
                    }
                }
                let removed = {
                    let mut mgr = bg_state.lock().await;
                    expired
                        .iter()
                        .filter_map(|game_code| {
                            let session = mgr.sessions.get(game_code)?;
                            if !session.game_started || prepared.contains_key(game_code) {
                                mgr.remove_game(game_code)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                };
                {
                    let conns = bg_connections.lock().await;
                    for session in &removed {
                        let game_code = &session.game_code;
                        info!(game = %game_code, reason = "disconnect_expired", "game over");
                        if let Some(players) = conns.get(game_code) {
                            if let Some(deliveries) = prepared.get(game_code) {
                                for (player, delivery) in deliveries {
                                    if let Some(sender) = players.get(player) {
                                        let _ = sender.send(ServerMessage::TerminalResult {
                                            delivery: Some(delivery.clone()),
                                        });
                                    }
                                }
                            }
                        }
                        if !session.game_started {
                            retire_unstarted_session_async(&bg_game_db, session);
                        }
                    }
                }
                prune_game_connections(
                    &bg_connections,
                    removed.iter().map(|session| session.game_code.as_str()),
                )
                .await;
                let mut specs = bg_game_spectators.lock().await;
                for session in &removed {
                    specs.remove(&session.game_code);
                }
            }

            // Check lobby game expiry (5 minute timeout for waiting games).
            // The broker reaps stale entries and returns the LobbyGameRemoved
            // fan-out outbounds; the Full-mode session/db deletion stays here
            // (the broker is WASM-safe and has no SQLite/SessionManager). The
            // expired codes are recovered from the returned outbounds.
            let reap_outbounds = {
                let mut broker = bg_lobby.lock().await;
                broker.reap_expired(300, &SysEnv)
            };
            if !reap_outbounds.is_empty() {
                let expired_lobby: Vec<String> = reap_outbounds
                    .iter()
                    .filter_map(|ob| match ob {
                        Outbound::ToSubscribers(
                            lobby_broker::LobbyServerMessage::LobbyGameRemoved { game_code },
                        ) => Some(game_code.clone()),
                        _ => None,
                    })
                    .collect();
                info!(count = expired_lobby.len(), "expiring stale lobby games");
                let mut mgr = bg_state.lock().await;
                for game_code in &expired_lobby {
                    if mgr
                        .sessions
                        .get(game_code)
                        .is_some_and(|session| !session.game_started)
                    {
                        if let Some(session) = mgr.remove_game(game_code) {
                            retire_unstarted_session_async(&bg_game_db, &session);
                        }
                    } else if mgr.sessions.contains_key(game_code) {
                        error!(game = %game_code, "refusing to retire a started session from lobby expiry");
                    }
                }
                drop(mgr);
                prune_game_connections(&bg_connections, expired_lobby.iter().map(String::as_str))
                    .await;
                let mut specs = bg_game_spectators.lock().await;
                for game_code in &expired_lobby {
                    specs.remove(game_code);
                }

                let subs = bg_lobby_subs.lock().await;
                for ob in reap_outbounds {
                    if let Outbound::ToSubscribers(msg) = ob {
                        let server_msg = to_server_message(msg);
                        for sub in subs.iter() {
                            let _ = sub.send(server_msg.clone());
                        }
                    }
                }
            }

            // Check draft disconnect grace period expiry — auto-pick for disconnected seats
            let draft_expired = {
                let mut mgr = bg_draft_state.lock().await;
                mgr.reconnect.check_expired_with_players()
            };
            if !draft_expired.is_empty() {
                let mut mgr = bg_draft_state.lock().await;
                for (draft_code, player_id) in &draft_expired {
                    let seat = player_id.0;
                    if let Some(session) = mgr.sessions.get(draft_code.as_str()) {
                        if session.session.status == draft_core::types::DraftStatus::Drafting
                            && !session.connected[seat as usize]
                        {
                            match mgr.pick_random_for_seat(draft_code, seat, None) {
                                Ok(()) => {
                                    info!(
                                        draft = %draft_code,
                                        seat,
                                        "auto-picked for disconnected seat (grace expired)"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        draft = %draft_code,
                                        seat,
                                        error = %e,
                                        "auto-pick on grace expiry failed"
                                    );
                                }
                            }
                        }
                    }
                }
                // Broadcast updated views + persist for any modified drafts
                let affected_drafts: Vec<String> = draft_expired
                    .iter()
                    .map(|(code, _)| code.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                drop(mgr);
                for draft_code in &affected_drafts {
                    // Broadcast to players
                    let views: Vec<_> = {
                        let mgr = bg_draft_state.lock().await;
                        let Some(session) = mgr.sessions.get(draft_code) else {
                            continue;
                        };
                        let pod_size = session.player_tokens.len();
                        (0..pod_size).map(|i| session.view_for_seat(i)).collect()
                    };
                    broadcast_draft_views(draft_code, &views, &bg_connections, &bg_draft_state)
                        .await;
                    // Broadcast to spectators
                    broadcast_draft_spectator_views(
                        draft_code,
                        &bg_draft_state,
                        &bg_draft_spectators,
                    )
                    .await;
                    // Persist
                    persist_draft_session_async(&bg_game_db, draft_code, &bg_draft_state).await;
                }
            }
        }
    });

    let cors = match cli.cors_origin.as_deref() {
        Some("*") | None => CorsLayer::permissive(),
        Some(origin) => CorsLayer::new()
            .allow_origin(origin.parse::<HeaderValue>().expect("invalid CORS origin")),
    };

    // Keep references for shutdown flush (Arcs are cheap to clone)
    let shutdown_state = state.clone();
    let shutdown_draft_state = draft_sessions.clone();
    let shutdown_game_db = game_db.clone();

    // Resolve the public URL advertised to clients for `<code>@<host>` join
    // strings. An explicit `--public-url` (validated at the boundary) wins;
    // otherwise an embedded ngrok tunnel supplies one when the `ngrok` feature
    // is built and NGROK_AUTHTOKEN is set. `_ngrok_forwarder` keeps the tunnel
    // open for the process lifetime (dropped on shutdown ⇒ tunnel closed); a
    // tunnel that fails to establish never blocks local boot.
    let configured_public_url = cli.public_url.as_deref().and_then(validate_public_url);
    #[cfg(feature = "ngrok")]
    let (advertised_public_url, _ngrok_forwarder) = match start_ngrok_tunnel(cli.port).await {
        Some((url, fwd)) => (configured_public_url.clone().or(Some(url)), Some(fwd)),
        None => (configured_public_url, None),
    };
    #[cfg(not(feature = "ngrok"))]
    let advertised_public_url = {
        if std::env::var_os("NGROK_AUTHTOKEN").is_some() {
            warn!(
                "NGROK_AUTHTOKEN is set but phase-server was built without the `ngrok` feature; \
                 embedded tunnel disabled. Rebuild with `--features ngrok`."
            );
        }
        configured_public_url
    };
    if let Some(url) = &advertised_public_url {
        info!(public_url = %url, "advertising public URL for join-code sharing");
    }

    // Public, client-facing HTTP surface. `/p2p-draft-backup*` is part of the
    // normal P2P draft flow; only the administrative `/admin/*` routes are gated.
    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/p2p-draft-backup", post(admin::p2p_backup_store))
        .route(
            "/p2p-draft-backup/{code}",
            get(admin::p2p_backup_get).delete(admin::p2p_backup_delete),
        );

    // Administrative endpoints are destructive and information-disclosing, and
    // reachable through the same reverse proxy as `/ws` (see deploy nginx).
    // Mount them only when PHASE_ADMIN_TOKEN is set; otherwise absent (404).
    let admin_token = admin_token_from_env();
    match admin_token.as_deref() {
        Some(_) => info!("admin HTTP endpoints enabled (bearer-token authenticated)"),
        None => info!("admin HTTP endpoints disabled (set PHASE_ADMIN_TOKEN to enable)"),
    }
    if let Some(token) = admin_token.as_deref().filter(|t| !t.is_empty()) {
        app = mount_admin_routes(app, token);
    }

    let app = app.layer(cors).with_state(AppState {
        sessions: state,
        draft_sessions,
        draft_pools,
        connections,
        db,
        lobby,
        lobby_subscribers,
        player_count,
        game_db,
        draft_spectators,
        game_spectators,
        mode,
        public_url: advertised_public_url,
        allowed_origin: cli.allowed_origin.clone(),
    });

    let listener = tokio::net::TcpListener::bind((cli.bind, cli.port))
        .await
        .expect("failed to bind");
    info!(bind = %cli.bind, port = %cli.port, "phase-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cli.exit_on_stdin_close))
        .await
        .expect("server error");

    // Flush all active sessions to SQLite before exiting so they survive restart
    let mgr = shutdown_state.lock().await;
    let mut persisted = 0u32;
    for (game_code, session) in &mgr.sessions {
        let Some(snapshot) = session.full_persist_snapshot() else {
            warn!(game = %game_code, "skipping unbound Full session at shutdown");
            continue;
        };
        match shutdown_game_db.save_full_session(&snapshot) {
            Ok(server_core::FullPersistDisposition::Applied) => persisted += 1,
            Ok(disposition) => {
                warn!(game = %game_code, ?disposition, "shutdown snapshot was no longer current")
            }
            Err(error) => {
                error!(game = %game_code, %error, "failed to persist Full session on shutdown")
            }
        }
    }
    if persisted > 0 {
        info!(
            count = persisted,
            "flushed active sessions to disk on shutdown"
        );
    }

    // Flush all active draft sessions to SQLite
    let dsm = shutdown_draft_state.lock().await;
    let mut flushed_drafts = 0u32;
    for (draft_code, session) in &dsm.sessions {
        let snapshot = session.to_persisted();
        match serde_json::to_string(&snapshot) {
            Ok(json) => {
                if let Err(e) = shutdown_game_db.save_draft_session(draft_code, &json) {
                    error!(draft = %draft_code, error = %e, "failed to persist draft on shutdown");
                } else {
                    flushed_drafts += 1;
                }
            }
            Err(e) => {
                error!(draft = %draft_code, error = %e, "failed to serialize draft for shutdown");
            }
        }
    }
    if flushed_drafts > 0 {
        info!(
            count = flushed_drafts,
            "flushed draft sessions to disk on shutdown"
        );
    }
}

fn stdin_close_watchdog(enabled: bool) -> Option<oneshot::Receiver<()>> {
    enabled.then(|| {
        let (sender, receiver) = oneshot::channel();
        let _stdin_watchdog = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buffer = [0_u8; 1024];
            loop {
                match stdin.read(&mut buffer).await {
                    Ok(0) => {
                        info!("stdin closed; shutting down");
                        let _ = sender.send(());
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(error = %error, "stdin watchdog stopped without EOF");
                        return;
                    }
                }
            }
        });
        receiver
    })
}

async fn wait_for_stdin_close(mut watchdog: Option<oneshot::Receiver<()>>) {
    match watchdog.as_mut() {
        Some(receiver) => {
            if receiver.await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

async fn shutdown_signal(exit_on_stdin_close: bool) {
    let ctrl_c = tokio::signal::ctrl_c();
    let stdin_close = wait_for_stdin_close(stdin_close_watchdog(exit_on_stdin_close));
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => info!("received Ctrl+C, shutting down"),
            _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
            _ = stdin_close => info!("stdin-close watchdog requested shutdown"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            result = ctrl_c => {
                result.expect("failed to listen for Ctrl+C");
                info!("received Ctrl+C, shutting down");
            }
            _ = stdin_close => info!("stdin-close watchdog requested shutdown"),
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

#[cfg(test)]
mod lifecycle_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};
    use clap::Parser;
    use tokio::sync::Mutex;

    use super::{
        bootstrap_required, origin_is_allowed, prune_game_connections, select_card_data_source,
        CardDataSource, Cli, SharedConnections,
    };

    #[test]
    fn bind_flag_defaults_to_lan_and_accepts_loopback() {
        let default = Cli::try_parse_from(["phase-server"]).expect("default CLI parses");
        assert_eq!(default.bind.to_string(), "0.0.0.0");

        let loopback = Cli::try_parse_from(["phase-server", "--bind", "127.0.0.1"])
            .expect("loopback bind parses");
        assert_eq!(loopback.bind.to_string(), "127.0.0.1");
    }

    #[test]
    fn allowed_origin_accepts_matching_and_originless_clients() {
        let mut matching = HeaderMap::new();
        matching.insert(ORIGIN, HeaderValue::from_static("https://phase-rs.dev"));
        assert!(origin_is_allowed(&matching, Some("https://phase-rs.dev")));

        let mut mismatched = HeaderMap::new();
        mismatched.insert(ORIGIN, HeaderValue::from_static("https://attacker.example"));
        assert!(!origin_is_allowed(
            &mismatched,
            Some("https://phase-rs.dev")
        ));

        assert!(origin_is_allowed(
            &HeaderMap::new(),
            Some("https://phase-rs.dev")
        ));
        assert!(origin_is_allowed(&mismatched, None));
    }

    #[test]
    fn fixture_fallback_requires_the_explicit_dev_opt_in() {
        let temp = tempfile::tempdir().expect("temp dir");

        assert!(bootstrap_required(temp.path(), false));
        assert!(select_card_data_source(temp.path(), false).is_err());
        assert!(!bootstrap_required(temp.path(), true));
        assert_eq!(
            select_card_data_source(temp.path(), true).expect("explicit fixture source"),
            CardDataSource::DevFixture(temp.path().join("mtgjson/test_fixture.json"))
        );
    }

    #[tokio::test]
    async fn disconnect_expiry_prunes_only_the_finished_game_connections() {
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut conns = connections.lock().await;
            conns.insert("EXPIRED".to_string(), HashMap::new());
            conns.insert("ACTIVE".to_string(), HashMap::new());
        }

        prune_game_connections(&connections, ["EXPIRED"]).await;

        let conns = connections.lock().await;
        assert!(!conns.contains_key("EXPIRED"));
        assert!(conns.contains_key("ACTIVE"));
    }

    #[tokio::test]
    async fn lobby_expiry_prunes_only_the_finished_game_connections() {
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut conns = connections.lock().await;
            conns.insert("EXPIRED".to_string(), HashMap::new());
            conns.insert("ACTIVE".to_string(), HashMap::new());
        }

        prune_game_connections(&connections, ["EXPIRED"]).await;

        let conns = connections.lock().await;
        assert!(!conns.contains_key("EXPIRED"));
        assert!(conns.contains_key("ACTIVE"));
    }
}

/// Constant-time byte comparison so admin-token validation does not leak the
/// expected token through response timing.
fn tokens_match(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in presented.iter().zip(expected.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Load the admin bearer token from the environment. Intentionally not a CLI
/// flag — command-line secrets leak via process listings and shell history.
fn admin_token_from_env() -> Option<String> {
    std::env::var("PHASE_ADMIN_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Mount bearer-guarded `/admin/*` routes on a router that will receive `AppState`.
fn mount_admin_routes(app: Router<AppState>, admin_token: &str) -> Router<AppState> {
    let auth_layer = |expected: Arc<str>| {
        from_fn(move |request: Request, next: Next| {
            let expected = expected.clone();
            async move { require_admin_auth(expected, request, next).await }
        })
    };
    let list_auth = auth_layer(Arc::from(admin_token));
    let detail_auth = auth_layer(Arc::from(admin_token));
    app.route(
        "/admin/drafts",
        get(admin::admin_list_drafts).route_layer(list_auth),
    )
    .route(
        "/admin/drafts/{code}",
        get(admin::admin_get_draft)
            .delete(admin::admin_delete_draft)
            .route_layer(detail_auth),
    )
}

/// Decide whether an `Authorization` header value authorizes an admin request.
/// Scheme must be `Bearer` (case-insensitive per RFC 9110); credential must
/// match `expected` in constant time.
fn admin_request_authorized(auth_header: Option<&str>, expected: &str) -> bool {
    let Some(value) = auth_header.map(str::trim) else {
        return false;
    };
    let Some((scheme, credentials)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return false;
    }
    tokens_match(credentials.trim().as_bytes(), expected.as_bytes())
}

/// Auth guard for the administrative `/admin/*` routes.
async fn require_admin_auth(expected: Arc<str>, request: Request, next: Next) -> Response {
    let auth_header = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    if admin_request_authorized(auth_header, &expected) {
        next.run(request).await
    } else {
        (http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

/// Validate an operator-supplied public URL at the system boundary. It must
/// parse as an absolute URL with a host; a malformed value is dropped (logged)
/// rather than advertised to clients verbatim. Returns the URL with any
/// trailing slash trimmed.
fn validate_public_url(raw: &str) -> Option<String> {
    match Url::parse(raw) {
        Ok(u) if u.host_str().is_some() => Some(raw.trim_end_matches('/').to_string()),
        _ => {
            warn!(value = %raw, "ignoring malformed PUBLIC_URL (need an absolute URL with a host)");
            None
        }
    }
}

/// Open an embedded ngrok HTTP tunnel that forwards public traffic to the local
/// server on `port`, returning `(public_url, forwarder_handle)`. The handle
/// keeps the tunnel open while held; dropping it closes the tunnel. Any failure
/// (missing/invalid `NGROK_AUTHTOKEN`, network) is logged and returns `None` —
/// the local server still runs, so the tunnel is strictly additive.
#[cfg(feature = "ngrok")]
async fn start_ngrok_tunnel(port: u16) -> Option<(String, Box<dyn std::any::Any + Send>)> {
    use ngrok::prelude::*;

    let upstream = match Url::parse(&format!("http://localhost:{port}")) {
        Ok(u) => u,
        Err(e) => {
            error!(error = %e, "ngrok: invalid upstream URL; tunnel disabled");
            return None;
        }
    };
    let session = match ngrok::Session::builder()
        .authtoken_from_env()
        .connect()
        .await
    {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "ngrok: session connect failed; tunnel disabled, local server still running");
            return None;
        }
    };
    let forwarder = match session.http_endpoint().listen_and_forward(upstream).await {
        Ok(f) => f,
        Err(e) => {
            error!(error = %e, "ngrok: tunnel start failed; disabled, local server still running");
            return None;
        }
    };
    let url = forwarder.url().to_string();
    info!(url = %url, "ngrok tunnel established");
    Some((url, Box::new(forwarder)))
}

#[derive(Clone)]
struct AppState {
    sessions: SharedState,
    draft_sessions: SharedDraftState,
    draft_pools: SharedDraftPools,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
    game_db: SharedGameDb,
    draft_spectators: SharedDraftSpectators,
    game_spectators: SharedGameSpectators,
    mode: Mode,
    /// Public base URL advertised in `ServerHello` (from `--public-url`/an
    /// embedded ngrok tunnel), or `None` when the server has no reachable
    /// address to share. Cloned per connection at greet time only.
    public_url: Option<String>,
    /// Origin allowed to upgrade WebSocket handshakes. `None` preserves the
    /// self-hosted permissive behavior; origin-less non-browser clients are
    /// always allowed.
    allowed_origin: Option<String>,
}

fn origin_is_allowed(headers: &HeaderMap, allowed_origin: Option<&str>) -> bool {
    let Some(allowed_origin) = allowed_origin else {
        return true;
    };
    match headers.get(http::header::ORIGIN) {
        None => true,
        Some(origin) => origin.to_str().is_ok_and(|origin| origin == allowed_origin),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app_state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !origin_is_allowed(&headers, app_state.allowed_origin.as_deref()) {
        warn!(
            origin = ?headers.get(http::header::ORIGIN),
            "rejecting WebSocket handshake from disallowed Origin"
        );
        return (http::StatusCode::FORBIDDEN, "WebSocket Origin not allowed").into_response();
    }
    let current = app_state.player_count.load(Ordering::Relaxed);
    if current >= MAX_CONNECTIONS {
        warn!(
            online_count = current,
            limit = MAX_CONNECTIONS,
            "connection limit reached, rejecting"
        );
        return (http::StatusCode::SERVICE_UNAVAILABLE, "Server full").into_response();
    }

    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                app_state.sessions,
                app_state.draft_sessions,
                app_state.draft_pools,
                app_state.connections,
                app_state.db,
                app_state.lobby,
                app_state.lobby_subscribers,
                app_state.player_count,
                app_state.game_db,
                app_state.draft_spectators,
                app_state.game_spectators,
                app_state.mode,
                app_state.public_url,
            )
        })
        .into_response()
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket(
    mut socket: WebSocket,
    state: SharedState,
    draft_state: SharedDraftState,
    draft_pools: SharedDraftPools,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
    game_db: SharedGameDb,
    draft_spectators: SharedDraftSpectators,
    game_spectators: SharedGameSpectators,
    mode: Mode,
    public_url: Option<String>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let count = player_count.fetch_add(1, Ordering::Relaxed) + 1;
    info!(online_count = count, "client connected");
    broadcast_player_count(&lobby_subscribers, count).await;

    let mut identity = SocketIdentity {
        game_code: None,
        player_id: None,
        player_token: None,
        lobby_subscribed: false,
        session_span: None,
        client_hello: None,
        lobby_host_game: None,
        seat_reservations: Vec::new(),
        lobby_reservations: Vec::new(),
        draft_code: None,
        draft_seat: None,
        draft_token: None,
        spectator_draft_code: None,
        spectator_visibility: None,
        spectator_game_code: None,
    };
    let mut rate_limiter = RateLimiter::new();

    // Greet the client with our version identity. The client uses this to
    // decide whether to proceed (protocol-version mismatch ⇒ it gives up
    // before sending any game-affecting frame). The advertised `mode` lets
    // the client route host/join flows through WS (Full) or P2P+broker
    // (LobbyOnly) without probing.
    let hello = ServerMessage::ServerHello {
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        build_commit: build_commit().to_string(),
        protocol_version: PROTOCOL_VERSION,
        mode,
        lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
        public_url,
    };
    if let Ok(json) = serde_json::to_string(&hello) {
        if socket.send(Message::text(json)).await.is_err() {
            let count = player_count.fetch_sub(1, Ordering::Relaxed) - 1;
            broadcast_player_count(&lobby_subscribers, count).await;
            return;
        }
    }

    loop {
        tokio::select! {
            biased;
            Some(msg) = rx.recv() => {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if socket.send(Message::text(json)).await.is_err() {
                        break;
                    }
                }
            }

            result = socket.recv() => {
                match result {
                    Some(Ok(msg)) => {
                        let text = match msg {
                            Message::Text(t) => t.to_string(),
                            Message::Close(_) => break,
                            _ => continue,
                        };

                        if !rate_limiter.check() {
                            debug!("rate limit exceeded, dropping message");
                            continue;
                        }

                        let client_msg: ClientMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(error = %e, "failed to parse client message");
                                let err_msg = ServerMessage::error(format!("Invalid message: {}", e));
                                if let Ok(json) = serde_json::to_string(&err_msg) {
                                    let _ = socket.send(Message::text(json)).await;
                                }
                                continue;
                            }
                        };

                        let span = identity.session_span.clone()
                            .unwrap_or_else(|| info_span!("ws_message"));
                        handle_client_message(
                            client_msg,
                            &mut socket,
                            &state,
                            &draft_state,
                            &draft_pools,
                            &connections,
                            &db,
                            &lobby,
                            &lobby_subscribers,
                            &player_count,
                            &game_db,
                            &draft_spectators,
                            &game_spectators,
                            &tx,
                            &mut identity,
                            mode,
                        )
                        .instrument(span)
                        .await;
                    }
                    Some(Err(_)) | None => break,
                }
            }
        }
    }

    // Socket closed -- handle disconnect
    info!(
        game = ?identity.game_code,
        player = ?identity.player_id,
        "client disconnected"
    );
    // Handle draft session disconnect
    if let (Some(draft_code), Some(seat)) = (&identity.draft_code, identity.draft_seat) {
        let mut mgr = draft_state.lock().await;
        mgr.handle_disconnect(draft_code, seat);
    }

    if let (Some(game_code), Some(player_id)) = (&identity.game_code, &identity.player_id) {
        let mut mgr = state.lock().await;
        mgr.handle_disconnect(game_code, *player_id);

        // Notify all other connected players about this disconnection
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            let msg = ServerMessage::OpponentDisconnected {
                grace_seconds: 120,
                player: Some(*player_id),
            };
            for (&pid, sender) in players.iter() {
                if pid != *player_id {
                    let _ = sender.send(msg.clone());
                }
            }
        }
    }

    if let Some(game_code) = &identity.spectator_game_code {
        remove_game_spectator_sender(&game_spectators, game_code, &tx).await;
    }
    if let Some(draft_code) = &identity.spectator_draft_code {
        remove_draft_spectator_sender(&draft_spectators, draft_code, &tx).await;
    }

    if !identity.seat_reservations.is_empty() {
        let changed = {
            let mut mgr = state.lock().await;
            mgr.release_reservations(&identity.seat_reservations)
        };
        if changed {
            for (game_code, _) in &identity.seat_reservations {
                broadcast_player_slots(&state, &connections, game_code).await;
                let updated = {
                    let current = {
                        let mut mgr = state.lock().await;
                        mgr.sessions.get_mut(game_code).map(|session| {
                            session.cleanup_expired_reservations();
                            session.current_player_count()
                        })
                    };
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    if let Some(current) = current {
                        lob.set_current_players(game_code, current, &SysEnv);
                    }
                    lob.public_game(game_code)
                };
                if let Some(game) = updated {
                    broadcast_to_lobby_subscribers(
                        &lobby_subscribers,
                        ServerMessage::LobbyGameUpdated { game },
                    )
                    .await;
                }
            }
        }
    }

    // Lobby teardown (reservation releases → host-entry removal → subscriber
    // pruning) is the broker's `on_disconnect`. It emits, in order, a
    // LobbyGameUpdated per released reservation, then a LobbyGameRemoved if
    // this socket owned an entry, then RemoveSubscriber. The 5-minute
    // staleness reaper is the fallback if this path doesn't fire (e.g. crash).
    // Player-count decrement + broadcast stays shell-side (unconditional).
    {
        let mut conn = identity.to_conn_state();
        let outbounds = {
            let mut broker = lobby.lock().await;
            broker.on_disconnect(&mut conn)
        };
        identity.absorb_conn_state(conn);
        apply_outbounds(outbounds, &tx, &lobby_subscribers, &player_count).await;
    }

    let count = player_count.fetch_sub(1, Ordering::Relaxed) - 1;
    broadcast_player_count(&lobby_subscribers, count).await;
}

async fn broadcast_player_count(lobby_subscribers: &SharedLobbySubscribers, count: u32) {
    let subs = lobby_subscribers.lock().await;
    let msg = ServerMessage::PlayerCount { count };
    for sub in subs.iter() {
        let _ = sub.send(msg.clone());
    }
}

/// Send PlayerSlotsUpdate to all connected players in a game.
async fn broadcast_player_slots(
    state: &SharedState,
    connections: &SharedConnections,
    game_code: &str,
) {
    let slots = {
        let mgr = state.lock().await;
        match mgr.sessions.get(game_code) {
            Some(session) => session.player_slot_info(),
            None => return,
        }
    };
    let msg = ServerMessage::PlayerSlotsUpdate { slots };
    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        for sender in players.values() {
            let _ = sender.send(msg.clone());
        }
    }
}

async fn broadcast_to_lobby_subscribers(
    lobby_subscribers: &SharedLobbySubscribers,
    msg: ServerMessage,
) {
    let subs = lobby_subscribers.lock().await;
    for sub in subs.iter() {
        let _ = sub.send(msg.clone());
    }
}

/// Translate a broker [`lobby_broker::LobbyServerMessage`] into the canonical
/// transport [`ServerMessage`]. Pure field-mapping at the serialization
/// boundary — the two enums are wire-compatible (guarded by the lobby wire
/// contract test); the shared payload types (`LobbyGame`, `FormatConfig`,
/// `MatchConfig`) are the same structs, so this is a zero-cost re-tag.
fn to_server_message(m: lobby_broker::LobbyServerMessage) -> ServerMessage {
    use lobby_broker::LobbyServerMessage as L;
    match m {
        L::ServerHello {
            server_version,
            build_commit,
            protocol_version,
            mode,
            lobby_protocol_version,
        } => ServerMessage::ServerHello {
            server_version,
            build_commit,
            protocol_version,
            mode: match mode {
                lobby_broker::ServerMode::Full => ServerMode::Full,
                lobby_broker::ServerMode::LobbyOnly => ServerMode::LobbyOnly,
            },
            lobby_protocol_version,
            // LobbyOnly brokers run no server-side game, so there is no
            // game-server URL to advertise for a `<code>@<host>` share string.
            public_url: None,
        },
        L::GameCreated {
            game_code,
            player_token,
        } => ServerMessage::GameCreated {
            game_code,
            player_token,
            full_key: None,
        },
        L::Error { message, code } => ServerMessage::Error { message, code },
        L::LobbyUpdate { games } => ServerMessage::LobbyUpdate { games },
        L::LobbyGameAdded { game } => ServerMessage::LobbyGameAdded { game },
        L::LobbyGameUpdated { game } => ServerMessage::LobbyGameUpdated { game },
        L::LobbyGameRemoved { game_code } => ServerMessage::LobbyGameRemoved { game_code },
        L::PlayerCount { count } => ServerMessage::PlayerCount { count },
        L::PasswordRequired { game_code } => ServerMessage::PasswordRequired { game_code },
        L::JoinTargetInfo {
            game_code,
            is_p2p,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
            reservation_expires_at_ms,
        } => ServerMessage::JoinTargetInfo {
            game_code,
            is_p2p,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
            reservation_expires_at_ms,
        },
        L::Pong { timestamp } => ServerMessage::Pong { timestamp },
        L::PeerInfo {
            game_code,
            host_peer_id,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
        } => ServerMessage::PeerInfo {
            game_code,
            host_peer_id,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
        },
    }
}

/// Project a canonical [`ClientMessage`] onto the broker's lobby subset
/// [`lobby_broker::LobbyClientMessage`]. The native shell already deserialized
/// and gated the full `ClientMessage` (unknown tags rejected at parse time, so
/// the two-stage `Envelope` path is unneeded here — it serves the DO shell).
/// Returns `None` for non-lobby messages, which the caller dispatches normally.
fn to_lobby_client_message(msg: &ClientMessage) -> Option<lobby_broker::LobbyClientMessage> {
    use lobby_broker::LobbyClientMessage as L;
    Some(match msg {
        ClientMessage::ClientHello {
            client_version,
            build_commit,
            protocol_version,
            lobby_protocol_version,
        } => L::ClientHello {
            client_version: client_version.clone(),
            build_commit: build_commit.clone(),
            protocol_version: *protocol_version,
            lobby_protocol_version: *lobby_protocol_version,
        },
        ClientMessage::SubscribeLobby => L::SubscribeLobby,
        ClientMessage::UnsubscribeLobby => L::UnsubscribeLobby,
        ClientMessage::Ping { timestamp } => L::Ping {
            timestamp: *timestamp,
        },
        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            public,
            password,
            timer_seconds,
            player_count,
            match_config,
            ai_seats: _,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            start_when_full,
            ranked,
        } => L::CreateGameWithSettings {
            deck: deck.clone(),
            display_name: display_name.clone(),
            public: *public,
            password: password.clone(),
            timer_seconds: *timer_seconds,
            player_count: *player_count,
            match_config: *match_config,
            format_config: format_config.clone(),
            room_name: room_name.clone(),
            host_peer_id: host_peer_id.clone(),
            draft_metadata: draft_metadata.clone(),
            start_when_full: *start_when_full,
            ranked: *ranked,
        },
        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => L::JoinGameWithPassword {
            game_code: game_code.clone(),
            deck: deck.clone(),
            display_name: display_name.clone(),
            password: password.clone(),
            reservation_token: reservation_token.clone(),
        },
        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            reserve,
            display_name,
            release_reservation_token,
        } => L::LookupJoinTarget {
            game_code: game_code.clone(),
            password: password.clone(),
            reserve: *reserve,
            display_name: display_name.clone(),
            release_reservation_token: release_reservation_token.clone(),
        },
        ClientMessage::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => L::UpdateLobbyMetadata {
            game_code: game_code.clone(),
            current_players: *current_players,
            max_players: *max_players,
            consumed_reservation_tokens: consumed_reservation_tokens.clone(),
        },
        ClientMessage::UnregisterLobby { game_code } => L::UnregisterLobby {
            game_code: game_code.clone(),
        },
        _ => return None,
    })
}

/// Run a lobby-broker dispatch end to end: project the message, hold the lobby
/// lock for the synchronous `Broker::handle`, drop it, then interpret the
/// returned outbounds. Centralizes the lock/sync-back discipline so each arm is
/// a one-liner.
async fn dispatch_broker(
    msg: &ClientMessage,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    if let Err(reason) = guard_broker_projection_inbound(msg) {
        let _ = tx.send(ServerMessage::error(reason));
        return;
    }
    let Some(lobby_msg) = to_lobby_client_message(msg) else {
        return;
    };
    dispatch_broker_msg(
        lobby_msg,
        lobby,
        lobby_subscribers,
        player_count,
        tx,
        identity,
    )
    .await;
}

/// Lower-level broker dispatch taking an already-projected
/// [`lobby_broker::LobbyClientMessage`]. Used by arms that destructured the
/// owned `ClientMessage` (so `&client_msg` is no longer available) but whose
/// LobbyOnly path delegates to the broker.
async fn dispatch_broker_msg(
    lobby_msg: lobby_broker::LobbyClientMessage,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    let mut conn = identity.to_conn_state();
    let outbounds = {
        let mut broker = lobby.lock().await;
        broker.handle(&mut conn, lobby_msg, &SysEnv)
    };
    identity.absorb_conn_state(conn);
    apply_outbounds(outbounds, tx, lobby_subscribers, player_count).await;
}

/// Interpret an ordered `Vec<Outbound>` from the broker over the shell's
/// transport. `ToSelf` point replies go through this connection's mpsc sender
/// (same path the pre-extraction SubscribeLobby used); fan-out and
/// subscriber/count side effects use the existing `lobby_subscribers` /
/// `player_count` machinery. Order is preserved exactly as returned.
async fn apply_outbounds(
    outbounds: Vec<Outbound>,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
) {
    for ob in outbounds {
        match ob {
            Outbound::ToSelf(msg) => {
                // Point replies go through this connection's mpsc sender (drained
                // by the select loop), exactly as the pre-extraction
                // SubscribeLobby path did. Using `tx` rather than a direct
                // `socket.send` preserves ordering relative to concurrently
                // broadcast frames that may also land in this conn's queue.
                let _ = tx.send(to_server_message(msg));
            }
            Outbound::ToSubscribers(msg) => {
                broadcast_to_lobby_subscribers(lobby_subscribers, to_server_message(msg)).await;
            }
            Outbound::AddSubscriber => {
                if let Err(reason) = reserve_lobby_subscriber_slot(lobby_subscribers, tx).await {
                    let _ = tx.send(ServerMessage::error(reason));
                    continue;
                }
            }
            Outbound::RemoveSubscriber => {
                let mut subs = lobby_subscribers.lock().await;
                subs.retain(|s| !s.same_channel(tx) && !s.is_closed());
            }
            Outbound::SendPlayerCountToSelf => {
                let count = player_count.load(Ordering::Relaxed);
                let _ = tx.send(ServerMessage::PlayerCount { count });
            }
        }
    }
}

/// Binds a freshly allocated Full key to its authoritative runtime before any
/// connection can publish it. The initial snapshot uses the same keyed writer
/// as every later mutation; single-user activation additionally obtains the
/// durable singleton epoch that fences a replaced native game.
fn initialize_full_runtime(
    game_db: &SharedGameDb,
    session: &mut GameSession,
    key: server_core::FullSessionKey,
) -> Result<(), String> {
    session.full_runtime = Some(FullRuntime {
        key,
        activation_epoch: None,
    });
    let snapshot = session
        .full_persist_snapshot()
        .expect("Full runtime was just initialized");
    if game_db.is_single_user() {
        let (epoch, disposition) = game_db
            .activate_single_user_session(&snapshot)
            .map_err(|error| format!("Failed to activate Full session: {error}"))?;
        if disposition != server_core::FullPersistDisposition::Applied {
            return Err("Full session activation was superseded".to_string());
        }
        session
            .full_runtime
            .as_mut()
            .expect("Full runtime remains installed")
            .activation_epoch = Some(epoch);
    } else if game_db
        .save_full_session(&snapshot)
        .map_err(|error| format!("Failed to persist Full session: {error}"))?
        != server_core::FullPersistDisposition::Applied
    {
        return Err("Full session persistence was superseded".to_string());
    }
    Ok(())
}

/// Fire-and-forget generation- and revision-fenced persistence of an active
/// Full session. Terminal code never calls this writer after preparation.
fn persist_full_session_async(game_db: &SharedGameDb, session: &GameSession) {
    let db = game_db.clone();
    let Some(snapshot) = session.full_persist_snapshot() else {
        warn!(game = %session.game_code, "skipping persistence for unbound Full session");
        return;
    };
    tokio::task::spawn_blocking(move || match db.save_full_session(&snapshot) {
        Ok(server_core::FullPersistDisposition::Applied) => {}
        Ok(disposition) => warn!(
            game = %snapshot.key.game_code,
            ?disposition,
            "Full snapshot was no longer current"
        ),
        Err(error) => error!(
            game = %snapshot.key.game_code,
            %error,
            "failed to persist Full session"
        ),
    });
}

/// Session-configuration inputs for [`create_and_connect_multiplayer_session`].
struct MultiplayerSessionRequest {
    resolved: engine::game::deck_loading::PlayerDeckPayload,
    display_name: String,
    timer_seconds: Option<u32>,
    pc: u8,
    match_config: engine::types::match_config::MatchConfig,
    format_config: Option<engine::types::format::FormatConfig>,
    start_when_full: bool,
    ranked: bool,
    ai_requests: Vec<(
        u8,
        phase_ai::config::AiDifficulty,
        engine::game::deck_loading::PlayerDeckPayload,
    )>,
    public: bool,
    password: Option<String>,
    host_tx: mpsc::UnboundedSender<ServerMessage>,
}

/// Phases 1–2 of the `CreateGameWithSettings` full multiplayer path.
///
/// Phase 1 (state lock): creates the session, configures AI seats and lobby
/// metadata, and extracts the initial player count. The state guard is
/// unconditionally dropped at the end of the inner block before this function
/// returns.
///
/// Phase 2 (connections lock): registers the host's sender. The connections
/// guard is unconditionally dropped at the end of its inner block.
///
/// Both locks are therefore free when this function returns, so callers may
/// safely call `broadcast_player_slots` immediately after — that function
/// re-acquires both. This extraction exists so that the test in
/// `issue_4548_deadlock_tests` exercises the exact same lock-scoping code that
/// the handler uses; a regression that holds either guard across the return
/// boundary would deadlock the test's subsequent `broadcast_player_slots` call.
async fn create_and_connect_multiplayer_session(
    state: &SharedState,
    connections: &SharedConnections,
    game_db: &SharedGameDb,
    req: MultiplayerSessionRequest,
) -> Result<(String, String, u32, server_core::FullSessionKey), String> {
    let MultiplayerSessionRequest {
        resolved,
        display_name,
        timer_seconds,
        pc,
        match_config,
        format_config,
        start_when_full,
        ranked,
        ai_requests,
        public,
        password,
        host_tx,
    } = req;

    // Phase 1 ── state lock; released at end of block.
    let (game_code, player_token, initial_player_count, full_key) = {
        let mut mgr = state.lock().await;
        let (game_code, player_token) = mgr.create_game_n_players(
            resolved,
            display_name.clone(),
            timer_seconds,
            pc,
            match_config,
            format_config,
        )?;
        let full_key = match game_db.create_full_session_key(&game_code) {
            Ok(key) => key,
            Err(error) => {
                mgr.remove_game(&game_code);
                return Err(format!("Failed to bind game session identity: {error}"));
            }
        };
        info!(game = %game_code, host = %display_name, players = pc, "game created via lobby");

        if let Some(session) = mgr.sessions.get_mut(&game_code) {
            session.start_when_full = start_when_full;
            session.ranked = ranked;
            for (seat_index, difficulty, deck) in &ai_requests {
                let seat = *seat_index as usize;
                session.display_names[seat] = format!("AI ({difficulty:?})");
                session.connected[seat] = true;
                session.decks[seat] = Some(deck.clone());
                let pid = PlayerId(*seat_index);
                session.ai_seats.insert(pid);
                let config = phase_ai::config::create_config_for_players(
                    *difficulty,
                    phase_ai::config::Platform::Native,
                    pc,
                );
                session.ai_configs.insert(pid, config);
            }
        }

        let initial_player_count = mgr
            .sessions
            .get(&game_code)
            .map(|s| s.current_player_count())
            .unwrap_or(1);

        if let Some(session) = mgr.sessions.get_mut(&game_code) {
            session.lobby_meta = Some(server_core::PersistedLobbyMeta {
                host_name: display_name.clone(),
                public,
                password,
                timer_seconds,
                start_when_full,
                ranked,
            });
            if let Err(error) = initialize_full_runtime(game_db, session, full_key.clone()) {
                mgr.remove_game(&game_code);
                return Err(error);
            }
        }

        (game_code, player_token, initial_player_count, full_key)
    }; // state lock released here

    // Phase 2 ── connections lock; released at end of block.
    {
        let mut conns = connections.lock().await;
        conns
            .entry(game_code.clone())
            .or_default()
            .insert(PlayerId(0), host_tx);
    } // connections lock released here

    Ok((game_code, player_token, initial_player_count, full_key))
}

/// Broadcast `DraftSpectatorView` to all spectators watching a draft.
/// Prunes disconnected spectators (closed sender channels).
async fn broadcast_draft_spectator_views(
    draft_code: &str,
    draft_state: &SharedDraftState,
    draft_spectators: &SharedDraftSpectators,
) {
    let mut specs = draft_spectators.lock().await;
    let Some(spectators) = specs.get_mut(draft_code) else {
        return;
    };

    let mgr = draft_state.lock().await;
    let Some(session) = mgr.sessions.get(draft_code) else {
        return;
    };

    // Retain only live senders, sending views to each
    spectators.retain(|(visibility, sender)| {
        let view = draft_core::view::filter_for_spectator(&session.session, *visibility);
        let msg = ServerMessage::DraftSpectatorView { view };
        sender.send(msg).is_ok()
    });

    // Clean up empty entries
    if spectators.is_empty() {
        specs.remove(draft_code);
    }
}

/// Fire-and-forget persistence of a draft session to SQLite.
async fn persist_draft_session_async(
    game_db: &SharedGameDb,
    draft_code: &str,
    draft_state: &SharedDraftState,
) {
    let mgr = draft_state.lock().await;
    let Some(session) = mgr.sessions.get(draft_code) else {
        return;
    };
    let snapshot = session.to_persisted();
    let db = game_db.clone();
    let code = draft_code.to_string();
    tokio::task::spawn_blocking(move || match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if let Err(e) = db.save_draft_session(&code, &json) {
                error!(draft = %code, error = %e, "failed to persist draft session");
            }
        }
        Err(e) => {
            error!(draft = %code, error = %e, "failed to serialize draft session");
        }
    });
}

/// Immutable terminal result prepared before any recipient is told that a
/// started Full session ended. The database transaction retires the runtime,
/// creates recipient-scoped capabilities, and makes retries idempotent.
fn terminal_artifact(
    session: &GameSession,
    winner: Option<PlayerId>,
    reason: String,
    ranked_result: Option<Vec<RankedPlayerResult>>,
) -> Result<persistence::FullTerminalArtifact, String> {
    let runtime = session
        .full_runtime
        .as_ref()
        .ok_or_else(|| "Full session has no runtime identity".to_string())?;
    let recipients = session
        .player_tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| !token.is_empty())
        .map(|(player_id, token)| persistence::TerminalRecipient {
            player_id: PlayerId(player_id as u8),
            pre_terminal_player_token: token.clone(),
        })
        .collect();
    Ok(persistence::FullTerminalArtifact {
        key: runtime.key.clone(),
        terminal_revision: session.state_revision,
        display: server_core::TerminalMatchDisplay {
            winner,
            reason,
            ranked_result,
        },
        recipients,
    })
}

async fn prepare_full_terminal(
    game_db: &SharedGameDb,
    artifact: persistence::FullTerminalArtifact,
) -> Result<Vec<(PlayerId, server_core::CurrentTerminalDelivery)>, String> {
    let db = game_db.clone();
    tokio::task::spawn_blocking(move || {
        db.prepare_full_terminal(&artifact)
            .map_err(|error| format!("Failed to prepare terminal result: {error}"))?;
        artifact
            .recipients
            .iter()
            .map(|recipient| {
                db.current_terminal_delivery_for_recipient(
                    &artifact.key,
                    recipient.player_id,
                    &recipient.pre_terminal_player_token,
                )
                .map_err(|error| format!("Failed to load terminal delivery: {error}"))?
                .map(|delivery| (recipient.player_id, delivery))
                .ok_or_else(|| "Prepared terminal delivery is missing".to_string())
            })
            .collect()
    })
    .await
    .map_err(|error| format!("Terminal persistence task failed: {error}"))?
}

/// Only a waiting-room session can be retired without a recipient delivery:
/// it has never entered engine play and therefore has no terminal outcome.
fn retire_unstarted_session_async(game_db: &SharedGameDb, session: &GameSession) {
    let Some(runtime) = session.full_runtime.clone() else {
        warn!(game = %session.game_code, "unbound waiting-room session was removed");
        return;
    };
    let db = game_db.clone();
    let game_code = session.game_code.clone();
    tokio::task::spawn_blocking(move || {
        match db.retire_unstarted_full_session(&runtime.key, runtime.activation_epoch) {
            Ok(server_core::FullPersistDisposition::Applied) => {}
            Ok(disposition) => {
                warn!(game = %game_code, ?disposition, "waiting-room retirement was not current")
            }
            Err(error) => {
                error!(game = %game_code, %error, "failed to retire waiting-room session")
            }
        }
    });
}

#[derive(Debug, Clone)]
struct RankedDuelPlayers {
    player_a_name: String,
    player_b_name: String,
}

fn normalize_player_key(name: &str) -> Option<String> {
    let trimmed = name.trim().to_lowercase();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn expected_score(a: i32, b: i32) -> f64 {
    1.0 / (1.0 + 10f64.powf((b - a) as f64 / 400.0))
}

fn k_factor(rating: i32) -> i32 {
    if rating < 1200 {
        40
    } else {
        24
    }
}

fn ranked_duel_players_for_room(
    ranked: bool,
    player_count: u8,
    has_ai_seats: bool,
    display_names: &[String],
) -> Option<RankedDuelPlayers> {
    if !ranked || player_count != 2 || has_ai_seats {
        return None;
    }
    Some(RankedDuelPlayers {
        player_a_name: display_names.first()?.clone(),
        player_b_name: display_names.get(1)?.clone(),
    })
}

fn ranked_duel_players(session: &GameSession) -> Option<RankedDuelPlayers> {
    ranked_duel_players_for_room(
        session.ranked,
        session.player_count,
        !session.ai_seats.is_empty(),
        &session.display_names,
    )
}

fn ranked_result_for_duel(
    game_db: &SharedGameDb,
    game_code: &str,
    players: &RankedDuelPlayers,
    winner: Option<PlayerId>,
) -> Option<Vec<RankedPlayerResult>> {
    let score_a = match winner {
        Some(PlayerId(0)) => 1.0,
        Some(PlayerId(1)) => 0.0,
        _ => 0.5,
    };
    let score_b = 1.0 - score_a;
    let key_a = normalize_player_key(&players.player_a_name)?;
    let key_b = normalize_player_key(&players.player_b_name)?;
    if key_a == key_b {
        return None;
    }

    let ra = game_db.load_rating(&key_a).ok().flatten().unwrap_or(1200);
    let rb = game_db.load_rating(&key_b).ok().flatten().unwrap_or(1200);
    let ea = expected_score(ra, rb);
    let eb = expected_score(rb, ra);
    let da = (k_factor(ra) as f64 * (score_a - ea)).round() as i32;
    let db = (k_factor(rb) as f64 * (score_b - eb)).round() as i32;
    let ra_next = ra + da;
    let rb_next = rb + db;
    let deltas = vec![
        persistence::RatingDelta {
            player_key: key_a.clone(),
            game_code: game_code.to_string(),
            opponent_key: key_b.clone(),
            won: score_a > score_b,
            rating_before: ra,
            rating_after: ra_next,
            rating_delta: da,
        },
        persistence::RatingDelta {
            player_key: key_b,
            game_code: game_code.to_string(),
            opponent_key: key_a.clone(),
            won: score_b > score_a,
            rating_before: rb,
            rating_after: rb_next,
            rating_delta: db,
        },
    ];
    let saved = match game_db.save_ranked_result_idempotent(&deltas) {
        Ok(saved) => saved,
        Err(e) => {
            error!(game = %game_code, error = %e, "failed to save ranked result");
            return None;
        }
    };
    Some(
        saved
            .into_iter()
            .enumerate()
            .map(|(player_id, delta)| RankedPlayerResult {
                player_id: player_id as u8,
                rating_before: delta.rating_before,
                rating_after: delta.rating_after,
                rating_delta: delta.rating_delta,
            })
            .collect(),
    )
}

/// If this game_code belongs to a draft tournament, auto-report the match
/// result to the DraftSessionManager and broadcast updated views. This
/// implements Pitfall 6 from RESEARCH: clients must NOT send
/// ReportMatchResult for server-hosted drafts — the server handles it.
async fn report_draft_game_over(
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    game_code: &str,
    winner: Option<PlayerId>,
) {
    let draft_code = {
        let mgr = draft_state.lock().await;
        mgr.draft_for_game_code(game_code)
    };
    let Some(draft_code) = draft_code else {
        return;
    };

    // Find the match_id and winner_seat from the draft session
    let (match_id, winner_seat) = {
        let mgr = draft_state.lock().await;
        let Some(session) = mgr.sessions.get(&draft_code) else {
            return;
        };
        // Find the match_id that maps to this game_code
        let match_entry = session
            .active_matches
            .iter()
            .find(|(_, gc)| gc.as_str() == game_code);
        let Some((match_id, _)) = match_entry else {
            warn!(draft = %draft_code, game = %game_code, "game_code not found in active_matches");
            return;
        };
        let match_id = match_id.clone();

        // Map PlayerId winner to seat index
        let winner_seat = winner.map(|pid| pid.0);

        (match_id, winner_seat)
    };

    info!(
        draft = %draft_code,
        game = %game_code,
        match_id = %match_id,
        winner_seat = ?winner_seat,
        "auto-reporting draft match result from GameOver"
    );

    let views = {
        let mut mgr = draft_state.lock().await;
        let action = draft_core::types::DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        };
        match mgr.apply_system_action(&draft_code, action, None) {
            Ok(views) => views,
            Err(e) => {
                warn!(draft = %draft_code, error = %e, "failed to auto-report draft match result");
                return;
            }
        }
    };

    // Broadcast updated views to all draft pod members
    let conns = connections.lock().await;
    if let Some(players) = conns.get(&draft_code) {
        for (pid, sender) in players.iter() {
            let seat = pid.0 as usize;
            if let Some(view) = views.get(seat) {
                let _ = sender.send(ServerMessage::DraftStateUpdate { view: view.clone() });
            }
        }
    }
}

/// When the draft pod is pairing or in match play, generate pairings (server-internal)
/// and spawn 2-player game sessions for each pending table.
async fn maybe_spawn_draft_matches(
    draft_code: &str,
    draft_state: &SharedDraftState,
    game_state: &SharedState,
    db: &SharedDb,
    connections: &SharedConnections,
) {
    let spawns = {
        let mut draft_mgr = draft_state.lock().await;
        let mut game_mgr = game_state.lock().await;
        if let Err(error) = draft_mgr.ensure_pairings_generated(draft_code) {
            warn!(
                draft = %draft_code,
                error = %error,
                "failed to generate draft pairings"
            );
            return;
        }
        let round = draft_mgr
            .sessions
            .get(draft_code)
            .map(|s| s.session.current_round)
            .unwrap_or(1);
        match draft_mgr.spawn_match_games_for_round(draft_code, &mut game_mgr, db, round) {
            Ok(s) => s,
            Err(e) => {
                warn!(draft = %draft_code, error = %e, "draft match spawn skipped");
                return;
            }
        }
    };

    if spawns.is_empty() {
        return;
    }

    let conns = connections.lock().await;
    let Some(players) = conns.get(draft_code) else {
        return;
    };

    for spawn in spawns {
        info!(
            draft = %draft_code,
            match_id = %spawn.match_id,
            game = %spawn.game_code,
            "draft match game spawned"
        );
        for (player, seat) in [(&spawn.player_a, 0usize), (&spawn.player_b, 1usize)] {
            let msg = ServerMessage::DraftMatchStart {
                match_id: spawn.match_id.clone(),
                round: spawn.round,
                game_code: spawn.game_code.clone(),
                player_token: player.game_token.clone(),
                your_player: player.game_player,
                opponent_name: spawn.opponent_names[seat].clone(),
            };
            if let Some(sender) = players.get(&PlayerId(player.draft_seat)) {
                let _ = sender.send(msg);
            }
        }
    }
}

/// Broadcast `DraftStateUpdate` to all connected sockets in a draft pod.
/// Iterates the connections map and filters by `identity.draft_code` match.
/// Because `SocketIdentity` is per-socket state (not stored globally), we
/// instead iterate draft session seats and send per-seat views via the
/// connections map keyed by draft_code.
async fn broadcast_draft_views(
    draft_code: &str,
    views: &[draft_core::view::DraftPlayerView],
    connections: &SharedConnections,
    draft_state: &SharedDraftState,
) {
    let conns = connections.lock().await;
    // Draft connections are stored under the draft_code in the connections map
    if let Some(players) = conns.get(draft_code) {
        for (pid, sender) in players.iter() {
            let seat = pid.0 as usize;
            if let Some(view) = views.get(seat) {
                let msg = ServerMessage::DraftStateUpdate { view: view.clone() };
                let _ = sender.send(msg);
            }
        }
    } else {
        // Fallback: broadcast to all sockets that have a matching draft_code
        // by sending the first view (for reconnect cases where identity is set
        // but connection may not be in the draft_code map yet)
        let _ = draft_state; // suppress unused
    }
}

async fn broadcast_draft_timer_sync(
    draft_code: &str,
    remaining_ms: u32,
    connections: &SharedConnections,
) {
    let msg = ServerMessage::DraftTimerSync { remaining_ms };
    let conns = connections.lock().await;
    if let Some(players) = conns.get(draft_code) {
        for sender in players.values() {
            let _ = sender.send(msg.clone());
        }
    }
}

/// Spawn a pick timer task. When the timer expires, auto-pick a random card
/// for any seat that hasn't picked yet. Aborts the previous timer if one exists.
fn spawn_pick_timer(
    draft_state: SharedDraftState,
    connections: SharedConnections,
    draft_code: String,
    pick_seconds: u32,
) {
    let timer_draft_code = draft_code.clone();
    let timer_draft_state = draft_state.clone();
    let timer_connections = connections.clone();
    let pick_ms = pick_seconds.saturating_mul(1000);

    let handle = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_millis(pick_ms as u64);
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u128::from(u32::MAX)) as u32;

            {
                let mut mgr = timer_draft_state.lock().await;
                let Some(session) = mgr.sessions.get_mut(&timer_draft_code) else {
                    return;
                };
                if session.session.status != draft_core::types::DraftStatus::Drafting {
                    session.timer_remaining_ms = None;
                    return;
                }
                session.timer_remaining_ms = Some(remaining_ms);
            }

            broadcast_draft_timer_sync(&timer_draft_code, remaining_ms, &timer_connections).await;

            if remaining_ms == 0 {
                break;
            }
        }

        let mut mgr = timer_draft_state.lock().await;
        let Some(session) = mgr.sessions.get_mut(&timer_draft_code) else {
            return;
        };

        // Only auto-pick if still in Drafting status
        if session.session.status != draft_core::types::DraftStatus::Drafting {
            session.timer_remaining_ms = None;
            return;
        }

        info!(draft = %timer_draft_code, "pick timer expired — auto-picking for pending seats");

        let pod_size = session.player_tokens.len();
        let seats = draft_seats_needing_auto_pick(&mut session.session, pod_size);
        for seat_idx in seats {
            if let Some(pack) = &session.session.current_pack[seat_idx] {
                if !pack.0.is_empty() {
                    let card_idx = rand::rng().random_range(0..pack.0.len());
                    let card_id = pack.0[card_idx].instance_id.clone();
                    let action = draft_core::types::DraftAction::Pick {
                        seat: seat_idx as u8,
                        card_instance_id: card_id,
                    };
                    if let Err(e) = draft_core::session::apply(&mut session.session, action, None) {
                        warn!(
                            draft = %timer_draft_code,
                            seat = seat_idx,
                            error = %e,
                            "auto-pick failed"
                        );
                    }
                }
            }
        }

        session.timer_remaining_ms = None;

        // Broadcast updated views
        let views: Vec<_> = (0..pod_size).map(|i| session.view_for_seat(i)).collect();
        drop(mgr);

        {
            let conns = timer_connections.lock().await;
            if let Some(players) = conns.get(&timer_draft_code) {
                for (pid, sender) in players.iter() {
                    let seat = pid.0 as usize;
                    if let Some(view) = views.get(seat) {
                        let _ = sender.send(ServerMessage::DraftStateUpdate { view: view.clone() });
                    }
                }
            }
        }

        // Re-arm for the next pick window if the draft is still in progress.
        let still_drafting = {
            let mgr = timer_draft_state.lock().await;
            let status = mgr
                .sessions
                .get(&timer_draft_code)
                .map(|s| s.session.status);
            status == Some(draft_core::types::DraftStatus::Drafting)
        };
        if still_drafting {
            spawn_pick_timer(
                timer_draft_state.clone(),
                timer_connections.clone(),
                timer_draft_code.clone(),
                pick_seconds,
            );
        }
    });

    // Store the handle so it can be aborted if all picks come in early
    tokio::spawn(async move {
        let mut mgr = draft_state.lock().await;
        if let Some(session) = mgr.sessions.get_mut(&draft_code) {
            // Abort previous timer if any (T-59-07: prevent timer task accumulation)
            if let Some(prev) = session.timer_task.take() {
                prev.abort();
            }
            session.timer_remaining_ms = Some(pick_ms);
            session.timer_task = Some(handle);
        }
    });
}

type DraftPickWindow = (draft_core::types::DraftStatus, u8, u8);

fn should_rearm_pick_timer(
    before: Option<DraftPickWindow>,
    after: Option<DraftPickWindow>,
) -> bool {
    let Some(after) = after else {
        return false;
    };
    if after.0 != draft_core::types::DraftStatus::Drafting {
        return false;
    }
    match before {
        Some((draft_core::types::DraftStatus::Lobby, _, _)) => true,
        Some((draft_core::types::DraftStatus::Drafting, pack, pick)) => {
            after.1 != pack || after.2 != pick
        }
        _ => false,
    }
}

struct ServerDeckResolver<'a> {
    db: &'a CardDatabase,
}

impl DeckResolver for ServerDeckResolver<'_> {
    fn resolve(
        &self,
        choice: &DeckChoice,
    ) -> Result<engine::game::deck_loading::PlayerDeckList, String> {
        let deck = match choice {
            DeckChoice::Random => server_core::starter_decks::random_starter_deck(),
            DeckChoice::Named(name) => server_core::starter_decks::find_starter_deck(name)
                .ok_or_else(|| format!("Starter deck not found: {name}"))?,
            DeckChoice::DeckList(deck) => deck.as_ref().clone(),
        };
        // The reducer stays at the name-only layer (see `DeckResolver` docs),
        // but we MUST still validate the names against the card database here
        // — otherwise a deck containing unresolvable names propagates through
        // `apply_seat_delta` as `None`, and `start_game` silently substitutes
        // an empty `PlayerDeckPayload` (see `Session::start_game`). The result
        // is CR 704.5b losing every player on their first draw step with no
        // user-visible error. Validating here causes the reducer to return
        // `Err`, which phase-server then surfaces to the client.
        server_core::resolve_deck(self.db, &deck)?;
        Ok(engine::game::deck_loading::PlayerDeckList {
            main_deck: deck.main_deck,
            sideboard: deck.sideboard,
            commander: deck.commander,
            companion: deck.companion,
            planar_deck: deck.planar_deck,
            scheme_deck: deck.scheme_deck,
            attraction_deck: deck.attraction_deck,
            contraption_deck: deck.contraption_deck,
            sticker_sheets: deck.sticker_sheets,
            signature_spell: deck.signature_spell,
            bracket_tier: deck.bracket_tier,
        })
    }
}

async fn broadcast_game_started(
    state: &SharedState,
    connections: &SharedConnections,
    game_spectators: &SharedGameSpectators,
    game_db: &SharedGameDb,
    game_code: &str,
) {
    let (player_messages, spectator_msg) = {
        let mut mgr = state.lock().await;
        let Some(session) = mgr.sessions.get_mut(game_code) else {
            return;
        };

        session.run_ai();
        persist_full_session_async(game_db, session);
        (
            build_game_started_messages(session),
            build_spectator_game_started_message(session),
        )
    };

    {
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, msg) in &player_messages {
                if let Some(sender) = players.get(pid) {
                    let _ = sender.send(msg.clone());
                }
            }
        }
    }

    let spectator_msg = match spectator_msg {
        Ok(msg) => msg,
        Err(reason) => {
            warn!(game = %game_code, %reason, "skipping spectator GameStarted: snapshot too large");
            return;
        }
    };

    let mut specs = game_spectators.lock().await;
    if let Some(spectators) = specs.get_mut(game_code) {
        spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
        if spectators.is_empty() {
            specs.remove(game_code);
        }
    }
}

async fn require_host(identity: &SocketIdentity, socket: &mut WebSocket) -> Result<(), ()> {
    if identity.player_id != Some(PlayerId(0)) {
        let msg = ServerMessage::error("Only the host can modify seats.".to_string());
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return Err(());
    }
    Ok(())
}

fn is_joining_current_game(identity: &SocketIdentity, target_game_code: &str) -> bool {
    identity
        .game_code
        .as_deref()
        .is_some_and(|active| active == target_game_code)
        || identity
            .lobby_host_game
            .as_deref()
            .is_some_and(|hosted| hosted == target_game_code)
}

async fn reject_joining_current_game(
    identity: &SocketIdentity,
    target_game_code: &str,
    socket: &mut WebSocket,
) -> Result<(), ()> {
    if !is_joining_current_game(identity, target_game_code) {
        return Ok(());
    }

    let msg = ServerMessage::error("You are already in this game.".to_string());
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = socket.send(Message::text(json)).await;
    }
    Err(())
}

async fn draft_pack_generator_for_start(
    draft_state: &SharedDraftState,
    draft_pools: &SharedDraftPools,
    draft_code: &str,
) -> Result<draft_core::pack_generator::PackGenerator, String> {
    let set_code = {
        let mgr = draft_state.lock().await;
        let session = mgr
            .sessions
            .get(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        session.config.set_code.clone()
    };

    draft_pools
        .generator_for_set(&set_code)
        .ok_or_else(|| format!("No draft pool data for set: {set_code}"))
}

/// Per-AI-result fan-out for a batch of `run_ai` results.
///
/// Lifted verbatim out of `handle_full_game_submission` so the approved-takeback
/// path can reuse it rather than duplicate 110 lines: a rollback can restore an
/// AI seat to priority, and that AI's follow-up must reach clients with the same
/// 100 ms pacing, size guard, `is_last` legal-action gating, per-player filter
/// and spectator fan-out as a normal action's. Pure extraction — no behavioural
/// delta on the shipped path.
///
/// `rewind_targets` and `eliminated` are both captured under the same lock as
/// the results themselves, and both **after** `run_ai` — neither can be
/// recomputed here because this function holds no session. Taking either from a
/// pre-`run_ai` value would ship a list one transition stale: the AI's follow-up
/// can cross a turn (adding a rewind boundary) or finish a player off (adding an
/// elimination), and every `StateUpdate` in the batch would then contradict the
/// state travelling with it.
async fn broadcast_ai_results(
    connections: &SharedConnections,
    game_spectators: &SharedGameSpectators,
    game_code: &str,
    player_count: u8,
    eliminated: &[PlayerId],
    ai_results: &[RevisionedActionResult],
    rewind_targets: &[RewindOption],
) {
    // Broadcast AI follow-up results with delays
    for (i, (ai_revision, result)) in ai_results.iter().enumerate() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (
            ai_raw_state,
            ai_events,
            ai_legal,
            ai_log_entries,
            _ai_auto_pass,
            ai_spell_costs,
            ai_by_object,
        ) = result;
        if guard_state_snapshot_broadcast(StateSnapshotParts {
            state: ai_raw_state,
            events: ai_events,
            log_entries: ai_log_entries,
            legal_actions: ai_legal,
            legal_actions_by_object: ai_by_object,
            spell_costs: ai_spell_costs,
        })
        .is_err()
        {
            continue;
        }
        let is_last = i == ai_results.len() - 1;

        // Filter AI state per-player outside the lock
        let ai_filtered: Vec<(PlayerId, GameState)> = (0..player_count)
            .map(|j| {
                let pid = PlayerId(j);
                (pid, server_core::filter_state_for_player(ai_raw_state, pid))
            })
            .collect();

        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, pstate) in &ai_filtered {
                if let Some(s) = players.get(pid) {
                    let is_actor = server_core::is_acting(ai_raw_state, *pid);
                    let player_legals = if is_last && is_actor {
                        ai_legal.clone()
                    } else {
                        vec![]
                    };
                    let p_auto_pass = if is_last {
                        engine_auto_pass_for_viewer(ai_raw_state, *pid, ai_legal)
                    } else {
                        false
                    };
                    let p_end_continuous_effect_offers =
                        engine_end_continuous_effect_offers(&player_legals);
                    let p_mana_payment_shortcut_actions = if is_last && is_actor {
                        engine_mana_payment_shortcut_actions(ai_raw_state, ai_by_object)
                    } else {
                        Vec::new()
                    };
                    let p_spell_costs = if is_last && is_actor {
                        ai_spell_costs.clone()
                    } else {
                        HashMap::new()
                    };
                    let p_by_object = if is_last && is_actor {
                        ai_by_object.clone()
                    } else {
                        HashMap::new()
                    };
                    let _ = s.send(ServerMessage::StateUpdate {
                        state_revision: *ai_revision,
                        state: pstate.clone(),
                        events: server_core::filter_events_for_player(
                            ai_events,
                            ai_raw_state,
                            *pid,
                        ),
                        legal_actions: player_legals,
                        auto_pass_recommended: p_auto_pass,
                        end_continuous_effect_offers: p_end_continuous_effect_offers,
                        mana_payment_shortcut_actions: p_mana_payment_shortcut_actions,
                        eliminated_players: eliminated.to_vec(),
                        log_entries: ai_log_entries.clone(),
                        spell_costs: p_spell_costs,
                        legal_actions_by_object: object_action_payloads(&p_by_object),
                        derived: derive_transport_views(ai_raw_state, pstate, Some(*pid)),
                        viewer_interaction: derive_viewer_interaction(ai_raw_state, pstate, *pid),
                        rewind_targets: rewind_targets.to_vec(),
                    });
                }
            }
        }
        let (ai_raw_state, ai_events, _, ai_log_entries, _, _, _) = result;
        if let Ok(spectator_msg) = build_spectator_state_update_message(
            ai_raw_state,
            ai_events,
            ai_log_entries,
            *ai_revision,
        ) {
            let mut specs = game_spectators.lock().await;
            if let Some(spectators) = specs.get_mut(game_code) {
                spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
                if spectators.is_empty() {
                    specs.remove(game_code);
                }
            }
        }
    }
}

/// Broadcasts the result of an approved takeback (GH #1507): a `StateUpdate`
/// carrying the rolled-back state to every seat, filtered per-player exactly
/// like a normal action result, followed by `TakebackResolved { approved: true, .. }`.
/// `resolved_by` is the player whose response concluded the request, or
/// `None` when it resolved naturally (e.g. the requester was the sole human).
// Same shape as the sibling transport fan-outs at `:2010`/`:3767`/`:4071`:
// every argument is a distinct broadcast input captured under the session lock,
// and bundling them into a struct here would only move the arity, not reduce it.
#[allow(clippy::too_many_arguments)]
async fn broadcast_takeback_approved(
    connections: &SharedConnections,
    game_spectators: &SharedGameSpectators,
    game_code: &str,
    player_count: u8,
    state_revision: u64,
    snapshot: server_core::BroadcastSnapshot,
    resolved_by: Option<PlayerId>,
    rewind_targets: Vec<RewindOption>,
) {
    let (raw_state, legal_actions, _auto_pass, spell_costs, by_object) = snapshot;
    let filtered_states: Vec<(PlayerId, GameState)> = (0..player_count)
        .map(|i| {
            let pid = PlayerId(i);
            (pid, server_core::filter_state_for_player(&raw_state, pid))
        })
        .collect();

    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        for (pid, pstate) in &filtered_states {
            if let Some(s) = players.get(pid) {
                let is_actor = server_core::is_acting(&raw_state, *pid);
                let player_legals = if is_actor {
                    legal_actions.clone()
                } else {
                    vec![]
                };
                let p_auto_pass = engine_auto_pass_for_viewer(&raw_state, *pid, &legal_actions);
                let p_end_continuous_effect_offers =
                    engine_end_continuous_effect_offers(&player_legals);
                let p_mana_payment_shortcut_actions = if is_actor {
                    engine_mana_payment_shortcut_actions(&raw_state, &by_object)
                } else {
                    Vec::new()
                };
                let p_spell_costs = if is_actor {
                    spell_costs.clone()
                } else {
                    HashMap::new()
                };
                let p_by_object = if is_actor {
                    by_object.clone()
                } else {
                    HashMap::new()
                };
                let _ = s.send(ServerMessage::StateUpdate {
                    state_revision,
                    state: pstate.clone(),
                    events: vec![],
                    legal_actions: player_legals,
                    auto_pass_recommended: p_auto_pass,
                    end_continuous_effect_offers: p_end_continuous_effect_offers,
                    mana_payment_shortcut_actions: p_mana_payment_shortcut_actions,
                    eliminated_players: raw_state.eliminated_players.clone(),
                    log_entries: vec![],
                    spell_costs: p_spell_costs,
                    legal_actions_by_object: object_action_payloads(&p_by_object),
                    derived: derive_transport_views(&raw_state, pstate, Some(*pid)),
                    viewer_interaction: derive_viewer_interaction(&raw_state, pstate, *pid),
                    // Captured by the caller under the same lock as the
                    // rollback, and — like the shipped action path's own
                    // capture — *after* its `run_ai`, not before. Both halves
                    // matter. Under the lock, because an approved rewind prunes
                    // the ring and a list read outside it could advertise
                    // boundaries that no longer exist. After `run_ai`, because
                    // `run_ai` is itself a capture site: an AI follow-up that
                    // crosses a turn adds a boundary, and a pre-`run_ai` read
                    // would ship a list already one behind the state travelling
                    // with it. The list is a live session affordance, not a
                    // projection of `snapshot`.
                    rewind_targets: rewind_targets.clone(),
                });
            }
        }

        let resolved_msg = ServerMessage::TakebackResolved {
            approved: true,
            resolved_by,
        };
        for sender in players.values() {
            let _ = sender.send(resolved_msg.clone());
        }
    }
    drop(conns);

    // Spectators are read-only viewers of `StateUpdate` only (mirroring the
    // normal action-broadcast path) — they never receive player-facing
    // notifications like `TakebackResolved`, just like they never receive
    // `Conceded`/`GameOver`. Without this, a spectator would stay frozen on
    // the pre-rollback state until some later action produced a new update.
    if let Ok(spectator_msg) =
        build_spectator_state_update_message(&raw_state, &[], &[], state_revision)
    {
        let mut specs = game_spectators.lock().await;
        if let Some(spectators) = specs.get_mut(game_code) {
            spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
            if spectators.is_empty() {
                specs.remove(game_code);
            }
        }
    }
}

/// What a Full-mode game socket submitted: a client-materialized `GameAction`,
/// or an opaque engine-authored interaction response. Both are authenticated,
/// applied, and broadcast identically, so they share one handler. A bool or a
/// pair of `Option`s here would hide that the two are alternatives — and would
/// not give the payload-guard match below a total, wildcard-free form.
#[derive(Debug)]
enum GameSubmission {
    Action(GameAction),
    Interaction(InteractionSubmission),
}

impl GameSubmission {
    /// Wire-bounds this submission and, on failure, names the channel the
    /// rejection is answered on.
    ///
    /// Defense in depth: `guard_client_message_before_dispatch` already ran
    /// these exact bounds before dispatch, so in production this returns `Ok`
    /// for every frame that reaches the handler — the same standing as the
    /// `Action` guard it replaces. It is kept because the handler must not
    /// assume its caller ran them.
    ///
    /// The channels here MUST agree with
    /// `client_message_wire_guard::wire_rejection_message`, which answers the
    /// same failure at the wire. An oversized `GameAction` is a malformed
    /// frame — a client materializes actions from engine-published legal
    /// actions and cannot produce one by accident. An oversized interaction
    /// response is a rejected decision: `TextChoiceProjection::allow_arbitrary`
    /// accepts free-form text and the bound is 256 bytes, so an ordinary paste
    /// trips it and `ServerMessage::Error` would tear the session down.
    /// `handler_payload_channels_agree_with_the_wire` pins that agreement
    /// directly, by comparing this function's answer with
    /// `wire_rejection_message`'s for the same payload.
    ///
    /// The `Err` is boxed because `ServerMessage` is ~13 KiB (it carries a
    /// `GameState`), matching how this file already passes the type around
    /// (`game_started_msg: Box<ServerMessage>`).
    /// Stable `kind` label for the diagnostics this handler emits.
    ///
    /// Both submission variants share one handler, so an unlabelled event is
    /// unattributable: an operator triaging an interaction report greps for
    /// "interaction" and matches nothing. Deriving the label here keeps the two
    /// call sites from restating the variant set.
    fn kind(&self) -> &'static str {
        match self {
            GameSubmission::Action(_) => "action",
            GameSubmission::Interaction(_) => "interaction",
        }
    }

    /// Accepted zero-count debug creates are transport no-ops: server-core
    /// still authenticates and preflights them, but the Full-mode wrapper must
    /// not allocate a revision, run AI, persist, or broadcast unchanged state.
    fn is_zero_count_debug_create(&self) -> bool {
        matches!(
            self,
            GameSubmission::Action(GameAction::Debug(debug_action))
                if debug_action.is_zero_count_create()
        )
    }

    fn payload_rejection(&self) -> Result<(), Box<ServerMessage>> {
        match self {
            GameSubmission::Action(action) => guard_game_action_payload(action)
                .map_err(|reason| Box::new(ServerMessage::error(reason))),
            GameSubmission::Interaction(submission) => {
                guard_interaction_submission_payload(submission)
                    .map_err(|reason| Box::new(ServerMessage::ActionRejected { reason }))
            }
        }
    }
}

/// Apply one authenticated game submission from a Full-mode game socket, then
/// broadcast the resulting state to every participant and spectator.
///
/// Extracted verbatim from the `ClientMessage::Action` arm of
/// [`handle_client_message`] so that `ClientMessage::Interaction` can reuse the
/// identical authorization, application, and fan-out path instead of growing a
/// second ~400-line copy that would drift.
#[allow(clippy::too_many_arguments)]
async fn handle_full_game_submission(
    submission: GameSubmission,
    socket: &mut WebSocket,
    state: &SharedState,
    db: &SharedDb,
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    game_db: &SharedGameDb,
    game_spectators: &SharedGameSpectators,
    // Read-only: this handler reads `game_code`, `player_token`, and `player_id`
    // and mutates nothing. `&SocketIdentity` is deliberate, not an oversight --
    // `require_host`, `is_joining_current_game`, and their neighbours already
    // take it by shared reference; only `dispatch_broker` and
    // `handle_client_message` need `&mut`. Do not "fix" this to `&mut`.
    identity: &SocketIdentity,
) {
    let kind = submission.kind();
    let is_zero_count_debug_create = submission.is_zero_count_debug_create();
    let game_code = match &identity.game_code {
        Some(c) => c.clone(),
        None => {
            warn!(kind, "game submission received but not in a game");
            let msg = ServerMessage::error("Not in a game".to_string());
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
    };
    let player_token = match &identity.player_token {
        Some(t) => t.clone(),
        None => {
            let msg = ServerMessage::error("No player token".to_string());
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
    };

    debug!(game = %game_code, player = ?identity.player_id, submission = ?submission, "game submission");

    // Bound client-supplied payload sizes before the clone-heavy engine
    // reducers process them (mirrors guard_draft_action_payload for draft
    // actions). The channel each variant answers on is declared by
    // `GameSubmission::payload_rejection`.
    if let Err(msg) = submission.payload_rejection() {
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }

    // Apply human action and collect AI follow-up results while holding the lock.
    // Filtering is deferred until after the lock is dropped to reduce contention.
    let action_result = {
        let lock_start = std::time::Instant::now();
        let mut mgr = state.lock().await;
        let applied = match submission {
            GameSubmission::Action(action) => {
                mgr.handle_action_with_card_db(&game_code, &player_token, action, Some(db.as_ref()))
            }
            GameSubmission::Interaction(submission) => {
                mgr.handle_interaction(&game_code, &player_token, submission)
            }
        };
        match applied {
            Ok(human_result) => {
                if is_zero_count_debug_create {
                    drop(mgr);
                    let _ = tx.send(ServerMessage::ActionNoOp);
                    return;
                }
                let human_revision = mgr
                    .sessions
                    .get_mut(&game_code)
                    .expect("handled action must retain its session")
                    .advance_state_revision();
                // Run AI follow-up actions (still inside lock — needs &mut state)
                let ai_results = match mgr.sessions.get_mut(&game_code) {
                    Some(session) => session.run_ai(),
                    None => vec![],
                };
                let session = mgr.sessions.get(&game_code).unwrap();
                let eliminated = session.state.eliminated_players.clone();
                // Captured once, AFTER `run_ai`, and reused for both the human
                // and the AI fan-out below: the list is a live session
                // affordance rather than a per-snapshot projection, so the
                // freshest value under this lock is the correct one for every
                // message this transition produces.
                let rewind_targets = session.rewind_options();
                let player_count = session.player_count;
                let game_over_winner = match &session.state.waiting_for {
                    engine::types::game_state::WaitingFor::GameOver { winner } => Some(*winner),
                    _ => None,
                };
                let terminal = if let Some(winner) = game_over_winner {
                    info!(game = %game_code, winner = ?winner, reason = "game_rules", "game over");
                    let ranked_result = ranked_duel_players(session).and_then(|players| {
                        ranked_result_for_duel(game_db, &game_code, &players, winner)
                    });
                    terminal_artifact(session, winner, "Game ended".to_string(), ranked_result)
                        .map(Some)
                } else {
                    persist_full_session_async(game_db, session);
                    Ok(None)
                };

                let lock_ms = lock_start.elapsed().as_millis();
                info!(
                    game = %game_code,
                    kind,
                    lock_ms,
                    ai_actions = ai_results.len(),
                    "game submission processed (lock held)"
                );

                terminal.map(|terminal| {
                    (
                        human_revision,
                        human_result,
                        ai_results,
                        eliminated,
                        player_count,
                        game_over_winner,
                        terminal,
                        rewind_targets,
                    )
                })
            }
            Err(e) => Err(e),
        }
    }; // lock dropped — filtering happens below without blocking other games

    match action_result {
        Ok((
            human_revision,
            (
                raw_state,
                events,
                legal_actions,
                log_entries,
                _auto_pass_rec,
                spell_costs,
                legal_actions_by_object,
            ),
            ai_results,
            eliminated,
            player_count,
            game_over_winner,
            terminal,
            rewind_targets,
        )) => {
            if let Err(reason) = guard_state_snapshot_broadcast(StateSnapshotParts {
                state: &raw_state,
                events: &events,
                log_entries: &log_entries,
                legal_actions: &legal_actions,
                legal_actions_by_object: &legal_actions_by_object,
                spell_costs: &spell_costs,
            }) {
                warn!(game = %game_code, %reason, "action snapshot too large to broadcast");
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let terminal_deliveries = match terminal {
                Some(artifact) => match prepare_full_terminal(game_db, artifact).await {
                    Ok(deliveries) => deliveries,
                    Err(error) => {
                        error!(game = %game_code, %error, "terminal preparation failed");
                        let msg = ServerMessage::error(error);
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                },
                None => Vec::new(),
            };

            // Filter state per-player outside the lock
            let filtered_states: Vec<(PlayerId, GameState)> = (0..player_count)
                .map(|i| {
                    let pid = PlayerId(i);
                    (pid, server_core::filter_state_for_player(&raw_state, pid))
                })
                .collect();

            // Broadcast human action result
            {
                let conns = connections.lock().await;
                if let Some(players) = conns.get(&game_code) {
                    for (pid, pstate) in &filtered_states {
                        if let Some(s) = players.get(pid) {
                            let is_actor = server_core::is_acting(&raw_state, *pid);
                            let player_legals = if ai_results.is_empty() && is_actor {
                                legal_actions.clone()
                            } else {
                                // AI will act next — don't send legal actions yet
                                vec![]
                            };
                            let p_auto_pass = if ai_results.is_empty() {
                                engine_auto_pass_for_viewer(&raw_state, *pid, &legal_actions)
                            } else {
                                false
                            };
                            let p_end_continuous_effect_offers =
                                engine_end_continuous_effect_offers(&player_legals);
                            let p_mana_payment_shortcut_actions =
                                if ai_results.is_empty() && is_actor {
                                    engine_mana_payment_shortcut_actions(
                                        &raw_state,
                                        &legal_actions_by_object,
                                    )
                                } else {
                                    Vec::new()
                                };
                            let p_spell_costs = if ai_results.is_empty() && is_actor {
                                spell_costs.clone()
                            } else {
                                HashMap::new()
                            };
                            let p_by_object = if ai_results.is_empty() && is_actor {
                                legal_actions_by_object.clone()
                            } else {
                                HashMap::new()
                            };
                            let _ = s.send(ServerMessage::StateUpdate {
                                state_revision: human_revision,
                                state: pstate.clone(),
                                events: server_core::filter_events_for_player(
                                    &events, &raw_state, *pid,
                                ),
                                legal_actions: player_legals,
                                auto_pass_recommended: p_auto_pass,
                                end_continuous_effect_offers: p_end_continuous_effect_offers,
                                mana_payment_shortcut_actions: p_mana_payment_shortcut_actions,
                                eliminated_players: eliminated.clone(),
                                log_entries: log_entries.clone(),
                                spell_costs: p_spell_costs,
                                legal_actions_by_object: object_action_payloads(&p_by_object),
                                derived: derive_transport_views(&raw_state, pstate, Some(*pid)),
                                viewer_interaction: derive_viewer_interaction(
                                    &raw_state, pstate, *pid,
                                ),
                                rewind_targets: rewind_targets.clone(),
                            });
                        }
                    }
                }
            }
            if let Ok(spectator_msg) = build_spectator_state_update_message(
                &raw_state,
                &events,
                &log_entries,
                human_revision,
            ) {
                let mut specs = game_spectators.lock().await;
                if let Some(spectators) = specs.get_mut(&game_code) {
                    spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
                    if spectators.is_empty() {
                        specs.remove(&game_code);
                    }
                }
            }

            broadcast_ai_results(
                connections,
                game_spectators,
                &game_code,
                player_count,
                &eliminated,
                &ai_results,
                &rewind_targets,
            )
            .await;

            if !terminal_deliveries.is_empty() {
                let conns = connections.lock().await;
                if let Some(players) = conns.get(&game_code) {
                    for (player, delivery) in &terminal_deliveries {
                        if let Some(sender) = players.get(player) {
                            let _ = sender.send(ServerMessage::TerminalResult {
                                delivery: Some(delivery.clone()),
                            });
                        }
                    }
                }
                drop(conns);
                report_draft_game_over(
                    draft_state,
                    connections,
                    &game_code,
                    game_over_winner.flatten(),
                )
                .await;
                state.lock().await.remove_game(&game_code);
            }
        }
        Err(e) => {
            let msg = ServerMessage::ActionRejected { reason: e };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
        }
    }
}

/// Handle the authenticated native Resolve All capability. Unlike ordinary
/// actions, this sends one compact final state snapshot and a requester-only
/// progress acknowledgement rather than replaying the entire batch event log.
#[allow(clippy::too_many_arguments)]
async fn handle_resolve_all(
    request_id: u64,
    max_resolutions: u32,
    state: &SharedState,
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    game_db: &SharedGameDb,
    game_spectators: &SharedGameSpectators,
    identity: &SocketIdentity,
) {
    let (Some(game_code), Some(player_token), Some(requester)) = (
        identity.game_code.clone(),
        identity.player_token.clone(),
        identity.player_id,
    ) else {
        let msg = ServerMessage::ResolveAllRejected {
            request_id,
            reason: "Not in a game".to_string(),
        };
        let _ = tx.send(msg);
        return;
    };

    let processed = {
        let mut mgr = state.lock().await;
        match mgr.resolve_all_for_player(&game_code, &player_token, max_resolutions) {
            Ok((transition, summary)) => match transition {
                None => Ok((summary, None)),
                Some((_, (_, _, _, batch_log_entries, _, _, _))) => {
                    let session = mgr
                        .sessions
                        .get_mut(&game_code)
                        .expect("Resolve All retains its session");
                    // Resolve All is a shortcut through a human-authorized batch,
                    // not a replacement for the session's ordinary AI hand-off.
                    // Keep that hand-off under the same lock, then derive the one
                    // final payload from the current session rather than the batch
                    // transition it has already moved past.
                    let ai_results = session.run_ai();
                    let (raw_state, legal_actions, _auto_pass, spell_costs, by_object) =
                        session.current_broadcast_snapshot();
                    let revision = session.state_revision;
                    let log_entries = resolve_all_log_tail(&batch_log_entries, &ai_results);
                    let eliminated = session.state.eliminated_players.clone();
                    let rewind_targets = session.rewind_options();
                    let player_count = session.player_count;
                    let game_over_winner = match &session.state.waiting_for {
                        engine::types::game_state::WaitingFor::GameOver { winner } => Some(*winner),
                        _ => None,
                    };
                    let terminal = if let Some(winner) = game_over_winner {
                        let ranked_result = ranked_duel_players(session).and_then(|players| {
                            ranked_result_for_duel(game_db, &game_code, &players, winner)
                        });
                        terminal_artifact(session, winner, "Game ended".to_string(), ranked_result)
                            .map(Some)
                    } else {
                        persist_full_session_async(game_db, session);
                        Ok(None)
                    };
                    terminal.map(|terminal| {
                        (
                            summary,
                            Some((
                                revision,
                                raw_state,
                                legal_actions,
                                log_entries,
                                spell_costs,
                                by_object,
                                eliminated,
                                rewind_targets,
                                player_count,
                                game_over_winner,
                                terminal,
                            )),
                        )
                    })
                }
            },
            Err(error) => Err(error),
        }
    };

    let (summary, payload) = match processed {
        Ok(processed) => processed,
        Err(reason) => {
            let _ = tx.send(ServerMessage::ResolveAllRejected { request_id, reason });
            return;
        }
    };

    let acknowledgement = ServerMessage::ResolveAllResult {
        request_id,
        items_resolved: summary.items_resolved,
        total: summary.total,
    };
    let Some((
        revision,
        raw_state,
        legal_actions,
        log_entries,
        spell_costs,
        by_object,
        eliminated,
        rewind_targets,
        player_count,
        game_over_winner,
        terminal,
    )) = payload
    else {
        let _ = tx.send(acknowledgement);
        return;
    };

    if let Err(reason) = guard_state_snapshot_broadcast(StateSnapshotParts {
        state: &raw_state,
        events: &[],
        log_entries: &log_entries,
        legal_actions: &legal_actions,
        legal_actions_by_object: &by_object,
        spell_costs: &spell_costs,
    }) {
        warn!(game = %game_code, %reason, "Resolve All snapshot exceeds broadcast bounds after commit");
        let _ = tx.send(build_resolve_all_state_update_message(
            &raw_state,
            &log_entries,
            &legal_actions,
            &spell_costs,
            &by_object,
            revision,
            requester,
            eliminated.clone(),
            rewind_targets.clone(),
        ));
        let _ = tx.send(acknowledgement);
        return;
    }

    let terminal_deliveries = match terminal {
        Some(artifact) => match prepare_full_terminal(game_db, artifact).await {
            Ok(deliveries) => deliveries,
            Err(error) => {
                error!(game = %game_code, %error, "Resolve All terminal preparation failed after commit");
                let _ = tx.send(build_resolve_all_state_update_message(
                    &raw_state,
                    &log_entries,
                    &legal_actions,
                    &spell_costs,
                    &by_object,
                    revision,
                    requester,
                    eliminated.clone(),
                    rewind_targets.clone(),
                ));
                let _ = tx.send(acknowledgement);
                return;
            }
        },
        None => Vec::new(),
    };

    // Queue the requester's final state and acknowledgement through its direct
    // sender in order; the adapter resolves only after this cached snapshot.
    let requester_update = build_resolve_all_state_update_message(
        &raw_state,
        &log_entries,
        &legal_actions,
        &spell_costs,
        &by_object,
        revision,
        requester,
        eliminated.clone(),
        rewind_targets.clone(),
    );
    let _ = tx.send(requester_update);

    {
        let conns = connections.lock().await;
        if let Some(players) = conns.get(&game_code) {
            for player in 0..player_count {
                let player = PlayerId(player);
                if player == requester {
                    continue;
                }
                if let Some(sender) = players.get(&player) {
                    let _ = sender.send(build_resolve_all_state_update_message(
                        &raw_state,
                        &log_entries,
                        &legal_actions,
                        &spell_costs,
                        &by_object,
                        revision,
                        player,
                        eliminated.clone(),
                        rewind_targets.clone(),
                    ));
                }
            }
        }
    }
    if let Ok(spectator_update) =
        build_spectator_state_update_message(&raw_state, &[], &log_entries, revision)
    {
        let mut spectators = game_spectators.lock().await;
        if let Some(senders) = spectators.get_mut(&game_code) {
            senders.retain(|sender| sender.send(spectator_update.clone()).is_ok());
            if senders.is_empty() {
                spectators.remove(&game_code);
            }
        }
    }
    let _ = tx.send(acknowledgement);

    if !terminal_deliveries.is_empty() {
        let conns = connections.lock().await;
        if let Some(players) = conns.get(&game_code) {
            for (player, delivery) in &terminal_deliveries {
                if *player == requester {
                    let _ = tx.send(ServerMessage::TerminalResult {
                        delivery: Some(delivery.clone()),
                    });
                } else if let Some(sender) = players.get(player) {
                    let _ = sender.send(ServerMessage::TerminalResult {
                        delivery: Some(delivery.clone()),
                    });
                }
            }
        }
        drop(conns);
        report_draft_game_over(
            draft_state,
            connections,
            &game_code,
            game_over_winner.flatten(),
        )
        .await;
        state.lock().await.remove_game(&game_code);
        connections.lock().await.remove(&game_code);
        game_spectators.lock().await.remove(&game_code);
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_client_message(
    client_msg: ClientMessage,
    socket: &mut WebSocket,
    state: &SharedState,
    draft_state: &SharedDraftState,
    draft_pools: &SharedDraftPools,
    connections: &SharedConnections,
    db: &SharedDb,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    game_db: &SharedGameDb,
    draft_spectators: &SharedDraftSpectators,
    game_spectators: &SharedGameSpectators,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
    mode: Mode,
) {
    // Handshake gate: ClientHello must be the first message. See
    // `classify_hello_gate` for the full truth table.
    match classify_hello_gate(
        identity.client_hello.is_some(),
        &client_msg,
        hello_acceptance(mode),
    ) {
        HelloGateOutcome::Accept(info) => {
            info!(
                version = %info.client_version,
                commit = %info.build_commit,
                "ClientHello accepted"
            );
            identity.client_hello = Some(info);
            return;
        }
        HelloGateOutcome::RejectInvalidHello(reason) => {
            warn!(%reason, "ClientHello rejected at wire guard");
            let msg = ServerMessage::error(reason);
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        HelloGateOutcome::RejectProtocol { client, server } => {
            warn!(
                client_protocol = client,
                server_protocol = server,
                "protocol version mismatch at ClientHello"
            );
            // Branch on which side is older so the user-facing remedy points at
            // the right party. "Please update" is wrong when the *server* is
            // the older one (post-bump preview server rolled back, or operator
            // running a stale build behind a freshly-deployed client).
            let remedy = if client < server {
                "Please update your client."
            } else {
                "This server is older than your client; wait for the rollout to complete."
            };
            let msg = ServerMessage::error(format!(
                "Protocol version mismatch (client={client} server={server}). {remedy}"
            ));
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        HelloGateOutcome::RejectHandshakeRequired => {
            warn!("client sent non-hello message before ClientHello");
            let msg =
                ServerMessage::error("ClientHello required before any other message".to_string());
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        HelloGateOutcome::IgnoreRedundantHello => {
            debug!("ignoring redundant ClientHello");
            return;
        }
        HelloGateOutcome::PassThrough => {
            // Fall through to the regular dispatch below.
        }
    }

    // Mode gate: some messages are meaningless in one mode or the other.
    // Rejecting here keeps every handler below single-purpose — they never
    // need to second-guess whether the message should reach them.
    if let Some(reason) = reject_if_disabled(&client_msg, mode) {
        warn!(?mode, msg = ?std::mem::discriminant(&client_msg), %reason, "rejecting message disabled by server mode");
        let msg = ServerMessage::error(reason.to_string());
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }

    if let Err(reason) = guard_client_message_before_dispatch(&client_msg, mode) {
        // The answer channel is wire policy, declared per variant alongside the
        // bounds themselves. Every variant except `Interaction` keeps today's
        // `ServerMessage::error`.
        let msg = wire_rejection_message(&client_msg, reason);
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }

    match client_msg {
        ClientMessage::ClientHello { .. } => {
            // Unreachable: IgnoreRedundantHello above handled this case.
            debug!("unreachable ClientHello arm");
        }
        // These terminal-only messages deliberately do not touch identity,
        // SessionManager, connection registration, or reconnect leases. A
        // terminal-unavailable bootstrap leaves the caller free to open a
        // separate ordinary reconnect socket.
        ClientMessage::BootstrapTerminalDelivery { request } => {
            let response = match game_db.bootstrap_terminal_delivery(&request) {
                Ok(delivery) => ServerMessage::TerminalBootstrapResult { delivery },
                Err(error) => ServerMessage::error(format!("Terminal bootstrap rejected: {error}")),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }
        ClientMessage::ReadTerminalResult { credential } => {
            let response = match game_db.read_terminal_result(&credential) {
                Ok(delivery) => ServerMessage::TerminalResult { delivery },
                Err(error) => ServerMessage::error(format!("Terminal read rejected: {error}")),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }
        ClientMessage::AckTerminalDelivery {
            delivery_id,
            credential,
        } => {
            let response = match game_db.ack_terminal_delivery(&delivery_id, &credential) {
                Ok(true) => ServerMessage::TerminalDeliveryAcknowledged { delivery_id },
                Ok(false) => ServerMessage::error("Terminal acknowledgement rejected".to_string()),
                Err(error) => {
                    ServerMessage::error(format!("Terminal acknowledgement failed: {error}"))
                }
            };
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }
        ClientMessage::CreateGame { deck } => {
            info!(deck_size = deck.main_deck.len(), "CreateGame");
            if let Err(reason) = guard_legacy_deck(&deck) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            {
                let mgr = state.lock().await;
                if mgr.sessions.len() >= MAX_GAMES {
                    warn!(limit = MAX_GAMES, "max games reached, rejecting CreateGame");
                    let msg = ServerMessage::error(
                        "Server is at game capacity, please try again later".to_string(),
                    );
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(error = %e, "CreateGame: deck resolve failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let (game_code, player_token) = mgr.create_game(resolved);
            let full_key = match game_db.create_full_session_key(&game_code) {
                Ok(key) => key,
                Err(error) => {
                    mgr.remove_game(&game_code);
                    drop(mgr);
                    let msg = ServerMessage::error(format!(
                        "Failed to bind game session identity: {error}"
                    ));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };
            if let Err(error) = initialize_full_runtime(
                game_db,
                mgr.sessions
                    .get_mut(&game_code)
                    .expect("new game session must exist"),
                full_key.clone(),
            ) {
                mgr.remove_game(&game_code);
                drop(mgr);
                let msg = ServerMessage::error(error);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            info!(game = %game_code, "game created");

            identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

            let mut conns = connections.lock().await;
            conns
                .entry(game_code.clone())
                .or_default()
                .insert(PlayerId(0), tx.clone());

            let msg = ServerMessage::GameCreated {
                game_code: game_code.clone(),
                player_token: player_token.clone(),
                full_key: Some(full_key.clone()),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            let attached = ServerMessage::SessionAttached {
                game_code,
                player_id: PlayerId(0),
                player_token,
                full_key: Some(full_key),
            };
            if let Ok(json) = serde_json::to_string(&attached) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::JoinGame { game_code, deck } => {
            info!(game = %game_code, deck_size = deck.main_deck.len(), "JoinGame");
            if let Err(reason) = guard_legacy_join_game(&game_code, &deck) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGame: deck resolve failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            match mgr.join_game(&game_code, resolved) {
                Ok((player_token, _filtered_state)) => {
                    mgr.set_card_names(&game_code, db.card_names());
                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    let joiner = session.player_for_token(&player_token).unwrap();
                    let started_messages = if session.is_full() {
                        session.run_ai();
                        persist_full_session_async(game_db, session);
                        // The joiner is excluded from the fan-out send below
                        // (`pid != joiner`), so it receives the contest dice via
                        // its own message here. Snapshot the events before the
                        // fan-out drains `start_events`.
                        let joiner_events = session.start_events.clone();
                        let joiner_msg = build_game_started_message(
                            session,
                            joiner,
                            Some(player_token.clone()),
                            joiner_events,
                        );
                        Some((joiner_msg, build_game_started_messages(session)))
                    } else {
                        None
                    };
                    info!(game = %game_code, player = ?joiner, "player joined");
                    identity.set_session(game_code.clone(), joiner, player_token.clone());
                    drop(mgr);

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Only send GameStarted when the game is full (all seats claimed)
                    if let Some((msg, other_messages)) = started_messages {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Send GameStarted to all other connected players
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, msg) in other_messages {
                                if pid != joiner {
                                    if let Some(sender) = players.get(&pid) {
                                        let _ = sender.send(msg);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGame failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::PreviewManaPayment { request_id, action } => {
            let response = match (identity.game_code.clone(), identity.player_token.clone()) {
                (Some(game_code), Some(player_token)) => {
                    if let Err(reason) = guard_game_action_payload(&action) {
                        ServerMessage::ManaPaymentPreviewRejected { request_id, reason }
                    } else {
                        let mgr = state.lock().await;
                        match mgr.preview_mana_payment(&game_code, &player_token, &action) {
                            Ok(source_ids) => ServerMessage::ManaPaymentPreview {
                                request_id,
                                source_ids,
                            },
                            Err(reason) => {
                                ServerMessage::ManaPaymentPreviewRejected { request_id, reason }
                            }
                        }
                    }
                }
                _ => ServerMessage::ManaPaymentPreviewRejected {
                    request_id,
                    reason: "Not in a game".to_string(),
                },
            };

            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::Action { action } => {
            handle_full_game_submission(
                GameSubmission::Action(action),
                socket,
                state,
                db,
                draft_state,
                connections,
                tx,
                game_db,
                game_spectators,
                identity,
            )
            .await;
        }

        ClientMessage::ResolveAll {
            request_id,
            max_resolutions,
        } => {
            handle_resolve_all(
                request_id,
                max_resolutions,
                state,
                draft_state,
                connections,
                tx,
                game_db,
                game_spectators,
                identity,
            )
            .await;
        }

        ClientMessage::Interaction { submission } => {
            handle_full_game_submission(
                GameSubmission::Interaction(submission),
                socket,
                state,
                db,
                draft_state,
                connections,
                tx,
                game_db,
                game_spectators,
                identity,
            )
            .await;
        }

        ClientMessage::Reconnect {
            game_code,
            player_token,
            full_key,
        } => {
            info!(game = %game_code, "Reconnect attempt");

            if let Err(reason) = guard_game_reconnect(&game_code, &player_token) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            match game_db.load_active_full_key(&game_code) {
                Ok(Some(active_key)) if active_key == full_key => {}
                Ok(_) => {
                    let msg = ServerMessage::error(
                        "Reconnect session identity is no longer current".to_string(),
                    );
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                Err(error) => {
                    let msg = ServerMessage::error(format!(
                        "Failed to validate reconnect identity: {error}"
                    ));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            // Determine game phase and handle reconnect in a single lock
            // to avoid TOCTOU races (game could fill between check and action).
            enum ReconnectOutcome {
                HostingOk {
                    player: PlayerId,
                    slot_info: Vec<server_core::PlayerSlotInfo>,
                },
                InGame {
                    player: PlayerId,
                    game_started_msg: Box<ServerMessage>,
                    ai_result: Option<Box<server_core::RevisionedActionResult>>,
                    /// GH #1507: present when a takeback vote is in flight,
                    /// so the reconnecting socket gets the same prompt it
                    /// would have received had it stayed connected.
                    pending_takeback_msg: Option<Box<ServerMessage>>,
                    /// Captured under the same lock as `ai_result`, for the
                    /// `StateUpdate` fan-out to the *other* seats below. The
                    /// reconnecting socket gets its own copy inside
                    /// `game_started_msg`.
                    rewind_targets: Vec<RewindOption>,
                },
                Err(String),
            }

            let outcome = {
                let mut mgr = state.lock().await;
                let is_waiting = mgr
                    .sessions
                    .get(&game_code)
                    .map(|s| s.is_pregame())
                    .unwrap_or(false);

                if is_waiting {
                    // Hosting reconnect: game exists but hasn't started yet.
                    // Scope session borrow to avoid conflicting with reconnect manager.
                    let session_result = mgr.sessions.get_mut(&game_code).map(|session| {
                        let player = session.player_for_token(&player_token);
                        if let Some(p) = player {
                            session.connected[p.0 as usize] = true;
                            let slot_info = session.player_slot_info();
                            Ok((p, slot_info))
                        } else {
                            Err("Invalid player token".to_string())
                        }
                    });
                    match session_result {
                        Some(Ok((player, slot_info))) => {
                            mgr.reconnect.remove_disconnect(&game_code, player);
                            ReconnectOutcome::HostingOk { player, slot_info }
                        }
                        Some(Err(e)) => ReconnectOutcome::Err(e),
                        None => ReconnectOutcome::Err(format!("Game not found: {}", game_code)),
                    }
                } else {
                    // In-game reconnect: game is full and started
                    match mgr.handle_reconnect(&game_code, &player_token) {
                        Ok(_filtered_state) => {
                            let session = mgr.sessions.get_mut(&game_code).unwrap();
                            let player = session.player_for_token(&player_token).unwrap();
                            let ai_results = session.run_ai();
                            let ai_result = ai_results.last().cloned().map(Box::new);
                            if ai_result.is_some() {
                                persist_full_session_async(game_db, session);
                            }
                            // Reconnect: no contest dice (the player must not
                            // re-see the first-player roll).
                            let game_started_msg =
                                build_game_started_message(session, player, None, Vec::new());
                            let pending_takeback_msg =
                                session.pending_takeback_message().map(Box::new);
                            let rewind_targets = session.rewind_options();
                            ReconnectOutcome::InGame {
                                player,
                                game_started_msg: Box::new(game_started_msg),
                                ai_result,
                                pending_takeback_msg,
                                rewind_targets,
                            }
                        }
                        Err(e) => ReconnectOutcome::Err(e),
                    }
                }
            }; // lock dropped

            match outcome {
                ReconnectOutcome::HostingOk { player, slot_info } => {
                    info!(game = %game_code, player = ?player, "hosting reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token.clone());

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(player, tx.clone());
                    }

                    // Re-send GameCreated so the client resumes hosting state
                    let msg = ServerMessage::GameCreated {
                        game_code: game_code.clone(),
                        player_token: player_token.clone(),
                        full_key: Some(full_key.clone()),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    let attached = ServerMessage::SessionAttached {
                        game_code,
                        player_id: player,
                        player_token,
                        full_key: Some(full_key),
                    };
                    if let Ok(json) = serde_json::to_string(&attached) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Send current room state
                    let slots_msg = ServerMessage::PlayerSlotsUpdate { slots: slot_info };
                    let _ = tx.send(slots_msg);
                }

                ReconnectOutcome::InGame {
                    player,
                    game_started_msg,
                    ai_result,
                    pending_takeback_msg,
                    rewind_targets,
                } => {
                    info!(game = %game_code, player = ?player, "reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token);

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(player, tx.clone());

                        // Notify all other players about the reconnection
                        let reconnect_msg = ServerMessage::OpponentReconnected {
                            player: Some(player),
                        };
                        if let Some(game_conns) = conns.get(&game_code) {
                            for (&pid, sender) in game_conns.iter() {
                                if pid != player {
                                    let _ = sender.send(reconnect_msg.clone());
                                }
                            }
                        }
                    }

                    if let Ok(json) = serde_json::to_string(&game_started_msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // GH #1507: replay the pending takeback prompt, if any,
                    // so this socket isn't left with no way to approve or
                    // decline a vote that started while it was disconnected.
                    if let Some(msg) = pending_takeback_msg {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                    }

                    if let Some(result) = ai_result {
                        let (state_revision, result) = *result;
                        let conns = connections.lock().await;
                        if let Some(game_conns) = conns.get(&game_code) {
                            for (&pid, sender) in game_conns.iter() {
                                if pid != player {
                                    if let Ok(msg) = build_state_update_message(
                                        &result,
                                        state_revision,
                                        pid,
                                        rewind_targets.clone(),
                                    ) {
                                        let _ = sender.send(msg);
                                    }
                                }
                            }
                        }
                    }
                }

                ReconnectOutcome::Err(e) => {
                    error!(game = %game_code, error = %e, "reconnect failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::SubscribeLobby => {
            if let Err(reason) = reserve_lobby_subscriber_slot(lobby_subscribers, tx).await {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            // Mode-agnostic: lobby subscription behaves identically on Full and
            // LobbyOnly servers, so the broker is the single authority for the
            // LobbyUpdate snapshot + PlayerCount. The subscriber slot is reserved
            // before broker state is absorbed so a capacity rejection cannot leave
            // a ghost subscription in SocketIdentity.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::UnsubscribeLobby => {
            // Mode-agnostic: lobby (un)subscription behaves identically on Full
            // and LobbyOnly servers, so the broker is the single authority for
            // RemoveSubscriber.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            public,
            password,
            timer_seconds,
            player_count: requested_player_count,
            match_config,
            ai_seats,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            start_when_full,
            ranked,
        } => {
            info!(
                display_name = %display_name,
                public = public,
                has_password = password.is_some(),
                timer = ?timer_seconds,
                deck_size = deck.main_deck.len(),
                ai_seats = ai_seats.len(),
                has_peer_id = host_peer_id.as_deref().is_some_and(|s| !s.is_empty()),
                "CreateGameWithSettings"
            );

            // --- Lobby-only broker path ------------------------------
            //
            // In this mode the server doesn't run a game — it only publishes
            // the host's PeerJS peer ID so guests can dial them directly. The
            // broker owns peer-id validation, re-registration cleanup, the
            // capacity cap, registration, GameCreated reply, and the public
            // LobbyGameAdded fan-out (in order). Deck data, AI seats, and
            // format-legality are host-authoritative and irrelevant here.
            if matches!(mode, ServerMode::LobbyOnly) {
                // Validate deck bounds before cloning to reject oversized decks early
                if let Err(reason) = lobby_broker::validate_deck_payload("deck", &deck) {
                    let msg = ServerMessage::error(reason);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                dispatch_broker_msg(
                    lobby_broker::LobbyClientMessage::CreateGameWithSettings {
                        deck,
                        display_name,
                        public,
                        password,
                        timer_seconds,
                        player_count: requested_player_count,
                        match_config,
                        format_config,
                        room_name,
                        host_peer_id,
                        draft_metadata,
                        start_when_full,
                        ranked,
                    },
                    lobby,
                    lobby_subscribers,
                    player_count,
                    tx,
                    identity,
                )
                .await;
                return;
            }

            let pc = match guard_full_create_game_settings_inbound(
                lobby_broker::CreateGameSettingsInbound {
                    deck: &deck,
                    display_name: &display_name,
                    password: password.as_deref(),
                    timer_seconds,
                    player_count: requested_player_count,
                    format_config: format_config.as_ref(),
                    room_name: room_name.as_deref(),
                    host_peer_id: host_peer_id.as_deref(),
                    draft_metadata: draft_metadata.as_ref(),
                },
                &ai_seats,
            ) {
                Ok(pc) => pc,
                Err(reason) => {
                    let msg = ServerMessage::error(reason);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            {
                let mgr = state.lock().await;
                if mgr.sessions.len() >= MAX_GAMES {
                    warn!(
                        limit = MAX_GAMES,
                        "max games reached, rejecting CreateGameWithSettings"
                    );
                    let msg = ServerMessage::error(
                        "Server is at game capacity, please try again later".to_string(),
                    );
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(error = %e, "CreateGameWithSettings: deck resolve failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            // Validate player deck against the selected format
            if let Some(ref fc) = format_config {
                if fc.format == engine::types::format::GameFormat::Planechase
                    && !ai_seats.is_empty()
                {
                    let msg = ServerMessage::error(
                        "Planechase does not support AI seats yet".to_string(),
                    );
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                if let Err(reasons) = validate_name_deck_for_format_full(
                    db,
                    &deck.main_deck,
                    &deck.sideboard,
                    &deck.commander,
                    &deck.companion,
                    &deck.planar_deck,
                    &deck.scheme_deck,
                    &deck.signature_spell,
                    fc.format,
                    Some(match_config.match_type),
                    usize::from(pc),
                ) {
                    let msg = ServerMessage::deck_rejected(format!(
                        "Deck not legal for {}: {}",
                        fc.format.label(),
                        reasons.join("; ")
                    ));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            let mut ai_requests = Vec::new();
            for seat in &ai_seats {
                if seat.seat_index == 0 || seat.seat_index >= pc {
                    continue;
                }
                let ai_deck_data = match &seat.deck {
                    Some(DeckChoice::DeckList(deck)) => deck.as_ref().clone(),
                    Some(DeckChoice::Named(name)) => {
                        server_core::starter_decks::find_starter_deck(name).unwrap_or_else(|| {
                            warn!(deck = %name, "unknown AI deck name, using random");
                            server_core::starter_decks::random_starter_deck()
                        })
                    }
                    Some(DeckChoice::Random) | None => match &seat.deck_name {
                        Some(name) if name.eq_ignore_ascii_case("random") => {
                            server_core::starter_decks::random_starter_deck()
                        }
                        Some(name) => server_core::starter_decks::find_starter_deck(name)
                            .unwrap_or_else(|| {
                                warn!(deck = %name, "unknown AI deck name, using random");
                                server_core::starter_decks::random_starter_deck()
                            }),
                        None => server_core::starter_decks::random_starter_deck(),
                    },
                };
                if let Some(ref fc) = format_config {
                    if let Err(reasons) = validate_name_deck_for_format_full(
                        db,
                        &ai_deck_data.main_deck,
                        &ai_deck_data.sideboard,
                        &ai_deck_data.commander,
                        &ai_deck_data.companion,
                        &ai_deck_data.planar_deck,
                        &ai_deck_data.scheme_deck,
                        &ai_deck_data.signature_spell,
                        fc.format,
                        Some(match_config.match_type),
                        usize::from(pc),
                    ) {
                        let msg = ServerMessage::error(format!(
                            "AI deck for seat {} not legal for {}: {}",
                            seat.seat_index,
                            fc.format.label(),
                            reasons.join("; ")
                        ));
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
                let ai_resolved = match resolve_deck(db, &ai_deck_data) {
                    Ok(d) => d,
                    Err(e) => {
                        error!(error = %e, "AI deck resolve failed, cloning host deck");
                        resolved.clone()
                    }
                };
                ai_requests.push((seat.seat_index, seat.difficulty, ai_resolved));
            }

            if !ai_requests.is_empty() && ai_requests.len() as u8 == pc - 1 {
                // --- AI game path: create, start, and run initial AI actions ---
                let (game_code, player_token, full_key, game_started_msg) = {
                    let mut mgr = state.lock().await;
                    let (game_code, player_token) = match mgr.create_game_with_ai(
                        resolved,
                        display_name.clone(),
                        timer_seconds,
                        match_config,
                        ai_requests,
                        db.card_names(),
                        format_config.clone(),
                        db.as_ref(),
                    ) {
                        Ok(created) => created,
                        Err(error) => {
                            let _ = tx.send(ServerMessage::error(error));
                            return;
                        }
                    };

                    let full_key = match game_db.create_full_session_key(&game_code) {
                        Ok(key) => key,
                        Err(error) => {
                            mgr.remove_game(&game_code);
                            let _ = tx.send(ServerMessage::error(format!(
                                "Failed to bind game session identity: {error}"
                            )));
                            return;
                        }
                    };

                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    session.run_ai();
                    // Initial start of a Play-vs-AI game: the human seat sees
                    // the first-player contest dice. Drain so they are not
                    // re-sent on reconnect.
                    let start_events = std::mem::take(&mut session.start_events);
                    let game_started_msg =
                        build_game_started_message(session, PlayerId(0), None, start_events);

                    if let Err(error) = initialize_full_runtime(game_db, session, full_key.clone())
                    {
                        mgr.remove_game(&game_code);
                        let _ = tx.send(ServerMessage::error(error));
                        return;
                    }

                    (game_code, player_token, full_key, game_started_msg)
                }; // lock dropped

                identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

                {
                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(PlayerId(0), tx.clone());
                }

                // Send GameCreated, then GameStarted (no lobby registration for AI games)
                let created_msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token: player_token.clone(),
                    full_key: Some(full_key.clone()),
                };
                if let Ok(json) = serde_json::to_string(&created_msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                let attached_msg = ServerMessage::SessionAttached {
                    game_code: game_code.clone(),
                    player_id: PlayerId(0),
                    player_token,
                    full_key: Some(full_key),
                };
                if let Ok(json) = serde_json::to_string(&attached_msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                if let Ok(json) = serde_json::to_string(&game_started_msg) {
                    let _ = socket.send(Message::text(json)).await;
                }

                info!(game = %game_code, host = %display_name, "AI game started");
            } else {
                // --- Standard multiplayer path ---
                //
                // DEADLOCK PREVENTION: `broadcast_player_slots` re-acquires
                // both `state` and `connections`.  Each MutexGuard must be
                // fully dropped (not merely "last-used" by NLL) before the
                // call, because Tokio's async state machine can keep guards
                // alive across `.await` points even after their last
                // syntactic use.  All three locks are therefore held inside
                // explicit `{ }` blocks so the guard is unconditionally
                // released before the first `.await` that follows.
                //
                // Phase 1 ── create session, configure it, and extract every
                // value needed by later phases; state lock is held for this
                // entire phase and nowhere else.

                // Capture the format before `format_config` is consumed so we
                // can stamp it on the lobby entry below.
                let format_config_for_lobby = format_config.clone();

                // Phases 1–2: create+configure the session (state lock) and
                // register the host connection (connections lock).  Both locks
                // are released inside `create_and_connect_multiplayer_session`
                // before it returns, so `broadcast_player_slots` (Phase 4) can
                // re-acquire them without deadlocking.
                let (game_code, player_token, initial_player_count, full_key) =
                    match create_and_connect_multiplayer_session(
                        state,
                        connections,
                        game_db,
                        MultiplayerSessionRequest {
                            resolved,
                            display_name: display_name.clone(),
                            timer_seconds,
                            pc,
                            match_config,
                            format_config,
                            start_when_full,
                            ranked,
                            ai_requests,
                            public,
                            password: password.clone(), // original still needed for Phase 3
                            host_tx: tx.clone(),
                        },
                    )
                    .await
                    {
                        Ok(session) => session,
                        Err(error) => {
                            let msg = ServerMessage::error(error);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    };

                identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

                // Phase 3 ── register with lobby broker and snapshot the
                // public-game entry while the lobby lock is held; released
                // before the subsequent .await calls.

                // Pull the client's advertised build identity from the
                // stored ClientHello. `client_hello` is guaranteed Some here
                // because the handshake gate at the top of this function
                // rejects any non-hello frame when it's None.
                let (host_version, host_build_commit) = identity
                    .client_hello
                    .as_ref()
                    .map(|h| (h.client_version.clone(), h.build_commit.clone()))
                    .unwrap_or_default();
                let lobby_added_game = {
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    lob.register_game(
                        &game_code,
                        RegisterGameRequest {
                            host_name: display_name,
                            public,
                            password,
                            timer_seconds,
                            host_version,
                            host_build_commit,
                            // Initial count reflects the host plus any AI seats
                            // configured at creation time; further updates flow
                            // through `set_current_players` as guests join.
                            current_players: initial_player_count,
                            // Use the clamped `pc` (not the raw request) so the
                            // lobby listing's max_players matches the session's
                            // actual capacity. A hostile client sending
                            // `player_count: 100` would otherwise advertise
                            // "1/100 players" while the game ran with 6.
                            max_players: pc as u32,
                            format_config: format_config_for_lobby,
                            match_config,
                            // Trim then drop empty strings so the client can't
                            // smuggle a blank room_name that would render as an
                            // empty row title. `None` is the "use host name"
                            // fallback both here and in the client.
                            room_name: room_name
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string),
                            // Full-mode server runs the engine itself — no
                            // PeerJS peer is involved, so this stays empty.
                            host_peer_id: String::new(),
                            // Draft metadata is P2P-only for now; Full-mode
                            // servers don't host draft pods.
                            draft_metadata: None,
                            ranked,
                        },
                        &SysEnv,
                    );
                    // Snapshot the public-game entry while the lock is still
                    // held; avoids re-locking lobby after the broadcast below.
                    if public {
                        lob.public_game(&game_code)
                    } else {
                        None
                    }
                }; // lobby lock released here

                // Phase 4 ── all locks are free; send replies and broadcast.
                // `broadcast_player_slots` re-acquires state + connections —
                // both are available now.
                let msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token: player_token.clone(),
                    full_key: Some(full_key.clone()),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                let attached = ServerMessage::SessionAttached {
                    game_code: game_code.clone(),
                    player_id: PlayerId(0),
                    player_token,
                    full_key: Some(full_key),
                };
                if let Ok(json) = serde_json::to_string(&attached) {
                    let _ = socket.send(Message::text(json)).await;
                }

                // Send initial slot state so host sees themselves in the room.
                broadcast_player_slots(state, connections, &game_code).await;

                if let Some(game) = lobby_added_game {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameAdded { game },
                    )
                    .await;
                }

                let count = player_count.load(Ordering::Relaxed);
                broadcast_player_count(lobby_subscribers, count).await;
            }
        }

        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            reserve,
            display_name,
            release_reservation_token,
        } => {
            info!(game = %game_code, "LookupJoinTarget");

            if let Err(reason) = lobby_broker::guard_lookup_join_target_inbound(
                lobby_broker::LookupJoinTargetInbound {
                    game_code: &game_code,
                    password: password.as_deref(),
                    display_name: display_name.as_deref(),
                    release_reservation_token: release_reservation_token.as_deref(),
                },
            ) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            let mut reservation_token = None;
            let mut reservation_expires_at_ms = None;
            let mut reservation_counted_in_info = false;

            let mut info = {
                let lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby();

                let guest_commit = identity
                    .client_hello
                    .as_ref()
                    .map(|h| h.build_commit.as_str())
                    .unwrap_or("");
                let host_commit = lob.host_build_commit(&game_code).unwrap_or("");
                if let BuildCommitCheck::Reject { host, guest } =
                    check_build_commit(host_commit, guest_commit)
                {
                    warn!(game = %game_code, %host, %guest, "build mismatch — refusing lookup");
                    if let Ok(json) = serde_json::to_string(&ServerMessage::error(format!(
                        "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
                    ))) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                } else {
                    match lob.verify_password(&game_code, password.as_deref()) {
                        Ok(()) => match lob.join_target_info(&game_code) {
                            Some(info) => info,
                            None => {
                                let msg = ServerMessage::error(format!(
                                    "Game not found in lobby: {game_code}"
                                ));
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    let _ = socket.send(Message::text(json)).await;
                                }
                                return;
                            }
                        },
                        Err(e) if e == "password_required" => {
                            let msg = ServerMessage::PasswordRequired {
                                game_code: game_code.clone(),
                            };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                        Err(e) => {
                            warn!(game = %game_code, error = %e, "lookup password verification failed");
                            let msg = ServerMessage::error(e);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                }
            };

            if let Some(token) = release_reservation_token.as_deref() {
                let held = if info.is_p2p {
                    conn_holds_reservation(&identity.lobby_reservations, &game_code, token)
                } else {
                    conn_holds_reservation(&identity.seat_reservations, &game_code, token)
                };
                if !held {
                    let msg = ServerMessage::error(NOT_OWNED_RESERVATION.to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                if info.is_p2p {
                    let released = {
                        let mut lob = lobby.lock().await;
                        lob.lobby_mut().release_reservation(&game_code, token)
                    };
                    if released {
                        identity
                            .lobby_reservations
                            .retain(|(code, t)| code != &game_code || t != token);
                        let game = {
                            let lob = lobby.lock().await;
                            lob.lobby().public_game(&game_code)
                        };
                        if let Some(game) = game {
                            broadcast_to_lobby_subscribers(
                                lobby_subscribers,
                                ServerMessage::LobbyGameUpdated { game },
                            )
                            .await;
                        }
                    }
                } else {
                    let released = {
                        let mut mgr = state.lock().await;
                        mgr.release_reservation(&game_code, token)
                    };
                    if released {
                        identity
                            .seat_reservations
                            .retain(|(code, t)| code != &game_code || t != token);
                        broadcast_player_slots(state, connections, &game_code).await;
                        let updated = {
                            let current = {
                                let mgr = state.lock().await;
                                mgr.sessions
                                    .get(&game_code)
                                    .map(|session| session.current_player_count())
                            };
                            let mut lob_guard = lobby.lock().await;
                            let lob = lob_guard.lobby_mut();
                            if let Some(current) = current {
                                lob.set_current_players(&game_code, current, &SysEnv);
                            }
                            lob.public_game(&game_code)
                        };
                        if let Some(game) = updated {
                            broadcast_to_lobby_subscribers(
                                lobby_subscribers,
                                ServerMessage::LobbyGameUpdated { game },
                            )
                            .await;
                        }
                    }
                }
            }

            if reserve {
                let already_reserved = if info.is_p2p {
                    let mut lob = lobby.lock().await;
                    identity.lobby_reservations.retain(|(code, token)| {
                        if code != &game_code {
                            return true;
                        }
                        lob.lobby_mut().has_active_reservation(code, token, &SysEnv)
                    });
                    identity
                        .lobby_reservations
                        .iter()
                        .any(|(code, _)| code == &game_code)
                } else {
                    let mut mgr = state.lock().await;
                    identity.seat_reservations.retain(|(code, token)| {
                        if code != &game_code {
                            return true;
                        }
                        mgr.has_active_reservation(code, token)
                    });
                    identity
                        .seat_reservations
                        .iter()
                        .any(|(code, _)| code == &game_code)
                };
                if already_reserved {
                    let msg = ServerMessage::error(
                        "You already hold a reservation for this game".to_string(),
                    );
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                if info.is_p2p {
                    let reserve_result = {
                        let mut lob = lobby.lock().await;
                        lob.lobby_mut().reserve_seat(
                            &game_code,
                            display_name.unwrap_or_else(|| "Player".to_string()),
                            &SysEnv,
                        )
                    };
                    match reserve_result {
                        Ok(reservation) => {
                            reservation_token = Some(reservation.token.clone());
                            reservation_expires_at_ms = reservation.expires_at_ms;
                            identity
                                .lobby_reservations
                                .push((game_code.clone(), reservation.token));
                            let game = {
                                let lob = lobby.lock().await;
                                lob.lobby().public_game(&game_code)
                            };
                            if let Some(game) = game {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let msg = ServerMessage::error(e);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                } else {
                    let reserve_result = {
                        let mut mgr = state.lock().await;
                        mgr.reserve_seat(
                            &game_code,
                            display_name.unwrap_or_else(|| "Player".to_string()),
                        )
                    };
                    match reserve_result {
                        Ok(reservation) => {
                            reservation_token = Some(reservation.token.clone());
                            reservation_expires_at_ms = reservation.expires_at_ms;
                            identity
                                .seat_reservations
                                .push((game_code.clone(), reservation.token));
                            broadcast_player_slots(state, connections, &game_code).await;
                            let updated = {
                                let current = {
                                    let mgr = state.lock().await;
                                    mgr.sessions
                                        .get(&game_code)
                                        .map(|session| session.current_player_count())
                                };
                                let mut lob_guard = lobby.lock().await;
                                let lob = lob_guard.lobby_mut();
                                if let Some(current) = current {
                                    lob.set_current_players(&game_code, current, &SysEnv);
                                }
                                lob.public_game(&game_code)
                            };
                            if let Some(game) = updated {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let msg = ServerMessage::error(e);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                }
                let latest_info = {
                    let lob = lobby.lock().await;
                    lob.lobby().join_target_info(&game_code)
                };
                if let Some(latest_info) = latest_info {
                    info = latest_info;
                    reservation_counted_in_info = true;
                }
            } else if info.max_players > 0 && info.current_players >= info.max_players {
                let msg = ServerMessage::error(format!("Game {game_code} is full"));
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let msg = ServerMessage::JoinTargetInfo {
                game_code: game_code.clone(),
                is_p2p: info.is_p2p,
                format_config: info.format_config,
                match_config: info.match_config,
                player_count: info.max_players as u8,
                filled_seats: (info.current_players
                    + u32::from(reservation_token.is_some() && !reservation_counted_in_info))
                .min(info.max_players) as u8,
                reservation_token,
                reservation_expires_at_ms,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            info!(game = %game_code, is_p2p = info.is_p2p, "sent JoinTargetInfo");
        }

        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => {
            info!(game = %game_code, joiner = %display_name, "JoinGameWithPassword");

            // --- Lobby-only broker path ------------------------------
            //
            // The broker runs the build-commit + password gates, the
            // not-brokerable / seat-full checks, reservation consumption, and
            // hands back PeerInfo so the guest can dial over PeerJS. No session
            // is created server-side. The deck is ignored — the host validates
            // guest decks over P2P once the connection is up.
            if matches!(mode, ServerMode::LobbyOnly) {
                // Validate deck bounds before cloning to reject oversized decks early
                if let Err(reason) = lobby_broker::validate_deck_payload("deck", &deck) {
                    let msg = ServerMessage::error(reason);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                dispatch_broker_msg(
                    lobby_broker::LobbyClientMessage::JoinGameWithPassword {
                        game_code,
                        deck,
                        display_name,
                        password,
                        reservation_token,
                    },
                    lobby,
                    lobby_subscribers,
                    player_count,
                    tx,
                    identity,
                )
                .await;
                return;
            }

            if let Err(reason) = lobby_broker::guard_join_game_with_password_inbound(
                lobby_broker::JoinGameWithPasswordInbound {
                    game_code: &game_code,
                    deck: &deck,
                    display_name: &display_name,
                    password: password.as_deref(),
                    reservation_token: reservation_token.as_deref(),
                },
            ) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            {
                let lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby();

                // Build-commit gate: see `check_build_commit` for the
                // policy. If both host and guest publish commits and they
                // differ, the guest is running a different engine than the
                // host and joining would diverge GameState on resolution.
                let guest_commit = identity
                    .client_hello
                    .as_ref()
                    .map(|h| h.build_commit.as_str())
                    .unwrap_or("");
                let host_commit = lob.host_build_commit(&game_code).unwrap_or("");
                if let BuildCommitCheck::Reject { host, guest } =
                    check_build_commit(host_commit, guest_commit)
                {
                    warn!(game = %game_code, %host, %guest, "build mismatch — refusing join");
                    let msg = ServerMessage::error(format!(
                        "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
                    ));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                match lob.verify_password(&game_code, password.as_deref()) {
                    Ok(()) => {}
                    Err(e) if e == "password_required" => {
                        info!(game = %game_code, "password required, prompting client");
                        let msg = ServerMessage::PasswordRequired {
                            game_code: game_code.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                    Err(e) => {
                        warn!(game = %game_code, error = %e, "password verification failed");
                        let msg = ServerMessage::error(e);
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            }

            if let Some(token) = reservation_token.as_deref() {
                if !conn_holds_reservation(&identity.seat_reservations, &game_code, token) {
                    let msg = ServerMessage::error(NOT_OWNED_RESERVATION.to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGameWithPassword: deck resolve failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            enum JoinOutcome {
                Waiting {
                    player_token: String,
                    joiner: PlayerId,
                    full_key: server_core::FullSessionKey,
                    slot_info: Vec<server_core::PlayerSlotInfo>,
                    current_count: u32,
                    raw_state: Box<engine::types::game_state::GameState>,
                    filtered_state: Box<engine::types::game_state::GameState>,
                    state_revision: u64,
                },
                Started {
                    player_token: String,
                    joiner: PlayerId,
                    public_before: bool,
                },
            }

            // Collects a bracket-violation message to broadcast after the state lock releases and
            // after the joiner receives their direct error (mirrors the seat-delta path).
            let mut bracket_broadcast: Option<String> = None;

            let join_outcome = {
                let mut mgr = state.lock().await;
                match mgr.join_game_with_name_and_reservation(
                    &game_code,
                    resolved,
                    display_name,
                    reservation_token.clone(),
                ) {
                    Ok((player_token, filtered_state)) => {
                        mgr.set_card_names(&game_code, db.card_names());
                        let session = mgr.sessions.get_mut(&game_code).unwrap();
                        let joiner = session.player_for_token(&player_token).unwrap();
                        info!(game = %game_code, player = ?joiner, "player joined via lobby");

                        if let Some(token) = reservation_token.as_deref() {
                            identity
                                .seat_reservations
                                .retain(|(code, t)| code != &game_code || t != token);
                        }

                        let should_start = session.is_full() && session.start_when_full;
                        let public_before =
                            session.lobby_meta.as_ref().is_some_and(|meta| meta.public);
                        if should_start {
                            if let Err(bracket_err) = session.start_game(db.as_ref()) {
                                // start_game guarantees no mutation on Err, so the session still
                                // holds the joining player. We keep them seated — rolling back
                                // would require deleting their deck/token which is more invasive.
                                // The host can correct the deck(s) and trigger a new start.
                                persist_full_session_async(game_db, session);
                                // Capture the message so we can fan it out to all connected
                                // players after the state lock releases (mirrors seat-delta path).
                                bracket_broadcast =
                                    Some(format!("Cannot start cEDH game: {bracket_err}"));
                                // Evaluate to Err so the outer match join_outcome sends an Error
                                // message to the client via the existing Err(e) arm.
                                Err(format!("Cannot start cEDH game: {bracket_err}"))
                            } else {
                                // Persist updated session (now has the new player and is started)
                                persist_full_session_async(game_db, session);
                                Ok(JoinOutcome::Started {
                                    player_token,
                                    joiner,
                                    public_before,
                                })
                            }
                        } else {
                            // Persist updated session (now has the new player, not yet started)
                            persist_full_session_async(game_db, session);
                            match session.full_runtime.as_ref() {
                                Some(runtime) => Ok(JoinOutcome::Waiting {
                                    player_token,
                                    joiner,
                                    full_key: runtime.key.clone(),
                                    slot_info: session.player_slot_info(),
                                    current_count: session.current_player_count(),
                                    raw_state: Box::new(session.state.clone()),
                                    filtered_state: Box::new(filtered_state),
                                    state_revision: session.state_revision,
                                }),
                                None => Err("Full session runtime is unavailable".to_string()),
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            };

            match join_outcome {
                Ok(JoinOutcome::Waiting {
                    player_token,
                    joiner,
                    full_key,
                    slot_info,
                    current_count,
                    raw_state,
                    filtered_state,
                    state_revision,
                }) => {
                    let raw_state = *raw_state;
                    let filtered_state = *filtered_state;
                    identity.set_session(game_code.clone(), joiner, player_token.clone());

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    let attached = ServerMessage::SessionAttached {
                        game_code: game_code.clone(),
                        player_id: joiner,
                        player_token,
                        full_key: Some(full_key),
                    };
                    if let Ok(json) = serde_json::to_string(&attached) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Notify all connected players about the updated room state
                    let slots_msg = ServerMessage::PlayerSlotsUpdate { slots: slot_info };
                    if let Some(players) = conns.get(&game_code) {
                        for sender in players.values() {
                            let _ = sender.send(slots_msg.clone());
                        }
                    }

                    let updated = {
                        let mut lob_guard = lobby.lock().await;
                        let lob = lob_guard.lobby_mut();
                        lob.set_current_players(&game_code, current_count, &SysEnv);
                        lob.public_game(&game_code)
                    };
                    if let Some(game) = updated {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameUpdated { game },
                        )
                        .await;
                    }

                    let derived = derive_transport_views(&raw_state, &filtered_state, Some(joiner));
                    let viewer_interaction =
                        derive_viewer_interaction(&raw_state, &filtered_state, joiner);
                    let msg = ServerMessage::StateUpdate {
                        state_revision,
                        state: filtered_state,
                        events: vec![],
                        legal_actions: vec![],
                        auto_pass_recommended: false,
                        end_continuous_effect_offers: vec![],
                        mana_payment_shortcut_actions: vec![],
                        eliminated_players: vec![],
                        log_entries: vec![],
                        spell_costs: HashMap::new(),
                        legal_actions_by_object: HashMap::new(),
                        derived,
                        viewer_interaction,
                        // `JoinOutcome::Waiting` — the game has not started, so
                        // no authoritative transition has happened and no turn
                        // boundary can exist. Empty by construction, not by
                        // omission; the first `GameStarted` publishes the real
                        // list.
                        rewind_targets: Vec::new(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    let count = player_count.load(Ordering::Relaxed);
                    broadcast_player_count(lobby_subscribers, count).await;
                }
                Ok(JoinOutcome::Started {
                    player_token,
                    joiner,
                    public_before,
                }) => {
                    identity.set_session(game_code.clone(), joiner, player_token);

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(joiner, tx.clone());
                    }

                    let removed = {
                        let mut lob_guard = lobby.lock().await;
                        let lob = lob_guard.lobby_mut();
                        let existed = lob.has_game(&game_code);
                        lob.unregister_game(&game_code);
                        existed
                    };
                    if removed && public_before {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameRemoved {
                                game_code: game_code.clone(),
                            },
                        )
                        .await;
                    }
                    broadcast_game_started(
                        state,
                        connections,
                        game_spectators,
                        game_db,
                        &game_code,
                    )
                    .await;
                }
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGameWithPassword failed");
                    let msg = ServerMessage::error(e);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }

            // If a cEDH bracket violation blocked the auto-start, fan the error out to all
            // players already connected to the room. The joiner's socket is not yet registered
            // in `connections` (registration only happens on Ok arms above), so this broadcast
            // naturally excludes them — they already received the direct error above.
            if let Some(err_msg) = bracket_broadcast {
                let conns = connections.lock().await;
                if let Some(players) = conns.get(&game_code) {
                    let msg = ServerMessage::error(err_msg);
                    for sender in players.values() {
                        let _ = sender.send(msg.clone());
                    }
                }
            }
        }

        ClientMessage::AbandonGame => {
            if require_host(identity, socket).await.is_err() {
                return;
            }
            let Some(game_code) = identity.game_code.clone() else {
                let msg = ServerMessage::error("Not in a game".to_string());
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            };

            let terminal = {
                let mgr = state.lock().await;
                match mgr.sessions.get(&game_code) {
                    Some(session) if session.game_started => {
                        match terminal_artifact(session, None, "Game abandoned".to_string(), None) {
                            Ok(artifact) => Some(artifact),
                            Err(error) => {
                                let _ = tx.send(ServerMessage::error(error));
                                return;
                            }
                        }
                    }
                    Some(_) => None,
                    None => {
                        let msg = ServerMessage::GameAbandoned {
                            game_code: game_code.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            };
            let terminal_deliveries = match terminal {
                Some(artifact) => match prepare_full_terminal(game_db, artifact).await {
                    Ok(deliveries) => deliveries,
                    Err(error) => {
                        error!(game = %game_code, %error, "terminal preparation failed");
                        let _ = tx.send(ServerMessage::error(error));
                        return;
                    }
                },
                None => Vec::new(),
            };

            let removed = state.lock().await.remove_game(&game_code);
            if let Some(session) = removed.filter(|session| !session.game_started) {
                retire_unstarted_session_async(game_db, &session);
            }

            if !terminal_deliveries.is_empty() {
                let conns = connections.lock().await;
                if let Some(players) = conns.get(&game_code) {
                    for (player, delivery) in &terminal_deliveries {
                        if let Some(sender) = players.get(player) {
                            let _ = sender.send(ServerMessage::TerminalResult {
                                delivery: Some(delivery.clone()),
                            });
                        }
                    }
                }
            }

            connections.lock().await.remove(&game_code);
            game_spectators.lock().await.remove(&game_code);
            lobby.lock().await.lobby_mut().unregister_game(&game_code);

            let msg = ServerMessage::GameAbandoned { game_code };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::Concede => {
            let (game_code, player_token, player_id) = match (
                identity.game_code.clone(),
                identity.player_token.clone(),
                identity.player_id,
            ) {
                (Some(game_code), Some(player_token), Some(player_id)) => {
                    (game_code, player_token, player_id)
                }
                _ => {
                    let msg = ServerMessage::error("Not in a game".to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            info!(game = %game_code, player = ?player_id, "player conceded game");
            let outcome = {
                let mut mgr = state.lock().await;
                match mgr.handle_action(
                    &game_code,
                    &player_token,
                    engine::types::actions::GameAction::Concede { player_id },
                ) {
                    Ok(result) => {
                        let session = mgr
                            .sessions
                            .get_mut(&game_code)
                            .expect("handled concession must retain its session");
                        let revision = session.advance_state_revision();
                        let winner = match &session.state.waiting_for {
                            engine::types::game_state::WaitingFor::GameOver { winner } => *winner,
                            _ => None,
                        };
                        let terminal = if matches!(
                            &session.state.waiting_for,
                            engine::types::game_state::WaitingFor::GameOver { .. }
                        ) {
                            let ranked_result = ranked_duel_players(session).and_then(|players| {
                                ranked_result_for_duel(game_db, &game_code, &players, winner)
                            });
                            terminal_artifact(
                                session,
                                winner,
                                "Opponent conceded".to_string(),
                                ranked_result,
                            )
                            .map(Some)
                        } else {
                            persist_full_session_async(game_db, session);
                            Ok(None)
                        };
                        let rewind_targets = session.rewind_options();
                        terminal
                            .map(|terminal| (revision, result, winner, terminal, rewind_targets))
                    }
                    Err(error) => Err(error),
                }
            };

            match outcome {
                Err(reason) => {
                    if let Ok(json) =
                        serde_json::to_string(&ServerMessage::ActionRejected { reason })
                    {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok((revision, result, winner, terminal, rewind_targets)) => {
                    let terminal_deliveries = match terminal {
                        Some(artifact) => match prepare_full_terminal(game_db, artifact).await {
                            Ok(deliveries) => deliveries,
                            Err(error) => {
                                error!(game = %game_code, %error, "terminal preparation failed");
                                let _ = tx.send(ServerMessage::error(error));
                                return;
                            }
                        },
                        None => Vec::new(),
                    };
                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        for (player, sender) in players {
                            if let Ok(update) = build_state_update_message(
                                &result,
                                revision,
                                *player,
                                rewind_targets.clone(),
                            ) {
                                let _ = sender.send(update);
                            }
                            let _ = sender.send(ServerMessage::Conceded { player: player_id });
                        }
                        for (player, delivery) in &terminal_deliveries {
                            if let Some(sender) = players.get(player) {
                                let _ = sender.send(ServerMessage::TerminalResult {
                                    delivery: Some(delivery.clone()),
                                });
                            }
                        }
                    }
                    drop(conns);

                    if !terminal_deliveries.is_empty() {
                        report_draft_game_over(draft_state, connections, &game_code, winner).await;
                        state.lock().await.remove_game(&game_code);
                    }
                }
            }
        }

        ClientMessage::ConcedeMatch => {
            let (game_code, player_token) =
                match (identity.game_code.clone(), identity.player_token.clone()) {
                    (Some(game_code), Some(player_token)) => (game_code, player_token),
                    _ => {
                        let msg = ServerMessage::error("Not in a game".to_string());
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                };

            let outcome = {
                let mut mgr = state.lock().await;
                match mgr.handle_match_concede(&game_code, &player_token) {
                    Ok((revision, result)) => {
                        let winner = match &result.0.waiting_for {
                            engine::types::game_state::WaitingFor::GameOver { winner } => *winner,
                            _ => None,
                        };
                        let session = mgr
                            .sessions
                            .get(&game_code)
                            .expect("handled match concession must retain its session");
                        let ranked_result = winner.and_then(|winner| {
                            ranked_duel_players(session).and_then(|players| {
                                ranked_result_for_duel(game_db, &game_code, &players, Some(winner))
                            })
                        });
                        let rewind_targets = session.rewind_options();
                        terminal_artifact(
                            session,
                            winner,
                            "Match conceded".to_string(),
                            ranked_result,
                        )
                        .map(|terminal| (revision, result, winner, terminal, rewind_targets))
                    }
                    Err(error) => Err(error),
                }
            };

            match outcome {
                Err(reason) => {
                    if let Ok(json) =
                        serde_json::to_string(&ServerMessage::ActionRejected { reason })
                    {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok((revision, result, winner, terminal, rewind_targets)) => {
                    let terminal_deliveries = match prepare_full_terminal(game_db, terminal).await {
                        Ok(deliveries) => deliveries,
                        Err(error) => {
                            error!(game = %game_code, %error, "terminal preparation failed");
                            let _ = tx.send(ServerMessage::error(error));
                            return;
                        }
                    };
                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        for (player, sender) in players {
                            if let Ok(update) = build_state_update_message(
                                &result,
                                revision,
                                *player,
                                rewind_targets.clone(),
                            ) {
                                let _ = sender.send(update);
                            }
                            if let Some((_, delivery)) =
                                terminal_deliveries.iter().find(|(seat, _)| seat == player)
                            {
                                let _ = sender.send(ServerMessage::TerminalResult {
                                    delivery: Some(delivery.clone()),
                                });
                            }
                        }
                    }
                    drop(conns);

                    report_draft_game_over(draft_state, connections, &game_code, winner).await;

                    state.lock().await.remove_game(&game_code);
                }
            }
        }

        // GH #1507: multiplayer-safe "request takeback" — see
        // `server_core::takeback` for the unanimous-approval rules this
        // delegates to. None of these three arms touch `session.state`
        // directly; they only call into `GameSession` methods that own the
        // takeback/rollback invariants.
        ClientMessage::RequestTakeback(target) => {
            let (game_code, player_id) = match (&identity.game_code, identity.player_id) {
                (Some(c), Some(p)) => (c.clone(), p),
                _ => {
                    let msg = ServerMessage::error("Not in a game".to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let Some(session) = mgr.sessions.get_mut(&game_code) else {
                drop(mgr);
                return;
            };
            // An absent payload is the frame every pre-rewind client sends;
            // normalizing at the transport edge keeps `RewindTarget` — not
            // `Option<RewindTarget>` — the session API's vocabulary.
            let outcome = session.request_takeback(player_id, target.unwrap_or_default());
            // `pending_takeback_message` reads `session.pending_takeback`,
            // which `request_takeback` already cleared on an Approved
            // outcome — so this is `Some` exactly when we need it (the
            // Pending arm below) and `None` otherwise.
            let requested_msg = session.pending_takeback_message();
            let player_count = session.player_count;
            let approved_snapshot = matches!(outcome, Ok(server_core::TakebackOutcome::Approved))
                .then(|| {
                    (
                        session.advance_state_revision(),
                        session.current_broadcast_snapshot(),
                    )
                });
            // A rollback can restore an AI seat to priority — most obviously a
            // `TurnStart` rewind onto an AI turn, but a `LastAction` rewind can
            // land there too. Without this the table freezes: nothing else on
            // the approved path drives the AI. `run_ai` is a no-op when there
            // are no AI seats, and its own pending-takeback guard is already
            // cleared by the time we get here. Revisions stay contiguous: the
            // rollback took R+1 above, `run_ai` allocates R+2..R+k.
            let ai_results = if approved_snapshot.is_some() {
                session.run_ai()
            } else {
                Vec::new()
            };
            // Both read AFTER `run_ai`, matching the shipped action path: an AI
            // follow-up can cross a turn (new rewind boundary) or finish a
            // player off (new elimination), and the AI fan-out below must not
            // describe the state as it stood before its own results.
            // `snapshot.0` is the *pre*-`run_ai` rollback state, so sourcing
            // eliminations from it would be exactly that staleness.
            let rewind_targets = session.rewind_options();
            let eliminated = session.state.eliminated_players.clone();
            // GH #1507: persist the rolled-back state immediately, in the
            // same lock as the rollback itself — otherwise SQLite still
            // holds the pre-rollback `GameState` until some later action
            // happens to persist, and a crash/restart in that window
            // resurrects the branch the table just agreed to undo. Ordered
            // AFTER `run_ai`, matching the normal action path: otherwise a
            // crash between the rollback and the next action resurrects a
            // state the AI has already moved past.
            if approved_snapshot.is_some() {
                persist_full_session_async(game_db, session);
            }
            drop(mgr);

            match outcome {
                Err(reason) => {
                    // A refused takeback is a benign rejection, not a
                    // transport error: "there is no previous action of yours
                    // to take back", "a takeback request is already pending",
                    // "only human players may request a takeback". Answer on
                    // the same channel the sibling `ClientMessage::Action`
                    // handler uses for a rejected action.
                    //
                    // `ServerMessage::error` is read by the native client as a
                    // terminal socket failure: `handleNativeEvent` disposes the
                    // adapter on ANY `error` event and GamePage then sets
                    // `reconnectState: "failed"`, leaving the desktop session
                    // unrecoverable. Reaching for the error channel here was
                    // this handler's inconsistency with its own sibling ~2,400
                    // lines above, not a deliberate signal.
                    let msg = ServerMessage::ActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok(server_core::TakebackOutcome::Pending) => {
                    info!(game = %game_code, player = ?player_id, "takeback requested");
                    if let Some(msg) = requested_msg {
                        let conns = connections.lock().await;
                        if let Some(players) = conns.get(&game_code) {
                            for sender in players.values() {
                                let _ = sender.send(msg.clone());
                            }
                        }
                    }
                }
                Ok(server_core::TakebackOutcome::Approved) => {
                    info!(game = %game_code, player = ?player_id, "takeback auto-approved (sole human seat)");
                    let (state_revision, snapshot) =
                        approved_snapshot.expect("Approved outcome always computes a snapshot");
                    broadcast_takeback_approved(
                        connections,
                        game_spectators,
                        &game_code,
                        player_count,
                        state_revision,
                        snapshot,
                        None,
                        rewind_targets.clone(),
                    )
                    .await;
                    broadcast_ai_results(
                        connections,
                        game_spectators,
                        &game_code,
                        player_count,
                        &eliminated,
                        &ai_results,
                        &rewind_targets,
                    )
                    .await;
                }
                Ok(server_core::TakebackOutcome::Rejected) => {
                    // request_takeback never returns Rejected — only respond_takeback does.
                }
            }
        }

        ClientMessage::RespondTakeback { approve } => {
            let (game_code, player_id) = match (&identity.game_code, identity.player_id) {
                (Some(c), Some(p)) => (c.clone(), p),
                _ => {
                    let msg = ServerMessage::error("Not in a game".to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let Some(session) = mgr.sessions.get_mut(&game_code) else {
                drop(mgr);
                return;
            };
            let outcome = session.respond_takeback(player_id, approve);
            let player_count = session.player_count;
            let approved_snapshot = matches!(outcome, Ok(server_core::TakebackOutcome::Approved))
                .then(|| {
                    (
                        session.advance_state_revision(),
                        session.current_broadcast_snapshot(),
                    )
                });
            // Same reason, same ordering, as the `RequestTakeback` arm above:
            // the rolled-back state can put an AI seat on priority.
            let ai_results = if approved_snapshot.is_some() {
                session.run_ai()
            } else {
                Vec::new()
            };
            // Read AFTER `run_ai` for the same reason as the `RequestTakeback`
            // arm above.
            let rewind_targets = session.rewind_options();
            let eliminated = session.state.eliminated_players.clone();
            // GH #1507: persist the rolled-back state immediately — see the
            // matching comment in the `RequestTakeback` arm above.
            if approved_snapshot.is_some() {
                persist_full_session_async(game_db, session);
            }
            drop(mgr);

            match outcome {
                Err(reason) => {
                    // Same classification as the `RequestTakeback` arm above:
                    // "there is no pending takeback request" and "only human
                    // players may respond" are refusals, not socket failures.
                    // Fixed here too so the pair stays consistent — a benign
                    // refusal must never travel the channel the native client
                    // treats as terminal.
                    let msg = ServerMessage::ActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok(server_core::TakebackOutcome::Pending) => {
                    info!(game = %game_code, player = ?player_id, approve, "takeback approval recorded");
                }
                Ok(server_core::TakebackOutcome::Approved) => {
                    info!(game = %game_code, player = ?player_id, "takeback unanimously approved");
                    let (state_revision, snapshot) =
                        approved_snapshot.expect("Approved outcome always computes a snapshot");
                    broadcast_takeback_approved(
                        connections,
                        game_spectators,
                        &game_code,
                        player_count,
                        state_revision,
                        snapshot,
                        Some(player_id),
                        rewind_targets.clone(),
                    )
                    .await;
                    broadcast_ai_results(
                        connections,
                        game_spectators,
                        &game_code,
                        player_count,
                        &eliminated,
                        &ai_results,
                        &rewind_targets,
                    )
                    .await;
                }
                Ok(server_core::TakebackOutcome::Rejected) => {
                    info!(game = %game_code, player = ?player_id, "takeback declined");
                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        let msg = ServerMessage::TakebackResolved {
                            approved: false,
                            resolved_by: Some(player_id),
                        };
                        for sender in players.values() {
                            let _ = sender.send(msg.clone());
                        }
                    }
                }
            }
        }

        ClientMessage::CancelTakeback => {
            let (game_code, player_id) = match (&identity.game_code, identity.player_id) {
                (Some(c), Some(p)) => (c.clone(), p),
                _ => {
                    let msg = ServerMessage::error("Not in a game".to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let Some(session) = mgr.sessions.get_mut(&game_code) else {
                drop(mgr);
                return;
            };
            let result = session.cancel_takeback(player_id);
            drop(mgr);

            match result {
                Err(reason) => {
                    // The third member of the same class as the two arms
                    // above: `cancel_takeback`'s only failures are benign
                    // refusals ("only the player who requested the takeback
                    // may cancel it", "there is no pending takeback
                    // request"). Answer on the rejection channel, not the
                    // terminal error channel — `handleNativeEvent` disposes
                    // the adapter on ANY `error` event, so a mis-clicked
                    // cancel would end the desktop session.
                    let msg = ServerMessage::ActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok(()) => {
                    info!(game = %game_code, player = ?player_id, "takeback request cancelled");
                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        let msg = ServerMessage::TakebackResolved {
                            approved: false,
                            resolved_by: Some(player_id),
                        };
                        for sender in players.values() {
                            let _ = sender.send(msg.clone());
                        }
                    }
                }
            }
        }

        ClientMessage::SpectatorJoin { game_code } => {
            if let Err(reason) = guard_spectator_join(&game_code) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            debug!(game = %game_code, "spectator join request");
            {
                let mgr = state.lock().await;
                let Some(session) = mgr.sessions.get(&game_code) else {
                    let msg = ServerMessage::error(format!("Game not found: {game_code}"));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                };
                if !session.game_started {
                    let msg = ServerMessage::error("Game has not started yet".to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            if let Err(reason) = switch_game_spectator_slot(
                game_spectators,
                identity.spectator_game_code.as_deref(),
                &game_code,
                tx,
            )
            .await
            {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let snapshot_result = {
                let mgr = state.lock().await;
                match mgr.sessions.get(&game_code) {
                    Some(session) if session.game_started => {
                        build_spectator_game_started_message(session)
                    }
                    Some(_) => Err("Game has not started yet".to_string()),
                    None => Err(format!("Game not found: {game_code}")),
                }
            };

            let spectator_msg = match snapshot_result {
                Ok(msg) => msg,
                Err(message) => {
                    remove_game_spectator_sender(game_spectators, &game_code, tx).await;
                    let msg = ServerMessage::error(message);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            if tx.send(spectator_msg).is_err() {
                remove_game_spectator_sender(game_spectators, &game_code, tx).await;
                return;
            }
            identity.spectator_game_code = Some(game_code.clone());
            info!(game = %game_code, "spectator connected to live game");
        }

        ClientMessage::Emote { emote } => {
            if let Err(reason) = guard_emote(&emote) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let game_code = match &identity.game_code {
                Some(c) => c.clone(),
                None => return,
            };
            let player_id = match identity.player_id {
                Some(p) => p,
                None => return,
            };

            debug!(game = %game_code, player = ?player_id, emote = %emote, "emote");
            let msg = ServerMessage::Emote {
                from_player: player_id,
                emote,
            };

            // Send emote to all other players in the game
            let conns = connections.lock().await;
            if let Some(game_conns) = conns.get(&game_code) {
                for (&pid, sender) in game_conns.iter() {
                    if pid != player_id {
                        let _ = sender.send(msg.clone());
                    }
                }
            }
        }

        ClientMessage::Ping { .. } => {
            // Mode-agnostic keepalive: the broker is the single authority for
            // the Pong reply on both Full and LobbyOnly servers.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::SeatMutate { mutation } => {
            if matches!(mode, ServerMode::LobbyOnly) {
                let msg = ServerMessage::error(
                    "Seat mutations are not available on lobby-only servers.".to_string(),
                );
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            if require_host(identity, socket).await.is_err() {
                return;
            }

            if let Err(reason) = guard_seat_mutation(&mutation) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let Some(game_code) = identity.game_code.clone() else {
                return;
            };

            let (
                slot_info,
                kicked_players,
                started,
                current_players,
                max_players,
                public_before,
                bracket_error,
            ) = {
                let mut mgr = state.lock().await;
                let Some(session) = mgr.sessions.get_mut(&game_code) else {
                    let msg = ServerMessage::error(format!("Game not found: {game_code}"));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                };

                let public_before = session.lobby_meta.as_ref().is_some_and(|meta| meta.public);
                let mut seat_state = session.seat_state();
                let delta_result = {
                    let resolver = ServerDeckResolver { db: db.as_ref() };
                    let ctx = ReducerCtx {
                        platform: phase_ai::config::Platform::Native,
                        deck_resolver: &resolver,
                    };
                    seat_reducer::apply(&mut seat_state, mutation, &ctx)
                };
                let delta = match delta_result {
                    Ok(delta) => delta,
                    Err(err) => {
                        let msg = ServerMessage::error(format!("Seat mutation failed: {err:?}"));
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                };

                let kicked_players = delta
                    .invalidated_tokens
                    .iter()
                    .filter_map(|token| {
                        session
                            .player_for_token(token)
                            .map(|pid| (pid, token.clone()))
                    })
                    .collect::<Vec<_>>();

                session.apply_seat_delta(seat_state, &delta, db.as_ref());
                // Issue #1506: a `SeatMutate` is an *explicit* host edit (Start,
                // Kick, Remove, add-AI). Only `SeatMutation::Start` — surfaced as
                // `delta.now_started` — may begin the game here. Folding in an
                // `is_full() && start_when_full` auto-start made every seat edit
                // (e.g. kicking a player from a full room) silently start the game,
                // while the real Start button appeared inert because the room had
                // already auto-started on the join that filled it. Auto-start-when-
                // full is handled in the `JoinGame` path (a guest filling the last
                // seat), per the `GameSession` contract; it does not belong on the
                // host's seat-editing path.
                let mut started = delta.now_started;
                // Collect a bracket-violation message to broadcast after releasing the state lock.
                // start_game guarantees no mutation on Err, so session state is untouched.
                let bracket_error: Option<String> = if started {
                    match session.start_game(db.as_ref()) {
                        Ok(()) => None,
                        Err(bracket_err) => {
                            started = false;
                            Some(format!("Cannot start cEDH game: {bracket_err}"))
                        }
                    }
                } else {
                    None
                };
                let slot_info = session.player_slot_info();
                let current_players = session.current_player_count();
                let max_players = session.player_count;
                persist_full_session_async(game_db, session);

                // Keep the token-to-game index consistent: this seat mutation
                // invalidated these tokens (kicked / replaced / removed seats),
                // so they must stop resolving to this game via game_for_token.
                // apply_seat_delta clears the per-seat token arrays but cannot
                // reach the manager's index. (Game removal does the equivalent
                // cleanup for whole-game teardown.)
                mgr.unindex_tokens(&delta.invalidated_tokens);
                (
                    slot_info,
                    kicked_players,
                    started,
                    current_players,
                    max_players,
                    public_before,
                    bracket_error,
                )
            };

            {
                let mut conns = connections.lock().await;
                if let Some(players) = conns.get_mut(&game_code) {
                    for (pid, _) in &kicked_players {
                        if let Some(sender) = players.remove(pid) {
                            let _ = sender.send(ServerMessage::error(
                                "You were removed from the room by the host.".to_string(),
                            ));
                        }
                    }

                    // If the start was blocked by a bracket violation, notify all players.
                    if let Some(ref err_msg) = bracket_error {
                        let msg = ServerMessage::error(err_msg.clone());
                        for sender in players.values() {
                            let _ = sender.send(msg.clone());
                        }
                    }

                    let msg = ServerMessage::PlayerSlotsUpdate {
                        slots: slot_info.clone(),
                    };
                    for sender in players.values() {
                        let _ = sender.send(msg.clone());
                    }
                }
            }

            if started {
                let removed = {
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    let existed = lob.has_game(&game_code);
                    lob.unregister_game(&game_code);
                    existed
                };
                if removed && public_before {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameRemoved {
                            game_code: game_code.clone(),
                        },
                    )
                    .await;
                }
                broadcast_game_started(state, connections, game_spectators, game_db, &game_code)
                    .await;
            } else {
                let updated = {
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    lob.set_current_players(&game_code, current_players, &SysEnv);
                    lob.set_max_players(&game_code, max_players);
                    lob.public_game(&game_code)
                };
                if let Some(game) = updated {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameUpdated { game },
                    )
                    .await;
                }
            }
        }

        ClientMessage::UpdateLobbyMetadata { .. } => {
            // LobbyOnly-exclusive (rejected in Full mode by reject_if_disabled).
            // The broker owns the ownership check, reservation consumption,
            // count/max updates, and the LobbyGameUpdated fan-out.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::CreateDraftWithSettings {
            display_name,
            set_code,
            kind,
            public,
            password,
            timer_seconds,
            tournament_format,
            pod_policy,
            pod_size,
        } => {
            info!(
                display_name = %display_name,
                set_code = %set_code,
                kind = ?kind,
                public,
                pod_size,
                "CreateDraftWithSettings"
            );

            if let Err(reason) = guard_create_draft_with_settings(
                &display_name,
                &set_code,
                &password,
                timer_seconds,
                pod_size,
                kind,
            ) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if !draft_pools.contains_set(&set_code) {
                let msg = ServerMessage::DraftActionRejected {
                    reason: format!("No draft pool data for set: {set_code}"),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if kind != draft_core::types::DraftKind::Quick
                && tournament_format == draft_core::types::TournamentFormat::SingleElimination
                && pod_size != 8
            {
                let msg = ServerMessage::DraftActionRejected {
                    reason: "Single-elimination draft events require exactly 8 seats".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let config = draft_core::types::DraftConfig {
                source: draft_core::types::DraftSource::Set {
                    code: set_code.clone(),
                },
                set_code: set_code.clone(),
                kind,
                pod_size,
                cards_per_pack: 14,
                pack_count: if kind == draft_core::types::DraftKind::Sealed {
                    6
                } else {
                    3
                },
                min_deck_size: 40,
                addable_cards: draft_core::types::DeckAddableCards::standard_basics(),
                rng_seed: rand::random(),
                tournament_format,
                pod_policy,
                spectator_visibility: draft_core::types::SpectatorVisibility::default(),
            };

            let (draft_code, player_token, seat_index) = {
                let mut mgr = draft_state.lock().await;
                let (draft_code, player_token, seat_index) =
                    mgr.create_draft(config, display_name.clone());
                if let Some(session) = mgr.sessions.get_mut(&draft_code) {
                    session.lobby_meta = Some(server_core::PersistedLobbyMeta {
                        host_name: display_name.clone(),
                        public,
                        password: password.clone(),
                        timer_seconds,
                        start_when_full: true,
                        ranked: false,
                    });
                }
                (draft_code, player_token, seat_index)
            };

            identity.draft_code = Some(draft_code.clone());
            identity.draft_seat = Some(seat_index as usize);
            identity.draft_token = Some(player_token.clone());

            // Register this connection in the connections map under draft_code
            {
                let mut conns = connections.lock().await;
                conns
                    .entry(draft_code.clone())
                    .or_default()
                    .insert(PlayerId(seat_index), tx.clone());
            }

            // Register in lobby so draft appears in the lobby list
            {
                let (host_version, host_build_commit) = identity
                    .client_hello
                    .as_ref()
                    .map(|h| (h.client_version.clone(), h.build_commit.clone()))
                    .unwrap_or_default();
                let mut lob_guard = lobby.lock().await;
                lob_guard.lobby_mut().register_game(
                    &draft_code,
                    RegisterGameRequest {
                        host_name: display_name.clone(),
                        public,
                        password,
                        timer_seconds,
                        host_version,
                        host_build_commit,
                        current_players: 1,
                        max_players: pod_size as u32,
                        format_config: None,
                        match_config: Default::default(),
                        room_name: None,
                        host_peer_id: String::new(),
                        draft_metadata: Some(server_core::protocol::DraftLobbyMetadata {
                            set_code,
                            draft_kind: format!("{kind:?}"),
                            cube_name: None,
                        }),
                        ranked: false,
                    },
                    &SysEnv,
                );
            }

            let msg = ServerMessage::DraftCreated {
                draft_code: draft_code.clone(),
                player_token,
                seat_index,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }

            if public {
                let game = {
                    let lob = lobby.lock().await;
                    lob.lobby().public_game(&draft_code)
                };
                if let Some(game) = game {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameAdded { game },
                    )
                    .await;
                }
            }

            info!(draft = %draft_code, host = %display_name, "draft created");
        }

        ClientMessage::JoinDraftWithPassword {
            draft_code,
            display_name,
            password,
        } => {
            info!(draft = %draft_code, joiner = %display_name, "JoinDraftWithPassword");

            if let Err(reason) =
                guard_join_draft_with_password(&draft_code, &display_name, &password)
            {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let result = {
                let mut mgr = draft_state.lock().await;
                mgr.join_draft(&draft_code, display_name.clone(), password.as_deref())
            };

            match result {
                Ok((player_token, seat_index, view)) => {
                    identity.draft_code = Some(draft_code.clone());
                    identity.draft_seat = Some(seat_index as usize);
                    identity.draft_token = Some(player_token.clone());

                    // Register this connection in the connections map under draft_code
                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(draft_code.clone())
                            .or_default()
                            .insert(PlayerId(seat_index), tx.clone());
                    }

                    // Update lobby seats_filled count
                    {
                        let mgr = draft_state.lock().await;
                        if let Some(session) = mgr.sessions.get(&draft_code) {
                            let filled = session
                                .player_tokens
                                .iter()
                                .filter(|t| !t.is_empty())
                                .count();
                            let mut lob_guard = lobby.lock().await;
                            let lob = lob_guard.lobby_mut();
                            lob.set_current_players(&draft_code, filled as u32, &SysEnv);
                            if let Some(game) = lob.public_game(&draft_code) {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                    }

                    let msg = ServerMessage::DraftJoined {
                        draft_code,
                        player_token,
                        seat_index,
                        view,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Err(reason) => {
                    if reason == "password_required" {
                        let msg = ServerMessage::PasswordRequired {
                            game_code: draft_code.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::DraftAction { draft_code, action } => {
            if let Err(reason) = guard_draft_action(&draft_code) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if let Err(reason) = guard_draft_action_payload(&action) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let token = match &identity.draft_token {
                Some(t) => t.clone(),
                None => {
                    let msg = ServerMessage::DraftActionRejected {
                        reason: "Not in a draft session".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            debug!(draft = %draft_code, action = ?action, "DraftAction");

            if let Some(reason) = client_forbidden_draft_action_reason(&action) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            // Check if this is a StartDraft action (triggers timer)
            let is_start = matches!(action, draft_core::types::DraftAction::StartDraft);
            let pack_generator = if is_start {
                match draft_pack_generator_for_start(draft_state, draft_pools, &draft_code).await {
                    Ok(generator) => Some(generator),
                    Err(reason) => {
                        let msg = ServerMessage::DraftActionRejected { reason };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            } else {
                None
            };

            let public_before = if is_start {
                draft_state
                    .lock()
                    .await
                    .sessions
                    .get(&draft_code)
                    .and_then(|s| s.lobby_meta.as_ref())
                    .is_some_and(|m| m.public)
            } else {
                false
            };

            let result = {
                let mut mgr = draft_state.lock().await;
                let before_window = mgr.sessions.get(&draft_code).map(|s| {
                    (
                        s.session.status,
                        s.session.current_pack_number,
                        s.session.pick_number,
                    )
                });
                let result = mgr.handle_draft_action(
                    &draft_code,
                    &token,
                    action,
                    pack_generator
                        .as_ref()
                        .map(|generator| generator as &dyn draft_core::pack_source::PackSource),
                );
                let after_window = mgr.sessions.get(&draft_code).map(|s| {
                    (
                        s.session.status,
                        s.session.current_pack_number,
                        s.session.pick_number,
                    )
                });
                let should_rearm_timer =
                    result.is_ok() && should_rearm_pick_timer(before_window, after_window);
                result.map(|views| (views, should_rearm_timer))
            };

            match result {
                Ok((views, should_rearm_timer)) => {
                    if is_start {
                        let removed = {
                            let mut lob_guard = lobby.lock().await;
                            let lob = lob_guard.lobby_mut();
                            let existed = lob.has_game(&draft_code);
                            lob.unregister_game(&draft_code);
                            existed
                        };
                        if removed && public_before {
                            broadcast_to_lobby_subscribers(
                                lobby_subscribers,
                                ServerMessage::LobbyGameRemoved {
                                    game_code: draft_code.clone(),
                                },
                            )
                            .await;
                        }
                    }

                    // Broadcast DraftStateUpdate to all connected sockets in the pod
                    broadcast_draft_views(&draft_code, &views, connections, draft_state).await;

                    // (Re)arm only when a new pick window begins: StartDraft
                    // or a completed round that advanced pack/pick position.
                    // A single partial pick must not reset the whole pod's
                    // timeout while other seats still owe picks in the current
                    // window.
                    if should_rearm_timer {
                        spawn_pick_timer(
                            draft_state.clone(),
                            connections.clone(),
                            draft_code.clone(),
                            75, // default pick timer seconds
                        );
                    }

                    maybe_spawn_draft_matches(&draft_code, draft_state, state, db, connections)
                        .await;

                    // Persist draft session after mutation
                    persist_draft_session_async(game_db, &draft_code, draft_state).await;

                    // Broadcast to spectators
                    broadcast_draft_spectator_views(&draft_code, draft_state, draft_spectators)
                        .await;
                }
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::ReconnectDraft {
            draft_code,
            player_token,
        } => {
            info!(draft = %draft_code, "ReconnectDraft attempt");

            if let Err(reason) = guard_reconnect_draft(&draft_code, &player_token) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let result = {
                let mut mgr = draft_state.lock().await;
                mgr.handle_reconnect(&draft_code, &player_token)
            };

            match result {
                Ok(view) => {
                    // Restore identity
                    let seat = {
                        let mgr = draft_state.lock().await;
                        mgr.sessions
                            .get(&draft_code)
                            .and_then(|s| s.seat_for_token(&player_token))
                    };
                    if let Some(seat) = seat {
                        identity.draft_code = Some(draft_code.clone());
                        identity.draft_seat = Some(seat);
                        identity.draft_token = Some(player_token);
                    }

                    let msg = ServerMessage::DraftStateUpdate { view };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    info!(draft = %draft_code, "draft reconnect succeeded");
                }
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::SpectateDraft { draft_code } => {
            if let Err(reason) = guard_spectate_draft(&draft_code) {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let visibility = {
                let drafts = draft_state.lock().await;
                match drafts.sessions.get(&draft_code) {
                    Some(session) => session.config.spectator_visibility,
                    None => {
                        let msg = ServerMessage::error("Draft not found".to_string());
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            };

            if let Err(reason) = switch_draft_spectator_slot(
                draft_spectators,
                identity.spectator_draft_code.as_deref(),
                &draft_code,
                visibility,
                tx,
            )
            .await
            {
                let msg = ServerMessage::error(reason);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let snapshot_result = {
                let drafts = draft_state.lock().await;
                match drafts.sessions.get(&draft_code) {
                    Some(session) => {
                        let visibility = session.config.spectator_visibility;
                        let view =
                            draft_core::view::filter_for_spectator(&session.session, visibility);
                        Ok((visibility, view))
                    }
                    None => Err("Draft not found".to_string()),
                }
            };

            let (visibility, view) = match snapshot_result {
                Ok(snapshot) => snapshot,
                Err(message) => {
                    remove_draft_spectator_sender(draft_spectators, &draft_code, tx).await;
                    let msg = ServerMessage::error(message);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let msg = ServerMessage::DraftSpectatorView { view };
            if let Ok(json) = serde_json::to_string(&msg) {
                if socket.send(Message::text(json)).await.is_err() {
                    remove_draft_spectator_sender(draft_spectators, &draft_code, tx).await;
                    return;
                }
            }
            identity.spectator_draft_code = Some(draft_code.clone());
            identity.spectator_visibility = Some(visibility);
            info!(draft = %draft_code, ?visibility, "spectator connected to draft");
        }

        ClientMessage::UnregisterLobby { .. } => {
            // LobbyOnly-exclusive (rejected in Full mode by reject_if_disabled).
            // The broker owns the ownership check, removal, LobbyGameRemoved
            // fan-out, and clearing the host-game ownership stamp.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }
    }
}

#[cfg(test)]
mod state_transport_derived_tests {
    use super::*;
    use engine::game::{deck_loading::PlayerDeckPayload, engine::apply};
    use engine::types::ability::{Effect, ResolvedAbility, SearchSelectionConstraint};
    use engine::types::actions::{GameAction, ResolveAllConsentDecision};
    use engine::types::game_state::{
        ActiveSearchDecisionAuthority, ActiveSearchDecisionControl, PriorityPassingMode,
        StackEntry, StackEntryKind, WaitingFor,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
    use engine::types::phase::Phase;

    fn low_use_window_priority_result(
        semantic_player: PlayerId,
        controller: Option<PlayerId>,
    ) -> ActionResult {
        let mut state = GameState::new_two_player(42);
        state.active_player = semantic_player;
        state.priority_player = controller.unwrap_or(semantic_player);
        state.waiting_for = WaitingFor::Priority {
            player: semantic_player,
        };
        state.turn_decision_controller = controller;
        state.phase = Phase::End;
        state.priority_passing_modes.insert(
            controller.unwrap_or(semantic_player),
            PriorityPassingMode::SkipLowUseWindows,
        );
        let legal_actions = vec![
            GameAction::PassPriority,
            GameAction::TurnFaceUp {
                object_id: ObjectId(999),
                x: 0,
            },
        ];

        (
            state,
            Vec::new(),
            legal_actions,
            Vec::new(),
            true,
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn state_update_action_fields(result: &ActionResult, viewer: PlayerId) -> (usize, bool) {
        match build_state_update_message(result, 1, viewer, Vec::new())
            .expect("fixture state update")
        {
            ServerMessage::StateUpdate {
                legal_actions,
                auto_pass_recommended,
                ..
            } => (legal_actions.len(), auto_pass_recommended),
            other => panic!("expected StateUpdate, got {other:?}"),
        }
    }

    #[test]
    fn resolve_all_snapshot_keeps_a_bounded_tail_of_engine_logs() {
        let state = GameState::new_two_player(42);
        let logs: Vec<_> = (0..=MAX_RESOLVE_ALL_LOG_ENTRIES)
            .map(|seq| GameLogEntry {
                seq: seq as u32,
                turn: 1,
                phase: Phase::PreCombatMain,
                category: LogCategory::Game,
                segments: vec![LogSegment::Text(format!("entry {seq}"))],
                presentation: Default::default(),
            })
            .collect();
        let tail = &logs[logs.len().saturating_sub(MAX_RESOLVE_ALL_LOG_ENTRIES)..];

        let update = build_resolve_all_state_update_message(
            &state,
            tail,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            1,
            PlayerId(0),
            Vec::new(),
            Vec::new(),
        );

        match update {
            ServerMessage::StateUpdate {
                log_entries,
                events,
                ..
            } => {
                assert!(events.is_empty());
                assert_eq!(log_entries.as_slice(), tail);
                assert_eq!(log_entries.len(), MAX_RESOLVE_ALL_LOG_ENTRIES);
            }
            other => panic!("expected StateUpdate, got {other:?}"),
        }
    }

    #[test]
    fn resolve_all_final_log_tail_orders_batch_before_ai_follow_up_logs() {
        let state = GameState::new_two_player(42);
        let batch_logs: Vec<_> = (0..=MAX_RESOLVE_ALL_LOG_ENTRIES)
            .map(|seq| GameLogEntry {
                seq: seq as u32,
                turn: 1,
                phase: Phase::PreCombatMain,
                category: LogCategory::Game,
                segments: vec![LogSegment::Text(format!("batch {seq}"))],
                presentation: Default::default(),
            })
            .collect();
        let ai_logs: Vec<_> = (0..2)
            .map(|seq| GameLogEntry {
                seq: (100 + seq) as u32,
                turn: 1,
                phase: Phase::PreCombatMain,
                category: LogCategory::Game,
                segments: vec![LogSegment::Text(format!("ai {seq}"))],
                presentation: Default::default(),
            })
            .collect();
        let ai_results = vec![(
            2,
            (
                state,
                Vec::new(),
                Vec::new(),
                ai_logs.clone(),
                false,
                HashMap::new(),
                HashMap::new(),
            ),
        )];

        let tail = resolve_all_log_tail(&batch_logs, &ai_results);

        assert_eq!(tail.len(), MAX_RESOLVE_ALL_LOG_ENTRIES);
        assert_eq!(tail.first(), batch_logs.get(3));
        assert_eq!(&tail[tail.len() - ai_logs.len()..], ai_logs.as_slice());
    }

    #[tokio::test]
    async fn resolve_all_handler_sends_the_final_snapshot_before_its_acknowledgement() {
        let mut manager = SessionManager::new();
        let (game_code, player_token) = manager.create_game(PlayerDeckPayload::default());
        let ai_player = PlayerId(1);
        let session = manager
            .sessions
            .get_mut(&game_code)
            .expect("new game retains its session");
        session.ai_seats.insert(ai_player);
        session.ai_configs.insert(
            ai_player,
            phase_ai::config::create_config_for_players(
                phase_ai::config::AiDifficulty::Easy,
                phase_ai::config::Platform::Native,
                2,
            ),
        );
        let stack_object = ObjectId(1);
        session.state.active_player = ai_player;
        session.state.priority_player = PlayerId(0);
        session.state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        // The AI has already passed in this priority cycle, so the requesting
        // human's pass deterministically resolves the stack entry.
        session.state.priority_passes.insert(ai_player);
        session.state.stack.push_back(StackEntry {
            id: stack_object,
            source_id: stack_object,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: stack_object,
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    stack_object,
                    PlayerId(0),
                )),
            },
        });
        apply(
            &mut session.state,
            PlayerId(0),
            GameAction::BeginResolveAll { max_resolutions: 1 },
        )
        .expect("priority holder may start Resolve All consent");
        let epoch = match session.state.waiting_for {
            WaitingFor::ResolveAllConsent { epoch, .. } => epoch,
            ref other => {
                panic!("Resolve All consent must await the AI representative, got {other:?}")
            }
        };
        apply(
            &mut session.state,
            ai_player,
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("AI representative may grant Resolve All consent");
        assert!(matches!(
            session.state.waiting_for,
            WaitingFor::ResolveAllReady { epoch: ready_epoch } if ready_epoch == epoch
        ));
        let revision_before = session.state_revision;

        let state: SharedState = Arc::new(Mutex::new(manager));
        let draft_state: SharedDraftState = Arc::new(Mutex::new(DraftSessionManager::new()));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        let game_spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
        let db_file = tempfile::NamedTempFile::new().expect("temporary game database");
        let game_db = Arc::new(
            persistence::GameDb::open(db_file.path(), persistence::SessionRetention::Multiplayer)
                .expect("open temporary game database"),
        );
        let (requester_tx, mut requester_rx) = mpsc::unbounded_channel();
        let (ai_tx, mut ai_rx) = mpsc::unbounded_channel();
        connections
            .lock()
            .await
            .insert(game_code.clone(), HashMap::from([(ai_player, ai_tx)]));
        let identity = SocketIdentity {
            game_code: Some(game_code.clone()),
            player_id: Some(PlayerId(0)),
            player_token: Some(player_token),
            lobby_subscribed: false,
            session_span: None,
            client_hello: None,
            lobby_host_game: None,
            seat_reservations: Vec::new(),
            lobby_reservations: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
            spectator_game_code: None,
        };

        handle_resolve_all(
            41,
            1,
            &state,
            &draft_state,
            &connections,
            &requester_tx,
            &game_db,
            &game_spectators,
            &identity,
        )
        .await;

        let (expected_revision, expected_waiting_for) = {
            let manager = state.lock().await;
            let session = manager
                .sessions
                .get(&game_code)
                .expect("Resolve All retains its session");
            assert!(
                session.state_revision > revision_before,
                "the resolved batch must advance the authoritative revision"
            );
            (session.state_revision, session.state.waiting_for.clone())
        };

        match tokio::time::timeout(std::time::Duration::from_secs(1), requester_rx.recv())
            .await
            .expect("Resolve All must send the requester state update")
            .expect("requester state update channel remains open")
        {
            ServerMessage::StateUpdate {
                state_revision,
                state,
                ..
            } => {
                assert_eq!(state_revision, expected_revision);
                assert_eq!(state.waiting_for, expected_waiting_for);
            }
            other => panic!("expected requester StateUpdate, got {other:?}"),
        }
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), requester_rx.recv())
                .await
                .expect("Resolve All must acknowledge after its state update")
                .expect("requester acknowledgement channel remains open"),
            ServerMessage::ResolveAllResult {
                request_id: 41,
                items_resolved: 1,
                total: 1,
            }
        ));
        match tokio::time::timeout(std::time::Duration::from_secs(1), ai_rx.recv())
            .await
            .expect("Resolve All must fan out the final state to the AI seat")
            .expect("AI recipient channel remains open")
        {
            ServerMessage::StateUpdate { state_revision, .. } => {
                assert_eq!(state_revision, expected_revision);
            }
            other => panic!("expected AI recipient StateUpdate, got {other:?}"),
        }
    }

    #[test]
    fn turn_controller_receives_low_use_window_recommendation_instead_of_controlled_seat() {
        let controlled = PlayerId(0);
        let controller = PlayerId(1);
        let result = low_use_window_priority_result(controlled, Some(controller));

        assert_eq!(state_update_action_fields(&result, controller), (2, true));
        assert_eq!(state_update_action_fields(&result, controlled), (0, false));
    }

    #[test]
    fn ordinary_actor_receives_low_use_window_recommendation_and_nonactor_does_not() {
        let actor = PlayerId(0);
        let nonactor = PlayerId(1);
        let result = low_use_window_priority_result(actor, None);

        assert_eq!(state_update_action_fields(&result, actor), (2, true));
        assert_eq!(state_update_action_fields(&result, nonactor), (0, false));
    }

    #[test]
    fn human_ai_and_takeback_transports_derive_search_authority_from_raw_state() {
        let mut raw = GameState::new_two_player(42);
        raw.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            library_owner: None,
            cards: Vec::new(),
            count: 0,
            reveal: false,
            up_to: true,
            allows_partial_find: true,
            constraint: SearchSelectionConstraint::None,
            ordering_hint: Default::default(),
            split: None,
        };
        raw.active_search_decision_controls
            .insert(ActiveSearchDecisionControl {
                searcher: PlayerId(0),
                searched_zone_owner: PlayerId(0),
                authority: ActiveSearchDecisionAuthority::LatchedController {
                    controller: PlayerId(1),
                },
            });

        let mut filtered = server_core::filter_state_for_player(&raw, PlayerId(0));
        filtered
            .active_search_decision_controls
            .remove(&PlayerId(0));

        for transport in ["human action", "AI follow-up", "takeback"] {
            assert_eq!(
                derive_transport_views(&raw, &filtered, Some(PlayerId(0)))
                    .unique_authorized_submitter,
                Some(PlayerId(1)),
                "{transport} transport must retain raw search authority",
            );
        }
    }
}

#[cfg(test)]
mod ranked_tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_db() -> SharedGameDb {
        let file = NamedTempFile::new().unwrap();
        Arc::new(
            persistence::GameDb::open(file.path(), persistence::SessionRetention::Multiplayer)
                .unwrap(),
        )
    }

    #[test]
    fn ranked_result_persists_distinct_human_duel_ratings() {
        let db = test_db();
        let players = RankedDuelPlayers {
            player_a_name: "Alice".to_string(),
            player_b_name: "Bob".to_string(),
        };

        let result = ranked_result_for_duel(&db, "RANK01", &players, Some(PlayerId(0))).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].rating_before, 1200);
        assert_eq!(result[0].rating_after, 1212);
        assert_eq!(result[0].rating_delta, 12);
        assert_eq!(result[1].rating_after, 1188);
        assert_eq!(db.load_rating("alice").unwrap(), Some(1212));
        assert_eq!(db.load_rating("bob").unwrap(), Some(1188));
    }

    #[test]
    fn ranked_result_rejects_duplicate_or_blank_player_keys() {
        let db = test_db();
        let duplicate = RankedDuelPlayers {
            player_a_name: "Alice".to_string(),
            player_b_name: " alice ".to_string(),
        };
        let blank = RankedDuelPlayers {
            player_a_name: "Alice".to_string(),
            player_b_name: " ".to_string(),
        };

        assert!(ranked_result_for_duel(&db, "RANK02", &duplicate, Some(PlayerId(0))).is_none());
        assert!(ranked_result_for_duel(&db, "RANK03", &blank, Some(PlayerId(0))).is_none());
        assert_eq!(db.load_rating("alice").unwrap(), None);
    }

    #[test]
    fn ranked_duel_players_require_ranked_two_human_seats() {
        let display_names = vec!["Alice".to_string(), "Bob".to_string()];

        assert!(ranked_duel_players_for_room(true, 2, false, &display_names).is_some());
        assert!(ranked_duel_players_for_room(false, 2, false, &display_names).is_none());
        assert!(ranked_duel_players_for_room(true, 3, false, &display_names).is_none());
        assert!(ranked_duel_players_for_room(true, 2, true, &display_names).is_none());
    }
}

#[cfg(test)]
mod lobby_subscriber_tests {
    use super::*;
    use server_core::lobby_subscriber_wire_guard::MAX_LOBBY_SUBSCRIBERS;

    #[tokio::test]
    async fn lobby_subscriber_reservation_rejects_when_at_cap() {
        let subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
        let mut receivers = Vec::new();
        {
            let mut subs = subscribers.lock().await;
            for _ in 0..MAX_LOBBY_SUBSCRIBERS {
                let (tx, rx) = mpsc::unbounded_channel();
                subs.push(tx);
                receivers.push(rx);
            }
        }
        let (overflow_tx, _overflow_rx) = mpsc::unbounded_channel();

        let err = reserve_lobby_subscriber_slot(&subscribers, &overflow_tx)
            .await
            .unwrap_err();

        assert!(err.contains("maximum"));
        assert_eq!(subscribers.lock().await.len(), MAX_LOBBY_SUBSCRIBERS);
        drop(receivers);
    }

    #[tokio::test]
    async fn lobby_subscriber_reservation_prunes_closed_senders_before_cap_check() {
        let subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
        {
            let mut subs = subscribers.lock().await;
            for _ in 0..MAX_LOBBY_SUBSCRIBERS {
                let (tx, rx) = mpsc::unbounded_channel();
                drop(rx);
                subs.push(tx);
            }
        }
        let (new_tx, _new_rx) = mpsc::unbounded_channel();

        reserve_lobby_subscriber_slot(&subscribers, &new_tx)
            .await
            .expect("closed senders should be pruned before enforcing cap");

        assert_eq!(subscribers.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn lobby_subscriber_reservation_is_idempotent_for_same_channel() {
        let subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::unbounded_channel();

        reserve_lobby_subscriber_slot(&subscribers, &tx)
            .await
            .unwrap();
        reserve_lobby_subscriber_slot(&subscribers, &tx)
            .await
            .unwrap();

        assert_eq!(subscribers.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn remove_subscriber_outbound_removes_current_channel_and_closed_senders() {
        let subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
        let player_count = Arc::new(AtomicU32::new(0));
        let (current_tx, _current_rx) = mpsc::unbounded_channel();
        let (live_tx, _live_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed_rx) = mpsc::unbounded_channel();
        drop(closed_rx);
        {
            let mut subs = subscribers.lock().await;
            subs.push(current_tx.clone());
            subs.push(live_tx.clone());
            subs.push(closed_tx);
        }

        apply_outbounds(
            vec![Outbound::RemoveSubscriber],
            &current_tx,
            &subscribers,
            &player_count,
        )
        .await;

        let subs = subscribers.lock().await;
        assert_eq!(subs.len(), 1);
        assert!(subs[0].same_channel(&live_tx));
    }
}

#[cfg(test)]
mod live_spectator_tests {
    use super::*;
    use server_core::spectator_wire_guard::{
        MAX_DRAFT_SPECTATORS_PER_DRAFT, MAX_GAME_SPECTATORS_PER_GAME,
    };

    #[test]
    fn spectator_state_update_keeps_public_status_without_actions() {
        let mut state = GameState::new_two_player(42);
        state.eliminated_players.push(PlayerId(1));

        let msg =
            build_spectator_state_update_message(&state, &[], &[], 1).expect("fixture snapshot");

        match msg {
            ServerMessage::StateUpdate {
                legal_actions,
                auto_pass_recommended,
                eliminated_players,
                spell_costs,
                legal_actions_by_object,
                ..
            } => {
                assert!(legal_actions.is_empty());
                assert!(!auto_pass_recommended);
                assert_eq!(eliminated_players, vec![PlayerId(1)]);
                assert!(spell_costs.is_empty());
                assert!(legal_actions_by_object.is_empty());
            }
            other => panic!("expected spectator StateUpdate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn game_spectator_reservation_rejects_when_game_is_at_cap() {
        let spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
        let mut receivers = Vec::new();
        {
            let mut specs = spectators.lock().await;
            let game_spectators = specs.entry("FULL".to_string()).or_default();
            for _ in 0..MAX_GAME_SPECTATORS_PER_GAME {
                let (tx, rx) = mpsc::unbounded_channel();
                game_spectators.push(tx);
                receivers.push(rx);
            }
        }
        let (overflow_tx, _overflow_rx) = mpsc::unbounded_channel();

        let err = reserve_game_spectator_slot(&spectators, "FULL", &overflow_tx)
            .await
            .unwrap_err();

        assert!(err.contains("maximum"));
        assert_eq!(
            spectators.lock().await.get("FULL").map(Vec::len),
            Some(MAX_GAME_SPECTATORS_PER_GAME)
        );
        drop(receivers);
    }

    #[tokio::test]
    async fn game_spectator_reservation_prunes_closed_senders_before_cap_check() {
        let spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut specs = spectators.lock().await;
            let game_spectators = specs.entry("PRUNE".to_string()).or_default();
            for _ in 0..MAX_GAME_SPECTATORS_PER_GAME {
                let (tx, rx) = mpsc::unbounded_channel();
                drop(rx);
                game_spectators.push(tx);
            }
        }
        let (new_tx, _new_rx) = mpsc::unbounded_channel();

        reserve_game_spectator_slot(&spectators, "PRUNE", &new_tx)
            .await
            .expect("closed senders should be pruned before enforcing cap");

        assert_eq!(spectators.lock().await.get("PRUNE").map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn game_spectator_reservation_is_idempotent_for_same_channel() {
        let spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel();

        reserve_game_spectator_slot(&spectators, "SAME", &tx)
            .await
            .unwrap();
        reserve_game_spectator_slot(&spectators, "SAME", &tx)
            .await
            .unwrap();

        assert_eq!(spectators.lock().await.get("SAME").map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn game_spectator_switch_keeps_previous_game_when_new_game_is_full() {
        let spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
        let (current_tx, _current_rx) = mpsc::unbounded_channel();
        let mut full_receivers = Vec::new();
        {
            let mut specs = spectators.lock().await;
            specs
                .entry("CURRENT".to_string())
                .or_default()
                .push(current_tx.clone());
            let full_game = specs.entry("FULL".to_string()).or_default();
            for _ in 0..MAX_GAME_SPECTATORS_PER_GAME {
                let (tx, rx) = mpsc::unbounded_channel();
                full_game.push(tx);
                full_receivers.push(rx);
            }
        }

        let err = switch_game_spectator_slot(&spectators, Some("CURRENT"), "FULL", &current_tx)
            .await
            .unwrap_err();

        assert!(err.contains("maximum"));
        let specs = spectators.lock().await;
        assert_eq!(specs.get("CURRENT").map(Vec::len), Some(1));
        assert_eq!(
            specs.get("FULL").map(Vec::len),
            Some(MAX_GAME_SPECTATORS_PER_GAME)
        );
    }

    #[tokio::test]
    async fn draft_spectator_reservation_rejects_when_draft_is_at_cap() {
        let spectators: SharedDraftSpectators = Arc::new(Mutex::new(HashMap::new()));
        let visibility = draft_core::types::SpectatorVisibility::default();
        let mut receivers = Vec::new();
        {
            let mut specs = spectators.lock().await;
            let draft_spectators = specs.entry("FULL".to_string()).or_default();
            for _ in 0..MAX_DRAFT_SPECTATORS_PER_DRAFT {
                let (tx, rx) = mpsc::unbounded_channel();
                draft_spectators.push((visibility, tx));
                receivers.push(rx);
            }
        }
        let (overflow_tx, _overflow_rx) = mpsc::unbounded_channel();

        let err = reserve_draft_spectator_slot(&spectators, "FULL", visibility, &overflow_tx)
            .await
            .unwrap_err();

        assert!(err.contains("maximum"));
        assert_eq!(
            spectators.lock().await.get("FULL").map(Vec::len),
            Some(MAX_DRAFT_SPECTATORS_PER_DRAFT)
        );
        drop(receivers);
    }

    #[tokio::test]
    async fn draft_spectator_reservation_prunes_closed_senders_before_cap_check() {
        let spectators: SharedDraftSpectators = Arc::new(Mutex::new(HashMap::new()));
        let visibility = draft_core::types::SpectatorVisibility::default();
        {
            let mut specs = spectators.lock().await;
            let draft_spectators = specs.entry("PRUNE".to_string()).or_default();
            for _ in 0..MAX_DRAFT_SPECTATORS_PER_DRAFT {
                let (tx, rx) = mpsc::unbounded_channel();
                drop(rx);
                draft_spectators.push((visibility, tx));
            }
        }
        let (new_tx, _new_rx) = mpsc::unbounded_channel();

        reserve_draft_spectator_slot(&spectators, "PRUNE", visibility, &new_tx)
            .await
            .expect("closed senders should be pruned before enforcing cap");

        assert_eq!(spectators.lock().await.get("PRUNE").map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn draft_spectator_reservation_is_idempotent_for_same_channel() {
        let spectators: SharedDraftSpectators = Arc::new(Mutex::new(HashMap::new()));
        let visibility = draft_core::types::SpectatorVisibility::default();
        let (tx, _rx) = mpsc::unbounded_channel();

        reserve_draft_spectator_slot(&spectators, "SAME", visibility, &tx)
            .await
            .unwrap();
        reserve_draft_spectator_slot(&spectators, "SAME", visibility, &tx)
            .await
            .unwrap();

        assert_eq!(spectators.lock().await.get("SAME").map(Vec::len), Some(1));
    }
}

#[cfg(test)]
mod full_create_guard_tests {
    use super::*;
    use lobby_broker::validation::{MAX_DRAFT_SET_CODE_LEN, MAX_TOKEN_LEN};
    use server_core::protocol::{AiSeatRequest, DeckData, DraftLobbyMetadata};

    fn deck() -> DeckData {
        DeckData {
            main_deck: vec!["Forest".into()],
            ..Default::default()
        }
    }

    fn fields<'a>(
        deck: &'a DeckData,
        host_peer_id: Option<&'a str>,
        draft_metadata: Option<&'a DraftLobbyMetadata>,
    ) -> lobby_broker::CreateGameSettingsInbound<'a> {
        lobby_broker::CreateGameSettingsInbound {
            deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id,
            draft_metadata,
        }
    }

    #[test]
    fn full_create_guard_accepts_valid_peer_and_draft_metadata() {
        let deck = deck();
        let draft = DraftLobbyMetadata {
            set_code: "TST".to_string(),
            draft_kind: "Premier".to_string(),
            cube_name: Some("Cube".to_string()),
        };

        let player_count = guard_full_create_game_settings_inbound(
            fields(&deck, Some("peer-host"), Some(&draft)),
            &[],
        )
        .unwrap();

        assert_eq!(player_count, 2);
    }

    #[test]
    fn full_create_guard_rejects_oversized_host_peer_id() {
        let deck = deck();
        let host_peer_id = "p".repeat(MAX_TOKEN_LEN + 1);

        let err =
            guard_full_create_game_settings_inbound(fields(&deck, Some(&host_peer_id), None), &[])
                .unwrap_err();

        assert!(err.contains("host_peer_id"));
    }

    #[test]
    fn full_create_guard_rejects_oversized_draft_metadata() {
        let deck = deck();
        let draft = DraftLobbyMetadata {
            set_code: "s".repeat(MAX_DRAFT_SET_CODE_LEN + 1),
            draft_kind: "Premier".to_string(),
            cube_name: None,
        };

        let err = guard_full_create_game_settings_inbound(fields(&deck, None, Some(&draft)), &[])
            .unwrap_err();

        assert!(err.contains("draft_metadata.set_code"));
    }

    #[test]
    fn full_create_guard_rejects_archenemy_seat_outside_player_count() {
        let deck = deck();
        let mut fields = fields(&deck, None, None);
        let mut format_config = engine::types::format::FormatConfig::archenemy();
        format_config.archenemy_player = Some(engine::types::player::PlayerId(2));
        fields.format_config = Some(&format_config);

        let err = guard_full_create_game_settings_inbound(fields, &[]).unwrap_err();

        assert!(err.contains("archenemy_player"));
    }

    #[test]
    fn full_create_guard_rejects_limited_range_until_supported() {
        let deck = deck();
        let mut fields = fields(&deck, None, None);
        let mut format_config = engine::types::format::FormatConfig::standard();
        format_config.range_of_influence =
            Some(Box::new(engine::types::format::RangeOfInfluenceConfig {
                default_range: 0,
                player_overrides: std::collections::BTreeMap::new(),
            }));
        fields.format_config = Some(&format_config);

        let err = guard_full_create_game_settings_inbound(fields, &[]).unwrap_err();

        assert!(err.contains("range_of_influence"));
    }

    #[test]
    fn full_create_guard_rejects_ai_seats_before_deck_payload() {
        let mut deck = deck();
        deck.main_deck =
            vec!["Forest".to_string(); lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES + 1];
        let ai_seats = vec![AiSeatRequest {
            seat_index: 0,
            difficulty: phase_ai::config::AiDifficulty::Medium,
            deck_name: None,
            deck: None,
        }];

        let err = guard_full_create_game_settings_inbound(fields(&deck, None, None), &ai_seats)
            .unwrap_err();

        assert!(err.contains("ai_seats[0].seat_index"));
    }
}

#[cfg(test)]
mod issue_4548_full_create_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use phase_ai::config::AiDifficulty;
    use server_core::protocol::{
        AiSeatRequest, ClientMessage, DeckChoice, DeckData, ServerErrorCode, ServerMessage,
    };
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::WebSocketStream;

    fn empty_deck() -> DeckData {
        DeckData::default()
    }

    fn deck_with_main_entries(entries: usize) -> DeckData {
        DeckData {
            main_deck: vec!["Forest".to_string(); entries],
            ..Default::default()
        }
    }

    pub(super) async fn spawn_full_mode_server(
    ) -> (String, tokio::task::JoinHandle<()>, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let game_db = Arc::new(
            persistence::GameDb::open(
                &temp_dir.path().join("games.db"),
                persistence::SessionRetention::Multiplayer,
            )
            .expect("game db"),
        );
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(AppState {
                sessions: Arc::new(Mutex::new(SessionManager::new())),
                draft_sessions: Arc::new(Mutex::new(DraftSessionManager::new())),
                draft_pools: Arc::new(draft_pools::DraftPools::default()),
                connections: Arc::new(Mutex::new(HashMap::new())),
                db: Arc::new(CardDatabase::default()),
                lobby: Arc::new(Mutex::new(Broker::new())),
                lobby_subscribers: Arc::new(Mutex::new(Vec::new())),
                player_count: Arc::new(AtomicU32::new(0)),
                game_db,
                draft_spectators: Arc::new(Mutex::new(HashMap::new())),
                game_spectators: Arc::new(Mutex::new(HashMap::new())),
                mode: ServerMode::Full,
                public_url: None,
                allowed_origin: None,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        (format!("ws://{addr}/ws"), handle, temp_dir)
    }

    pub(super) async fn recv_server_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let msg = socket
            .next()
            .await
            .expect("websocket message")
            .expect("websocket frame");
        match msg {
            WsMessage::Text(text) => serde_json::from_str(&text).expect("server message"),
            other => panic!("expected text server message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_mode_create_sends_slots_after_game_created() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");

            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));

            let hello = ClientMessage::ClientHello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                build_commit: build_commit().to_string(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&hello).expect("hello json").into(),
                ))
                .await
                .expect("send hello");

            let create = ClientMessage::CreateGameWithSettings {
                deck: empty_deck(),
                display_name: "Alice".to_string(),
                public: true,
                password: None,
                timer_seconds: None,
                player_count: 2,
                match_config: Default::default(),
                ai_seats: Vec::new(),
                format_config: None,
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&create).expect("create json").into(),
                ))
                .await
                .expect("send create");

            let mut game_code = None;
            let mut saw_slots = false;
            while game_code.is_none() || !saw_slots {
                match recv_server_message(&mut socket).await {
                    ServerMessage::GameCreated {
                        game_code: code, ..
                    } => game_code = Some(code),
                    ServerMessage::PlayerSlotsUpdate { slots } => {
                        assert_eq!(slots.len(), 2);
                        assert_eq!(slots[0].name, "Alice");
                        saw_slots = true;
                    }
                    _ => {}
                }
            }

            game_code.expect("created game code")
        })
        .await;
        server.abort();

        assert!(
            result.is_ok(),
            "full-mode create deadlocked before slot broadcast"
        );
    }

    #[tokio::test]
    async fn full_mode_create_rejects_format_invalid_host_deck() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");

            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));

            let hello = ClientMessage::ClientHello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                build_commit: build_commit().to_string(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&hello).expect("hello json").into(),
                ))
                .await
                .expect("send hello");

            let create = ClientMessage::CreateGameWithSettings {
                deck: empty_deck(),
                display_name: "Alice".to_string(),
                public: true,
                password: None,
                timer_seconds: None,
                player_count: 2,
                match_config: Default::default(),
                ai_seats: Vec::new(),
                format_config: Some(engine::types::format::FormatConfig::standard()),
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&create).expect("create json").into(),
                ))
                .await
                .expect("send create");

            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Error {
                    code: Some(ServerErrorCode::DeckRejected),
                    ..
                }
            ));
        })
        .await;
        server.abort();

        assert!(
            result.is_ok(),
            "full-mode create did not reject the invalid format deck"
        );
    }

    #[tokio::test]
    async fn full_mode_accepts_native_multi_ai_setup_larger_than_eight_kib() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");

            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));

            let hello = ClientMessage::ClientHello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                build_commit: build_commit().to_string(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&hello).expect("hello json").into(),
                ))
                .await
                .expect("send hello");

            let create = ClientMessage::CreateGameWithSettings {
                deck: deck_with_main_entries(300),
                display_name: "Alice".to_string(),
                public: false,
                password: None,
                timer_seconds: None,
                player_count: 3,
                match_config: Default::default(),
                ai_seats: vec![
                    AiSeatRequest {
                        seat_index: 1,
                        difficulty: AiDifficulty::Medium,
                        deck_name: None,
                        deck: Some(DeckChoice::DeckList(Box::new(deck_with_main_entries(300)))),
                    },
                    AiSeatRequest {
                        seat_index: 2,
                        difficulty: AiDifficulty::Medium,
                        deck_name: None,
                        deck: Some(DeckChoice::DeckList(Box::new(deck_with_main_entries(300)))),
                    },
                ],
                format_config: None,
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
            };
            let create_json = serde_json::to_string(&create).expect("create json");
            assert!(create_json.len() > 8 * 1024);
            assert!(create_json.len() <= MAX_WS_MESSAGE_BYTES);
            socket
                .send(WsMessage::Text(create_json.into()))
                .await
                .expect("send create");

            // The empty test card database rejects the deck, which proves the
            // complete multi-AI frame passed WebSocket framing and reached the
            // normal create-game validation path.
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Error { .. }
            ));
        })
        .await;
        server.abort();

        assert!(
            result.is_ok(),
            "native multi-AI setup frame did not reach server validation"
        );
    }
}

/// End-to-end coverage for the shared game-submission handler
/// ([`handle_full_game_submission`]) over a real socket.
///
/// Before this module existed, `ClientMessage::Action` had **no** end-to-end
/// test through `handle_client_message` at all: every `ClientMessage::Action`
/// occurrence in this file's test modules exercised `reject_if_disabled` or
/// `classify_hello_gate` as pure functions. These tests are what gate the
/// extraction of that arm into a shared handler.
#[cfg(test)]
mod game_submission_tests {
    use super::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};
    use super::*;
    use engine::game::interaction::MAX_INTERACTION_STRING_LEN;
    use engine::types::actions::DebugAction;
    use engine::types::interaction::{InteractionChoiceId, InteractionId, InteractionResponse};
    use engine::types::zones::Zone;
    use futures_util::SinkExt;
    use server_core::game_action_payload_guard::MAX_ACTION_LIST_LEN;
    use server_core::protocol::DeckData;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::MaybeTlsStream;
    use tokio_tungstenite::WebSocketStream;

    #[test]
    fn zero_count_debug_create_is_the_only_submission_no_op() {
        let create_card = |count| {
            GameSubmission::Action(GameAction::Debug(DebugAction::CreateCard {
                card_name: "Lightning Bolt".to_string(),
                owner: PlayerId(0),
                zone: Zone::Hand,
                count,
                attach_to: None,
                run_etb: false,
                nonlegendary: false,
            }))
        };

        assert!(create_card(0).is_zero_count_debug_create());
        assert!(!create_card(1).is_zero_count_debug_create());
        assert!(!GameSubmission::Action(GameAction::PassPriority).is_zero_count_debug_create());
    }

    /// Connect, handshake, and create a two-seat game so the socket carries an
    /// authenticated `SocketIdentity` with both a `game_code` and a
    /// `player_token`.
    ///
    /// Seat 1 never joins, so the game never *starts* — which is exactly the
    /// reachable surface these tests need: everything up to and including
    /// `SessionManager`'s verdict and the `Err(e) => ActionRejected` arm. The
    /// `Ok(..)` broadcast fan-out is not reachable over this harness, because
    /// `spawn_full_mode_server` builds `AppState` with an empty
    /// `CardDatabase::default()`.
    async fn create_authenticated_game_socket(
        url: String,
    ) -> WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        assert!(matches!(
            recv_server_message(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ));

        let hello = ClientMessage::ClientHello {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            build_commit: build_commit().to_string(),
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
        };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&hello).expect("hello json").into(),
            ))
            .await
            .expect("send hello");

        let create = ClientMessage::CreateGameWithSettings {
            deck: DeckData::default(),
            display_name: "Alice".to_string(),
            public: false,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: Default::default(),
            ai_seats: Vec::new(),
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            start_when_full: true,
            ranked: false,
        };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&create).expect("create json").into(),
            ))
            .await
            .expect("send create");

        let mut saw_created = false;
        let mut saw_slots = false;
        while !saw_created || !saw_slots {
            match recv_server_message(&mut socket).await {
                ServerMessage::GameCreated { .. } => saw_created = true,
                ServerMessage::PlayerSlotsUpdate { .. } => saw_slots = true,
                _ => {}
            }
        }

        socket
    }

    /// Read frames until one is an `ActionRejected` or an `Error`, ignoring the
    /// unrelated broadcasts the session emits. The enclosing
    /// `tokio::time::timeout` is the failure mode, as in every sibling test.
    async fn recv_submission_answer<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let msg = recv_server_message(socket).await;
            if matches!(
                msg,
                ServerMessage::ActionRejected { .. } | ServerMessage::Error { .. }
            ) {
                return msg;
            }
        }
    }

    /// Gates the extraction of the `ClientMessage::Action` arm body into
    /// [`handle_full_game_submission`]: an extraction that broke identity
    /// extraction, the payload guard, the lock, `player_for_token`, or the
    /// `Err(e) => ActionRejected` arm fails here.
    ///
    /// `GrantDebugPermission` is chosen over `PassPriority` deliberately.
    /// `GameState::new` initializes `waiting_for: WaitingFor::Priority` with the
    /// host holding priority, so a `PassPriority` from this socket may well be
    /// *accepted* — which would make the assertion pass for the wrong reason.
    /// The sandbox refusal, by contrast, is decidable without running the
    /// engine: the action is Full-mode allowed, is a payloadless no-op in
    /// `guard_game_action_payload`, and hits `handle_action`'s Grant/Revoke gate
    /// *first* — before the seat check, before `debug_permitted`, and before
    /// `apply` — where `format_config.allow_debug_actions` is `false` because
    /// the wire sent `format_config: None` and `FormatConfig::standard()` sets
    /// it to `false`. `handle_action`'s `Err(String)` is then answered verbatim,
    /// with no `"Engine error: "` prefix.
    #[tokio::test]
    async fn action_frame_reaches_the_shared_submission_handler() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let answer = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = create_authenticated_game_socket(url).await;

            let action = ClientMessage::Action {
                action: GameAction::GrantDebugPermission {
                    player_id: PlayerId(1),
                },
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&action).expect("action json").into(),
                ))
                .await
                .expect("send action");

            recv_submission_answer(&mut socket).await
        })
        .await;
        server.abort();

        let answer = answer.expect("action frame was never answered");
        match answer {
            ServerMessage::ActionRejected { reason } => {
                assert_eq!(reason, "Sandbox mode is not enabled for this game");
            }
            other => panic!("expected ActionRejected from the shared handler, got {other:?}"),
        }
    }

    fn submission(response: InteractionResponse) -> InteractionSubmission {
        InteractionSubmission {
            interaction_id: InteractionId("interaction-1".to_string()),
            response,
        }
    }

    fn oversized_submission() -> InteractionSubmission {
        submission(InteractionResponse::Text {
            value: "x".repeat(MAX_INTERACTION_STRING_LEN + 1),
        })
    }

    /// The real cross-check of the two independent channel declarations.
    ///
    /// Both `GameSubmission::payload_rejection` and `wire_rejection_message`
    /// are pure functions, so they can be compared directly. An end-to-end
    /// socket test structurally cannot do this: the wire guard returns before
    /// dispatch, so no frame ever reaches both layers.
    #[test]
    fn handler_payload_channels_agree_with_the_wire() {
        let oversized = oversized_submission();
        let oversized_action = GameAction::ReorderHand {
            order: vec![engine::types::identifiers::ObjectId(1); MAX_ACTION_LIST_LEN + 1],
        };

        // (i) an oversized interaction answers on the benign channel.
        let handler_reason = match *GameSubmission::Interaction(oversized.clone())
            .payload_rejection()
            .expect_err("an oversized interaction is refused")
        {
            ServerMessage::ActionRejected { reason } => reason,
            ref other => panic!("an oversized paste must not tear the session down: {other:?}"),
        };

        // (ii) an oversized action stays a malformed frame.
        let action_reason = match *GameSubmission::Action(oversized_action.clone())
            .payload_rejection()
            .expect_err("an oversized action is refused")
        {
            ServerMessage::Error { message, .. } => message,
            ref other => panic!("an oversized action is a malformed frame, got {other:?}"),
        };

        // (iii) both layers agree, in variant *and* in reason string.
        let wire_interaction = ClientMessage::Interaction {
            submission: oversized,
        };
        let wire_reason =
            guard_client_message_before_dispatch(&wire_interaction, ServerMode::Full).unwrap_err();
        match wire_rejection_message(&wire_interaction, wire_reason) {
            ServerMessage::ActionRejected { reason } => assert_eq!(reason, handler_reason),
            other => panic!("wire and handler disagree on the interaction channel: {other:?}"),
        }

        let wire_action = ClientMessage::Action {
            action: oversized_action,
        };
        let wire_action_reason =
            guard_client_message_before_dispatch(&wire_action, ServerMode::Full).unwrap_err();
        match wire_rejection_message(&wire_action, wire_action_reason) {
            ServerMessage::Error { message, .. } => assert_eq!(message, action_reason),
            other => panic!("wire and handler disagree on the action channel: {other:?}"),
        }

        // Reach guard: without it, a `payload_rejection` that always errored
        // would satisfy (i) and (ii).
        assert!(
            GameSubmission::Interaction(submission(InteractionResponse::Choose {
                choice_id: InteractionChoiceId("a".to_string()),
            }))
            .payload_rejection()
            .is_ok()
        );
        assert!(GameSubmission::Action(GameAction::PassPriority)
            .payload_rejection()
            .is_ok());
    }

    /// The revert-failing assertion for #6941.
    ///
    /// Before the fix, the frame never reaches a handler at all: serde answers
    /// `ServerMessage::Error { message: "Invalid message: unknown variant
    /// `Interaction` ..." }` — a different variant *and* a different string.
    ///
    /// Asserting the exact reason is the reach guard: only a frame that
    /// traversed serde, `reject_if_disabled`, the wire guard, both identity
    /// checks, `payload_rejection`, `state.lock()`, `player_for_token`,
    /// `submit_interaction`, and `slot_for_submission`'s
    /// `.ok_or(StaleInteraction)` can produce it.
    #[tokio::test]
    async fn interaction_frame_is_accepted_by_the_wire_schema() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let answer = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = create_authenticated_game_socket(url).await;

            let frame = ClientMessage::Interaction {
                submission: submission(InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("no-such-choice".to_string()),
                }),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&frame)
                        .expect("interaction json")
                        .into(),
                ))
                .await
                .expect("send interaction");

            recv_submission_answer(&mut socket).await
        })
        .await;
        server.abort();

        let answer = answer.expect("interaction frame was never answered");
        match answer {
            ServerMessage::ActionRejected { reason } => {
                assert_eq!(reason, "Engine error: StaleInteraction");
            }
            other => panic!("the wire schema must accept an Interaction frame, got {other:?}"),
        }
    }

    /// Scope note: this exercises the **wire** layer only — the guard returns
    /// before dispatch, so the handler's `payload_rejection` is never reached
    /// by this frame. It makes no claim about the handler layer; that agreement
    /// is pinned by `handler_payload_channels_agree_with_the_wire`.
    #[tokio::test]
    async fn an_oversized_interaction_is_answered_on_the_benign_channel() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let answers = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = create_authenticated_game_socket(url).await;

            let frame = ClientMessage::Interaction {
                submission: oversized_submission(),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&frame)
                        .expect("oversized json")
                        .into(),
                ))
                .await
                .expect("send oversized interaction");

            let first = recv_submission_answer(&mut socket).await;

            // Liveness probe rather than a vacuous `!matches!`: a socket that
            // had been answered with `ServerMessage::Error` would have been
            // torn down client-side, so a second answer on the same socket is
            // what proves the benign channel was used.
            let bounded = ClientMessage::Interaction {
                submission: submission(InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("no-such-choice".to_string()),
                }),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&bounded)
                        .expect("bounded json")
                        .into(),
                ))
                .await
                .expect("send bounded interaction");

            (first, recv_submission_answer(&mut socket).await)
        })
        .await;
        server.abort();

        let (first, second) = answers.expect("oversized interaction was never answered");
        match first {
            ServerMessage::ActionRejected { reason } => {
                assert_eq!(reason, "Engine error: PayloadTooLarge");
            }
            other => panic!("an oversized paste must not end the match, got {other:?}"),
        }
        assert!(
            matches!(second, ServerMessage::ActionRejected { .. }),
            "the socket must still be live after a bounds rejection, got {second:?}"
        );
    }

    /// Pins the pre-session rows of the channel table, and is the discriminator
    /// for `interaction_frame_is_accepted_by_the_wire_schema`: it proves that
    /// test's `ActionRejected` came from an engine verdict rather than being
    /// this handler's blanket answer.
    #[tokio::test]
    async fn a_game_submission_without_a_session_is_answered_on_the_error_channel() {
        let (url, server, _temp_dir) = spawn_full_mode_server().await;
        let answer = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");

            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));

            let hello = ClientMessage::ClientHello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                build_commit: build_commit().to_string(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&hello).expect("hello json").into(),
                ))
                .await
                .expect("send hello");

            let frame = ClientMessage::Interaction {
                submission: submission(InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("a".to_string()),
                }),
            };
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&frame)
                        .expect("interaction json")
                        .into(),
                ))
                .await
                .expect("send interaction");

            recv_submission_answer(&mut socket).await
        })
        .await;
        server.abort();

        let answer = answer.expect("sessionless interaction was never answered");
        match answer {
            ServerMessage::Error { message, .. } => assert_eq!(message, "Not in a game"),
            other => panic!("a pre-session condition is not an engine verdict: {other:?}"),
        }
    }
}

#[cfg(test)]
mod mode_gate_tests {
    use super::*;
    use engine::types::actions::GameAction;
    use server_core::protocol::DeckData;

    fn deck() -> DeckData {
        DeckData {
            main_deck: vec!["Forest".into()],
            ..Default::default()
        }
    }

    #[test]
    fn lobby_only_rejects_game_state_messages() {
        let disabled: Vec<ClientMessage> = vec![
            ClientMessage::CreateGame { deck: deck() },
            ClientMessage::JoinGame {
                game_code: "X".into(),
                deck: deck(),
            },
            ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            ClientMessage::PreviewManaPayment {
                request_id: 1,
                action: GameAction::PassPriority,
            },
            ClientMessage::Reconnect {
                game_code: "X".into(),
                player_token: "t".into(),
                full_key: server_core::FullSessionKey {
                    game_code: "X".into(),
                    generation: 1,
                },
            },
            ClientMessage::AbandonGame,
            ClientMessage::Concede,
            ClientMessage::ConcedeMatch,
            ClientMessage::Emote { emote: "GG".into() },
            ClientMessage::SpectatorJoin {
                game_code: "X".into(),
            },
            ClientMessage::CreateDraftWithSettings {
                display_name: "A".into(),
                set_code: "TST".into(),
                kind: draft_core::types::DraftKind::Premier,
                public: true,
                password: None,
                timer_seconds: None,
                tournament_format: draft_core::types::TournamentFormat::Swiss,
                pod_policy: draft_core::types::PodPolicy::Competitive,
                pod_size: 8,
            },
            ClientMessage::JoinDraftWithPassword {
                draft_code: "X".into(),
                display_name: "B".into(),
                password: None,
            },
            ClientMessage::DraftAction {
                draft_code: "X".into(),
                action: draft_core::types::DraftAction::StartDraft,
            },
            ClientMessage::ReconnectDraft {
                draft_code: "X".into(),
                player_token: "t".into(),
            },
            ClientMessage::RequestTakeback(None),
            ClientMessage::RespondTakeback { approve: true },
            ClientMessage::CancelTakeback,
        ];
        for msg in disabled {
            assert!(
                reject_if_disabled(&msg, ServerMode::LobbyOnly).is_some(),
                "expected {msg:?} to be rejected in lobby-only mode"
            );
        }
    }

    #[test]
    fn lobby_only_allows_broker_and_lifecycle_messages() {
        let allowed: Vec<ClientMessage> = vec![
            ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            },
            ClientMessage::SubscribeLobby,
            ClientMessage::UnsubscribeLobby,
            ClientMessage::Ping { timestamp: 0 },
            ClientMessage::UpdateLobbyMetadata {
                game_code: "X".into(),
                current_players: 2,
                max_players: 4,
                consumed_reservation_tokens: Vec::new(),
            },
            ClientMessage::UnregisterLobby {
                game_code: "X".into(),
            },
        ];
        for msg in allowed {
            assert!(
                reject_if_disabled(&msg, ServerMode::LobbyOnly).is_none(),
                "expected {msg:?} to be allowed in lobby-only mode"
            );
        }
    }

    #[test]
    fn full_mode_rejects_lobby_only_messages() {
        let msgs = vec![
            ClientMessage::UpdateLobbyMetadata {
                game_code: "X".into(),
                current_players: 2,
                max_players: 4,
                consumed_reservation_tokens: Vec::new(),
            },
            ClientMessage::UnregisterLobby {
                game_code: "X".into(),
            },
        ];
        for msg in msgs {
            assert!(
                reject_if_disabled(&msg, ServerMode::Full).is_some(),
                "expected {msg:?} to be rejected in full mode"
            );
        }
    }

    #[test]
    fn full_mode_allows_game_state_messages() {
        let msgs: Vec<ClientMessage> = vec![
            ClientMessage::CreateGame { deck: deck() },
            ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            ClientMessage::PreviewManaPayment {
                request_id: 1,
                action: GameAction::PassPriority,
            },
            ClientMessage::AbandonGame,
            ClientMessage::Concede,
            ClientMessage::ConcedeMatch,
            ClientMessage::Ping { timestamp: 0 },
            ClientMessage::CreateDraftWithSettings {
                display_name: "A".into(),
                set_code: "TST".into(),
                kind: draft_core::types::DraftKind::Premier,
                public: true,
                password: None,
                timer_seconds: None,
                tournament_format: draft_core::types::TournamentFormat::Swiss,
                pod_policy: draft_core::types::PodPolicy::Competitive,
                pod_size: 8,
            },
            ClientMessage::DraftAction {
                draft_code: "X".into(),
                action: draft_core::types::DraftAction::StartDraft,
            },
            ClientMessage::RequestTakeback(None),
            ClientMessage::RespondTakeback { approve: true },
            ClientMessage::CancelTakeback,
        ];
        for m in msgs {
            assert!(reject_if_disabled(&m, ServerMode::Full).is_none());
        }
    }

    fn interaction_frame() -> ClientMessage {
        ClientMessage::Interaction {
            submission: InteractionSubmission {
                interaction_id: engine::types::interaction::InteractionId("i-1".to_string()),
                response: engine::types::interaction::InteractionResponse::Choose {
                    choice_id: engine::types::interaction::InteractionChoiceId("a".to_string()),
                },
            },
        }
    }

    /// Both halves are required: either alone is satisfiable by a wrong
    /// grouping. A LobbyOnly broker runs no engine and holds no
    /// `SessionManager`, so it publishes no interactions and no client can hold
    /// a live `interaction_id` against it — identical to `Action`.
    #[test]
    fn interaction_is_full_only() {
        assert!(reject_if_disabled(&interaction_frame(), ServerMode::Full).is_none());
        assert!(reject_if_disabled(&interaction_frame(), ServerMode::LobbyOnly).is_some());
    }

    /// Supplies the other half of `server-core`'s
    /// `broker_projection_accepts_an_interaction_without_bounding_it`: leaving
    /// the projection guard unbounded for this variant is safe only because
    /// nothing is ever cloned into the broker.
    ///
    /// The paired positive is required in the same test so a wholesale-`None`
    /// regression in `to_lobby_client_message` cannot satisfy it.
    #[test]
    fn interaction_is_never_projected_into_the_lobby_broker() {
        assert!(to_lobby_client_message(&interaction_frame()).is_none());
        assert!(to_lobby_client_message(&ClientMessage::SubscribeLobby).is_some());
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use engine::types::actions::GameAction;
    use lobby_broker::validation::MAX_TOKEN_LEN;
    use server_core::protocol::DeckData;

    fn empty_identity() -> SocketIdentity {
        SocketIdentity {
            game_code: None,
            player_id: None,
            player_token: None,
            lobby_subscribed: false,
            session_span: None,
            client_hello: None,
            lobby_host_game: None,
            seat_reservations: Vec::new(),
            lobby_reservations: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
            spectator_game_code: None,
        }
    }

    fn empty_deck() -> DeckData {
        DeckData {
            main_deck: vec!["Forest".into()],
            ..Default::default()
        }
    }

    #[test]
    fn accepts_matching_client_hello() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::Accept(ClientHelloInfo {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
            })
        );
    }

    #[test]
    fn rejects_previous_protocol_for_breaking_planechase_release() {
        let previous = PROTOCOL_VERSION.saturating_sub(1);
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.10".into(),
                build_commit: "old1234".into(),
                protocol_version: previous,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: previous,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn accepts_previous_protocol_for_lobby_only_range() {
        let previous = PROTOCOL_VERSION.saturating_sub(1);
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.10".into(),
                build_commit: "old1234".into(),
                protocol_version: previous,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::LobbyOnly),
        );
        assert!(matches!(outcome, HelloGateOutcome::Accept(_)));
    }

    /// The regression this whole change exists for. A client whose full-game
    /// `protocol_version` is many bumps behind the broker is still accepted,
    /// because the lobby gates on the surface it actually speaks. Before the
    /// split this was a `RejectProtocol` and it took preview multiplayer down.
    #[test]
    fn lobby_accepts_stale_full_game_protocol_when_lobby_version_current() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "old1234".into(),
                // Far outside LOBBY_MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION.
                protocol_version: PROTOCOL_VERSION.saturating_sub(9),
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            },
            hello_acceptance(ServerMode::LobbyOnly),
        );
        assert!(matches!(outcome, HelloGateOutcome::Accept(_)));
    }

    /// No ceiling on the lobby surface: a client newer than this broker can
    /// only fail by sending a lobby variant the broker does not know, and
    /// `parse_lobby_client_message` rejects that per-frame as an unknown tag.
    /// Evicting the whole connection would refuse a client over a variant it
    /// may never send.
    #[test]
    fn lobby_accepts_future_lobby_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "9.9.9".into(),
                build_commit: "future12".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION + 5),
            },
            hello_acceptance(ServerMode::LobbyOnly),
        );
        assert!(matches!(outcome, HelloGateOutcome::Accept(_)));
    }

    /// The floor is still enforced — that is what a genuinely breaking lobby
    /// change would raise.
    #[test]
    fn lobby_rejects_below_lobby_floor() {
        let Some(below) = MIN_SUPPORTED_LOBBY_PROTOCOL.checked_sub(1) else {
            // Floor is 0; nothing can sit below it. Nothing to assert.
            return;
        };
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "ancient1".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(below),
            },
            hello_acceptance(ServerMode::LobbyOnly),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: below,
                server: LOBBY_PROTOCOL_VERSION,
            }
        );
    }

    /// A Full server must ignore the lobby field entirely — full-game payloads
    /// are not compatible across a `PROTOCOL_VERSION` bump regardless of what
    /// the client claims about the lobby surface.
    #[test]
    fn full_game_gate_ignores_lobby_protocol_version() {
        let previous = PROTOCOL_VERSION.saturating_sub(1);
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.10".into(),
                build_commit: "old1234".into(),
                protocol_version: previous,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: previous,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn rejects_client_hello_below_min_supported() {
        let too_old = PROTOCOL_VERSION.saturating_sub(1);
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "ancient1".into(),
                protocol_version: too_old,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: too_old,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn rejects_client_hello_with_zero_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: 0,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: 0,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn rejects_client_hello_with_future_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.2.0".into(),
                build_commit: "def5678".into(),
                protocol_version: PROTOCOL_VERSION + 1,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert!(matches!(outcome, HelloGateOutcome::RejectProtocol { .. }));
    }

    #[test]
    fn rejects_oversized_client_hello_fields() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "v".repeat(MAX_TOKEN_LEN + 1),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert!(matches!(outcome, HelloGateOutcome::RejectInvalidHello(_)));
    }

    #[test]
    fn rejects_non_hello_frame_before_handshake() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::CreateGame { deck: empty_deck() },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::SubscribeLobby,
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::Ping { timestamp: 1 },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);
    }

    #[test]
    fn ignores_redundant_hello_after_accept() {
        let outcome = classify_hello_gate(
            true,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::IgnoreRedundantHello);
    }

    #[test]
    fn passes_through_regular_frames_after_handshake() {
        let outcome = classify_hello_gate(
            true,
            &ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(outcome, HelloGateOutcome::PassThrough);
    }

    #[test]
    fn build_commit_allows_matching() {
        assert_eq!(
            check_build_commit("abc1234", "abc1234"),
            BuildCommitCheck::Allow
        );
    }

    #[test]
    fn build_commit_allows_when_either_side_is_empty() {
        // Restored sessions / legacy clients are treated as unknown.
        assert_eq!(check_build_commit("", "abc1234"), BuildCommitCheck::Allow);
        assert_eq!(check_build_commit("abc1234", ""), BuildCommitCheck::Allow);
        assert_eq!(check_build_commit("", ""), BuildCommitCheck::Allow);
    }

    #[test]
    fn build_commit_rejects_when_both_populated_and_different() {
        assert_eq!(
            check_build_commit("abc1234", "def5678"),
            BuildCommitCheck::Reject {
                host: "abc1234".into(),
                guest: "def5678".into(),
            }
        );
    }

    #[test]
    fn joining_current_game_is_rejected_by_helper() {
        let mut identity = empty_identity();
        identity.game_code = Some("GAME01".to_string());
        identity.player_id = Some(PlayerId(0));

        assert!(is_joining_current_game(&identity, "GAME01"));
        assert!(!is_joining_current_game(&identity, "GAME02"));

        let mut lobby_identity = empty_identity();
        lobby_identity.lobby_host_game = Some("GAME01".to_string());
        assert!(is_joining_current_game(&lobby_identity, "GAME01"));
        assert!(!is_joining_current_game(&lobby_identity, "GAME02"));
    }

    #[test]
    fn joining_without_active_game_is_allowed_by_helper() {
        let identity = empty_identity();
        assert!(!is_joining_current_game(&identity, "GAME01"));
    }

    // ------------------------------------------------------------------
    // GH #1254: MP wire-trust — client cannot forge another seat's
    // connection state via DraftAction::SetSeatConnected.
    // ------------------------------------------------------------------

    #[test]
    fn client_forbidden_draft_action_rejects_set_seat_connected() {
        // The forged payload: a malicious authenticated client passes
        // *another* seat's index. The handler currently discards the
        // token-resolved seat (`let _seat = ...` at draft_session.rs:247),
        // so the payload's `seat` would flow through unchecked without
        // this filter. Reject the variant outright — it's engine state
        // plumbing, not user intent.
        let action = draft_core::types::DraftAction::SetSeatConnected {
            seat: 3,
            connected: true,
        };
        let reason = client_forbidden_draft_action_reason(&action);
        assert!(
            reason.is_some(),
            "SetSeatConnected MUST be rejected when sent from a client"
        );
        let msg = reason.unwrap();
        assert!(
            msg.contains("server-internal"),
            "rejection reason should explain why: got {msg:?}"
        );
    }

    #[test]
    fn client_forbidden_draft_action_rejects_generate_pairings() {
        // Regression coverage: this rejection predates GH #1254 and must
        // continue to fire. GeneratePairings is server-internal because
        // match spawning now drives it after deck submission.
        let action = draft_core::types::DraftAction::GeneratePairings;
        let reason = client_forbidden_draft_action_reason(&action);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("server-internal"));
    }

    #[test]
    fn client_forbidden_draft_action_allows_legitimate_variants() {
        // Every variant that IS allowed from a client must return None.
        // If a new DraftAction variant lands and the helper's exhaustive
        // match doesn't handle it, this test fails at compile time on
        // the function — and the security-relevant decision is made
        // explicitly, not by default-allow.
        let allowed = [
            draft_core::types::DraftAction::StartDraft,
            draft_core::types::DraftAction::Pick {
                seat: 0,
                card_instance_id: "x".into(),
            },
            draft_core::types::DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id: "effect".into(),
                card_instance_ids: vec!["x".into(), "y".into()],
            },
            draft_core::types::DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
            },
            draft_core::types::DraftAction::ReportMatchResult {
                match_id: "m1".into(),
                winner_seat: Some(0),
            },
            draft_core::types::DraftAction::AdvanceRound,
            draft_core::types::DraftAction::ReplaceSeatWithBot {
                seat: 1,
                name: None,
            },
        ];
        for action in allowed {
            assert!(
                client_forbidden_draft_action_reason(&action).is_none(),
                "expected {action:?} to be allowed from client"
            );
        }
    }

    #[test]
    fn pick_timer_rearms_when_draft_starts() {
        use draft_core::types::DraftStatus;

        assert!(should_rearm_pick_timer(
            Some((DraftStatus::Lobby, 0, 0)),
            Some((DraftStatus::Drafting, 0, 0)),
        ));
    }

    #[test]
    fn pick_timer_rearms_when_pick_window_advances() {
        use draft_core::types::DraftStatus;

        assert!(should_rearm_pick_timer(
            Some((DraftStatus::Drafting, 0, 0)),
            Some((DraftStatus::Drafting, 0, 1)),
        ));
        assert!(should_rearm_pick_timer(
            Some((DraftStatus::Drafting, 0, 13)),
            Some((DraftStatus::Drafting, 1, 0)),
        ));
    }

    #[test]
    fn pick_timer_does_not_rearm_for_partial_pick_or_non_drafting_status() {
        use draft_core::types::DraftStatus;

        assert!(!should_rearm_pick_timer(
            Some((DraftStatus::Drafting, 0, 0)),
            Some((DraftStatus::Drafting, 0, 0)),
        ));
        assert!(!should_rearm_pick_timer(
            Some((DraftStatus::Drafting, 2, 13)),
            Some((DraftStatus::Deckbuilding, 2, 13)),
        ));
    }
}

// Regression test for https://github.com/phase-rs/phase/issues/4548:
// `broadcast_player_slots` must be callable without holding either the
// `state` or `connections` lock — both are re-acquired internally.
// The fix scopes every MutexGuard inside an explicit `{ }` block so the
// guard is unconditionally released before the `.await` inside
// `broadcast_player_slots`.
#[cfg(test)]
mod issue_4548_deadlock_tests {
    use super::*;
    use engine::game::deck_loading::PlayerDeckPayload;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn broadcast_player_slots_completes_when_no_locks_held() {
        let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));

        let game_code = {
            let mut mgr = state.lock().await;
            let (code, _token) = mgr.create_game(PlayerDeckPayload::default());
            code
        }; // state lock released here — matches the fixed handler path

        // If the old code were in effect (mgr held across this call), this
        // `.await` would block forever.  With the fix the lock is already
        // released, so it completes immediately.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            broadcast_player_slots(&state, &connections, &game_code),
        )
        .await
        .expect("broadcast_player_slots must not deadlock when called without holding locks");
    }

    #[tokio::test]
    async fn broadcast_player_slots_completes_while_lobby_lock_held() {
        // Regression: the old code kept `lob_guard` alive past the broadcast
        // call.  `broadcast_player_slots` does not acquire lobby, so holding
        // the lobby lock while calling it must not deadlock.
        let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        let lobby: SharedLobby = Arc::new(Mutex::new(Broker::new()));

        let game_code = {
            let mut mgr = state.lock().await;
            let (code, _token) = mgr.create_game(PlayerDeckPayload::default());
            code
        };

        // Deliberately hold the lobby lock — should not deadlock.
        let _lob_guard = lobby.lock().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            broadcast_player_slots(&state, &connections, &game_code),
        )
        .await
        .expect("broadcast_player_slots must not deadlock when lobby lock is held by caller");
    }

    /// Handler-path regression: drives `create_and_connect_multiplayer_session`,
    /// the exact function the `CreateGameWithSettings` handler uses for Phases 1–2.
    ///
    /// If that function were to hold the state or connections guard past its
    /// return boundary (the old deadlock pattern), the `broadcast_player_slots`
    /// call below would block waiting to re-acquire the same mutex and the
    /// two-second timeout would fire, failing this test.
    ///
    /// The two earlier tests above verify `broadcast_player_slots` itself; this
    /// test verifies the handler's lock-release contract by sharing the
    /// production code path.
    #[tokio::test]
    async fn create_and_connect_multiplayer_session_releases_locks_before_broadcast() {
        let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        let game_db = {
            let file = NamedTempFile::new().unwrap();
            Arc::new(
                persistence::GameDb::open(file.path(), persistence::SessionRetention::Multiplayer)
                    .unwrap(),
            )
        };
        let (tx, _rx) = mpsc::unbounded_channel::<ServerMessage>();

        let (game_code, _token, _count, _full_key) = create_and_connect_multiplayer_session(
            &state,
            &connections,
            &game_db,
            MultiplayerSessionRequest {
                resolved: PlayerDeckPayload::default(),
                display_name: "Alice".to_string(),
                timer_seconds: None,
                pc: 2,
                match_config: Default::default(),
                format_config: None,
                start_when_full: false,
                ranked: false,
                ai_requests: vec![],
                public: false,
                password: None,
                host_tx: tx,
            },
        )
        .await
        .expect("test session must be created");

        // Both state and connections locks must be free at this point.
        // A regression that holds either guard across the helper's return
        // causes this call to deadlock → timeout fires → test fails.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            broadcast_player_slots(&state, &connections, &game_code),
        )
        .await
        .expect(
            "create_and_connect_multiplayer_session must release state+connections before returning",
        );
    }
}

#[cfg(test)]
mod admin_auth_tests {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;
    use lobby_broker::Broker;
    use server_core::draft_session::DraftSessionManager;
    use server_core::session::SessionManager;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    use url::Url;

    use super::{
        admin_request_authorized, draft_pools, mount_admin_routes, persistence, tokens_match,
        AppState, ServerMode,
    };

    const TOKEN: &str = "s3cr3t-admin-token";

    fn test_app_state(temp_dir: &tempfile::TempDir) -> AppState {
        let game_db_path = temp_dir.path().join("games.db");
        let game_db = Arc::new(
            persistence::GameDb::open(&game_db_path, persistence::SessionRetention::Multiplayer)
                .expect("game db"),
        );
        AppState {
            sessions: Arc::new(Mutex::new(SessionManager::new())),
            draft_sessions: Arc::new(Mutex::new(DraftSessionManager::new())),
            draft_pools: Arc::new(draft_pools::DraftPools::default()),
            connections: Arc::new(Mutex::new(std::collections::HashMap::new())),
            db: Arc::new(engine::database::CardDatabase::default()),
            lobby: Arc::new(Mutex::new(Broker::new())),
            lobby_subscribers: Arc::new(Mutex::new(Vec::new())),
            player_count: Arc::new(AtomicU32::new(0)),
            game_db,
            draft_spectators: Arc::new(Mutex::new(std::collections::HashMap::new())),
            game_spectators: Arc::new(Mutex::new(std::collections::HashMap::new())),
            mode: ServerMode::Full,
            public_url: None,
            allowed_origin: None,
        }
    }

    async fn spawn_admin_http_test(
        admin_token: Option<&str>,
    ) -> (String, tokio::task::JoinHandle<()>, tempfile::TempDir) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        // Mirror production: establish Router<AppState> before mounting admin routes.
        let mut app = Router::new().route("/ws", get(super::ws_handler));
        if let Some(token) = admin_token.filter(|t| !t.is_empty()) {
            app = mount_admin_routes(app, token);
        }
        let app = app.with_state(app_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        (format!("http://{addr}"), handle, temp_dir)
    }

    async fn get_admin_drafts(base_url: &str, auth: Option<&str>) -> StatusCode {
        let url = Url::parse(&format!("{base_url}/admin/drafts")).expect("url");
        let host = url.host_str().expect("host");
        let port = url.port().expect("port");
        let mut stream = tokio::net::TcpStream::connect((host, port))
            .await
            .expect("connect");
        let mut request = String::from("GET /admin/drafts HTTP/1.1\r\n");
        request.push_str(&format!("Host: {host}\r\n"));
        if let Some(value) = auth {
            request.push_str(&format!("Authorization: {value}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.expect("read");
        let response = std::str::from_utf8(&buf[..n]).expect("utf8");
        let status_code = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .expect("status line");
        StatusCode::from_u16(status_code).expect("status code")
    }

    #[test]
    fn tokens_match_is_exact() {
        assert!(tokens_match(b"abc", b"abc"));
        assert!(!tokens_match(b"abc", b"abd"));
        assert!(!tokens_match(b"abc", b"ab"));
        assert!(!tokens_match(b"", b"x"));
        assert!(tokens_match(b"", b""));
    }

    #[test]
    fn authorized_only_with_matching_bearer_token() {
        let ok = format!("Bearer {TOKEN}");
        assert!(admin_request_authorized(Some(&ok), TOKEN));
        let padded = format!("Bearer   {TOKEN}  ");
        assert!(admin_request_authorized(Some(&padded), TOKEN));
        assert!(admin_request_authorized(
            Some(&format!("bearer {TOKEN}")),
            TOKEN
        ));
        assert!(admin_request_authorized(
            Some(&format!("BEARER {TOKEN}")),
            TOKEN
        ));
    }

    #[test]
    fn rejects_missing_wrong_or_malformed_header() {
        assert!(!admin_request_authorized(None, TOKEN));
        assert!(!admin_request_authorized(Some(""), TOKEN));
        assert!(!admin_request_authorized(Some("Bearer wrong-token"), TOKEN));
        let basic = format!("Basic {TOKEN}");
        assert!(!admin_request_authorized(Some(&basic), TOKEN));
        assert!(!admin_request_authorized(Some(TOKEN), TOKEN));
    }

    #[tokio::test]
    async fn admin_routes_absent_without_token() {
        let (base_url, server, _temp) = spawn_admin_http_test(None).await;
        assert_eq!(
            get_admin_drafts(&base_url, None).await,
            StatusCode::NOT_FOUND
        );
        server.abort();
    }

    #[tokio::test]
    async fn admin_routes_reject_missing_bearer() {
        let (base_url, server, _temp) = spawn_admin_http_test(Some(TOKEN)).await;
        assert_eq!(
            get_admin_drafts(&base_url, None).await,
            StatusCode::UNAUTHORIZED
        );
        server.abort();
    }

    #[tokio::test]
    async fn admin_routes_reject_wrong_bearer() {
        let (base_url, server, _temp) = spawn_admin_http_test(Some(TOKEN)).await;
        assert_eq!(
            get_admin_drafts(&base_url, Some("Bearer wrong-token")).await,
            StatusCode::UNAUTHORIZED
        );
        server.abort();
    }

    #[tokio::test]
    async fn admin_routes_accept_valid_bearer() {
        let (base_url, server, _temp) = spawn_admin_http_test(Some(TOKEN)).await;
        assert_eq!(
            get_admin_drafts(&base_url, Some(&format!("Bearer {TOKEN}"))).await,
            StatusCode::OK
        );
        server.abort();
    }
}

#[cfg(test)]
mod p2p_backup_delete_tests {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::Router;
    use lobby_broker::Broker;
    use server_core::draft_session::DraftSessionManager;
    use server_core::session::SessionManager;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    use url::Url;

    use super::{admin, draft_pools, persistence, AppState, ServerMode};

    const DRAFT_CODE: &str = "BACK01";
    const HOST_PEER: &str = "peer-host-owner";
    const OTHER_PEER: &str = "peer-not-owner";
    const SNAPSHOT: &str = r#"{"status":"Drafting"}"#;

    fn test_app_state(temp_dir: &tempfile::TempDir) -> AppState {
        let game_db_path = temp_dir.path().join("games.db");
        let game_db = Arc::new(
            persistence::GameDb::open(&game_db_path, persistence::SessionRetention::Multiplayer)
                .expect("game db"),
        );
        AppState {
            sessions: Arc::new(Mutex::new(SessionManager::new())),
            draft_sessions: Arc::new(Mutex::new(DraftSessionManager::new())),
            draft_pools: Arc::new(draft_pools::DraftPools::default()),
            connections: Arc::new(Mutex::new(std::collections::HashMap::new())),
            db: Arc::new(engine::database::CardDatabase::default()),
            lobby: Arc::new(Mutex::new(Broker::new())),
            lobby_subscribers: Arc::new(Mutex::new(Vec::new())),
            player_count: Arc::new(AtomicU32::new(0)),
            game_db,
            draft_spectators: Arc::new(Mutex::new(std::collections::HashMap::new())),
            game_spectators: Arc::new(Mutex::new(std::collections::HashMap::new())),
            mode: ServerMode::Full,
            public_url: None,
            allowed_origin: None,
        }
    }

    async fn spawn_p2p_backup_http_test(
        app_state: AppState,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/p2p-draft-backup", post(admin::p2p_backup_store))
            .route(
                "/p2p-draft-backup/{code}",
                get(admin::p2p_backup_get).delete(admin::p2p_backup_delete),
            )
            .with_state(app_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        (format!("http://{addr}"), handle)
    }

    async fn request_status(base_url: &str, method: &str, path: &str) -> StatusCode {
        let url = Url::parse(&format!("{base_url}{path}")).expect("url");
        let host = url.host_str().expect("host");
        let port = url.port().expect("port");
        let mut stream = tokio::net::TcpStream::connect((host, port))
            .await
            .expect("connect");
        let mut request = format!("{method} {path} HTTP/1.1\r\n");
        request.push_str(&format!("Host: {host}\r\n"));
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.expect("read");
        let response = std::str::from_utf8(&buf[..n]).expect("utf8");
        let status_code = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .expect("status line");
        StatusCode::from_u16(status_code).expect("status code")
    }

    fn seed_backup(app_state: &AppState) {
        app_state
            .game_db
            .save_p2p_backup(DRAFT_CODE, HOST_PEER, SNAPSHOT)
            .expect("seed backup");
    }

    #[tokio::test]
    async fn get_rejects_missing_host_peer_id() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(&base_url, "GET", &format!("/p2p-draft-backup/{DRAFT_CODE}")).await,
            StatusCode::BAD_REQUEST,
        );
        server.abort();
    }

    #[tokio::test]
    async fn get_rejects_mismatched_host_peer_id() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(
                &base_url,
                "GET",
                &format!("/p2p-draft-backup/{DRAFT_CODE}?host_peer_id={OTHER_PEER}")
            )
            .await,
            StatusCode::NOT_FOUND,
        );
        server.abort();
    }

    #[tokio::test]
    async fn get_accepts_matching_host_peer_id() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(
                &base_url,
                "GET",
                &format!("/p2p-draft-backup/{DRAFT_CODE}?host_peer_id={HOST_PEER}")
            )
            .await,
            StatusCode::OK,
        );
        server.abort();
    }

    #[tokio::test]
    async fn delete_rejects_missing_host_peer_id_and_preserves_row() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let game_db = Arc::clone(&app_state.game_db);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(
                &base_url,
                "DELETE",
                &format!("/p2p-draft-backup/{DRAFT_CODE}")
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
        assert!(
            game_db.load_p2p_backup(DRAFT_CODE).expect("load").is_some(),
            "backup must survive DELETE without host_peer_id"
        );
        server.abort();
    }

    #[tokio::test]
    async fn delete_rejects_mismatched_host_peer_id_and_preserves_row() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let game_db = Arc::clone(&app_state.game_db);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(
                &base_url,
                "DELETE",
                &format!("/p2p-draft-backup/{DRAFT_CODE}?host_peer_id={OTHER_PEER}"),
            )
            .await,
            StatusCode::FORBIDDEN,
        );
        assert!(
            game_db.load_p2p_backup(DRAFT_CODE).expect("load").is_some(),
            "backup must survive DELETE with wrong host_peer_id"
        );
        server.abort();
    }

    #[tokio::test]
    async fn delete_accepts_matching_host_peer_id() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        seed_backup(&app_state);
        let game_db = Arc::clone(&app_state.game_db);
        let (base_url, server) = spawn_p2p_backup_http_test(app_state).await;

        assert_eq!(
            request_status(
                &base_url,
                "DELETE",
                &format!("/p2p-draft-backup/{DRAFT_CODE}?host_peer_id={HOST_PEER}"),
            )
            .await,
            StatusCode::OK,
        );
        assert!(
            game_db.load_p2p_backup(DRAFT_CODE).expect("load").is_none(),
            "backup must be removed after authorized DELETE"
        );
        server.abort();
    }
}
