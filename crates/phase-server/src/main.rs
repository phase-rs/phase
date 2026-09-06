mod admin;
mod data_bootstrap;
mod draft_pools;
mod logging;
mod metrics;
mod persistence;
mod wire;

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
use axum::{Json, Router};
use clap::Parser;
use engine::ai_support::{
    activation_block_reasons_for_viewer as engine_activation_block_reasons_for_viewer,
    auto_pass_recommended_for_viewer as engine_auto_pass_for_viewer,
    end_continuous_effect_offers as engine_end_continuous_effect_offers,
    legal_actions_full as engine_legal_actions_full,
    mana_payment_shortcut_actions as engine_mana_payment_shortcut_actions,
};
use engine::database::CardDatabase;
use engine::game::derived_views::derive_filtered_views;
use engine::game::interaction::{derive_viewer_interaction, object_action_payloads};
use engine::game::validate_name_deck_for_format_full;
use engine::types::action_rejection::{ActionRejection, ActionRejectionCode};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, TrustedGameStateEnvelope};
use engine::types::interaction::{InteractionPreviewRequest, InteractionSubmission};
use engine::types::player::PlayerId;
use engine::types::GameLogEntry;
use http::{HeaderMap, HeaderValue};
use lobby_broker::{
    check_build_commit, conn_holds_reservation, validate_announcement, Broker, BrokerEnv,
    BuildCommitCheck, ConnState, Outbound, RawAnnouncement, ServerAnnouncement, ServerInfoDocument,
    DIRECTORY_VERSION, INFO_PATH, MAX_SERVER_NAME_LEN, NOT_OWNED_RESERVATION,
};
use rand::{Rng, TryRngCore};
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx};
use server_core::ai_seats_wire_guard::{guard_create_ai_seats, MAX_FULL_GAME_PLAYER_COUNT};
use server_core::client_hello_guard::guard_client_hello;
use server_core::client_message_wire_guard::{
    guard_broker_projection_inbound, guard_client_message_before_dispatch, wire_rejection_message,
};
use server_core::draft_action_payload_guard::guard_draft_action_payload;
use server_core::draft_session::{
    draft_seats_needing_auto_pick, DraftMatchPlayer, DraftMatchSpawn, DraftSessionManager,
};
use server_core::draft_wire_guard::{
    guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
    guard_reconnect_draft,
};
use server_core::emote_guard::guard_emote;
use server_core::game_action_payload_guard::guard_game_action_payload;
use server_core::game_reconnect_guard::guard_game_reconnect;
use server_core::game_state_snapshot_wire_guard::{
    guard_game_state_for_broadcast, guard_state_snapshot_broadcast, StateSnapshotParts,
};
use server_core::interaction_payload_guard::guard_interaction_submission_payload;
use server_core::legacy_deck_guard::guard_legacy_deck;
use server_core::legacy_join_guard::guard_legacy_join_game;
use server_core::lobby::RegisterGameRequest;
use server_core::lobby_subscriber_wire_guard::guard_lobby_subscriber_capacity;
use server_core::protocol::{
    build_commit, resolve_draft_source_intent, ClientMessage, RankedPlayerResult, ServerMessage,
    ServerMode, WireFormat, LOBBY_MIN_SUPPORTED_PROTOCOL, LOBBY_PROTOCOL_VERSION,
    MIN_SUPPORTED_LOBBY_PROTOCOL, MIN_SUPPORTED_PROTOCOL, PROTOCOL_VERSION,
};
use server_core::resolve_deck;
use server_core::seat_mutation_wire_guard::guard_seat_mutation;
use server_core::session::{
    ActionResult, FullRuntime, GameSession, HostingMode, PreviewRefusal, RevisionedActionResult,
    SessionActionError, SessionManager,
};
use server_core::spectator_wire_guard::{
    guard_draft_spectator_capacity, guard_game_spectator_capacity, guard_spectate_draft,
    guard_spectator_join,
};
use server_core::takeback::RewindOption;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot, Mutex};
use tower_http::compression::CompressionLayer;
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
    GameSession::from_persisted(persisted, &db)
}

/// Admit a persisted draft row only when its SQLite key names the enclosed
/// session, then prepare its post-admission startup data.
///
/// The persisted wrapper and core session validate each other in `server-core`,
/// but the database row key is a separate authority used by startup lookups and
/// lobby registration. Validate it before admission so a corrupt row cannot
/// leave the manager indexed under one code and the caller looking up another.
fn restore_persisted_draft_session(
    row_draft_code: &str,
    persisted: server_core::persist::PersistedDraftSession,
    drafts: &mut DraftSessionManager,
) -> Result<(Option<RegisterGameRequest>, Option<u32>), String> {
    if persisted.draft_code != row_draft_code {
        return Err("persisted draft code does not match its database row".to_string());
    }
    drafts.restore_persisted_session(persisted)?;
    let restored = drafts
        .sessions
        .get(row_draft_code)
        .ok_or_else(|| "admitted persisted draft session is missing".to_string())?;
    let restored_snapshot = restored.to_persisted();
    Ok((
        server_core::persist::restored_draft_lobby_register_request(&restored_snapshot),
        restored.timer_remaining_ms,
    ))
}

/// The startup restore owner keeps a session private until this handoff has
/// either durably fenced its one explicit automation resume or terminalized
/// it. Only [`RestoredFullStartup::Active`] may be exposed to reconnect,
/// lobby, or WebSocket code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoredFullStartup {
    Active,
    Terminal,
}

/// Re-attaches one Full snapshot to this process and commits its optional
/// startup-only stack-automation resume before callers may publish it.
///
/// `SessionManager::restore_session` deliberately happens before the resume:
/// it re-stamps process-owned hosting policy and revokes a persisted debug
/// capability before the engine can make another transition. The session stays
/// private behind the manager lock throughout. A changed non-terminal resume
/// must win the database revision fence synchronously; an ended game uses the
/// same terminal transaction as a live Full game. Only an active result keeps
/// the private insertion; failures leave the retained row for recovery and a
/// terminal outcome leaves only its durable delivery artifact.
fn finish_restored_full_startup(
    manager: &mut SessionManager,
    game_db: &SharedGameDb,
    snapshot: &server_core::FullPersistSnapshot,
    session: GameSession,
) -> Result<RestoredFullStartup, String> {
    let game_code = session.game_code.clone();
    if game_code != snapshot.key.game_code {
        return Err(
            "restored Full session game code does not match its persistence key".to_string(),
        );
    }
    manager.restore_session(session);

    let completion = (|| {
        let session = manager
            .sessions
            .get_mut(&game_code)
            .expect("restored session is private to this startup handoff");
        session.full_runtime = Some(FullRuntime {
            key: snapshot.key.clone(),
            activation_epoch: snapshot.activation_epoch,
        });

        let resumed = session.resume_restored_stack_automation();
        if let engine::types::game_state::WaitingFor::GameOver { winner } =
            &session.state.waiting_for
        {
            let winner = *winner;
            let ranked_result = ranked_duel_players(session)
                .and_then(|players| ranked_result_for_duel(game_db, &game_code, &players, winner));
            let artifact =
                terminal_artifact(session, winner, "Game ended".to_string(), ranked_result)?;
            game_db
                .prepare_full_terminal(&artifact)
                .map_err(|error| format!("failed to terminalize restored Full session: {error}"))?;
            return Ok(RestoredFullStartup::Terminal);
        }

        if resumed.state_revision.is_some() {
            let post_resume = session
                .full_persist_snapshot()
                .expect("restored Full session remains runtime-bound");
            match game_db
                .save_full_session(&post_resume)
                .map_err(|error| format!("failed to persist restored Full session: {error}"))?
            {
                server_core::FullPersistDisposition::Applied => {}
                disposition => {
                    return Err(format!(
                        "restored Full session persistence was superseded: {disposition:?}"
                    ));
                }
            }
        }

        Ok(RestoredFullStartup::Active)
    })();

    if completion.as_ref() != Ok(&RestoredFullStartup::Active) {
        manager.remove_game(&game_code);
    }
    completion
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
    // CR 118.3 + CR 117.1: derived per recipient from the same raw pre-filter
    // state binding its sibling `spell_costs` came from. No `if is_actor`
    // wrapper: the entry point self-gates on `turn_control::is_authorized_submitter`,
    // which is the SAME function `server_core::is_acting` delegates to.
    let activation_block_reasons =
        engine_activation_block_reasons_for_viewer(&session.state, player);
    debug_assert!(
        activation_block_reasons.is_empty() || is_actor,
        "a non-empty CR 118.3 read-out implies an authorized viewer"
    );

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
        activation_block_reasons,
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
    full_key: &server_core::FullSessionKey,
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
    // CR 118.3 + CR 117.1: same raw state binding the sibling `spell_costs`
    // came from (both destructured from this `ActionResult`).
    let activation_block_reasons = engine_activation_block_reasons_for_viewer(raw_state, player);
    debug_assert!(
        activation_block_reasons.is_empty() || is_actor,
        "a non-empty CR 118.3 read-out implies an authorized viewer"
    );

    Ok(ServerMessage::StateUpdate {
        full_key: Some(full_key.clone()),
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
        activation_block_reasons,
        derived,
        viewer_interaction,
        rewind_targets,
    })
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
        // CR 117.1: a spectator is a non-seat viewer with no action authority,
        // so the read-out is empty by the same existing design as the two
        // siblings above.
        activation_block_reasons: HashMap::new(),
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
    full_key: &server_core::FullSessionKey,
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
        full_key: Some(full_key.clone()),
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
        // CR 117.1: spectators have no action authority — empty by the same
        // existing design as the two siblings above.
        activation_block_reasons: HashMap::new(),
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

/// Server-wide limits to prevent resource exhaustion and abuse. These are the
/// defaults; an operator running many small replicas behind a load balancer can
/// lower them per process with `--max-connections` / `--max-games`.
const DEFAULT_MAX_CONNECTIONS: u32 = 200;
const DEFAULT_MAX_GAMES: usize = 100;
/// Admission limits for this process, resolved once from the CLI at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    max_connections: u32,
    max_games: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_games: DEFAULT_MAX_GAMES,
        }
    }
}

/// Ambient per-process context every admission decision needs: what the limits
/// are, and where a refusal gets counted. Threaded through the socket handlers
/// rather than held in a global so tests can drive a handler at a small cap
/// without perturbing the rest of the suite.
#[derive(Clone, Default)]
struct ServerContext {
    limits: Limits,
    /// Ordinal of this replica within its StatefulSet, from `--replica-ordinal`.
    /// Carried only to be exposed as `phase_replica_ordinal`: an autoscaler
    /// needs it to name the highest replica still holding players, and PromQL
    /// has no way to turn a label back into a number.
    replica_ordinal: Option<u32>,
    metrics: Arc<metrics::ServerMetrics>,
}

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

    /// Maximum concurrent WebSocket connections before upgrades are refused
    /// with 503. Lower it when running several small replicas behind a load
    /// balancer so one process cannot absorb the whole fleet's traffic.
    #[arg(long, default_value_t = DEFAULT_MAX_CONNECTIONS, env = "PHASE_MAX_CONNECTIONS")]
    max_connections: u32,

    /// Maximum concurrent game sessions before CreateGame is refused.
    #[arg(long, default_value_t = DEFAULT_MAX_GAMES, env = "PHASE_MAX_GAMES")]
    max_games: usize,

    /// Serve Prometheus metrics on this port, on a second listener bound to
    /// `--bind`. Unset (the default) means no metrics listener at all: the
    /// gauges describe capacity and occupancy, which belongs to the operator
    /// rather than to anyone who can reach the public port.
    #[arg(long, env = "PHASE_METRICS_PORT")]
    metrics_port: Option<u16>,

    /// This process's ordinal within a replica set, exposed as the
    /// `phase_replica_ordinal` metric. A scale-in policy needs it to identify
    /// the highest-numbered replica that still has players on it.
    #[arg(long, env = "PHASE_REPLICA_ORDINAL")]
    replica_ordinal: Option<u32>,

    /// Public base URL to advertise to clients for sharing join codes (e.g.
    /// `https://play.example.com` when running behind a TLS reverse proxy or
    /// tunnel). Clients surface `<code>@<host>` so friends can join without the
    /// host reading server logs. When the `ngrok` feature is built and
    /// `NGROK_AUTHTOKEN` is set, the live tunnel URL is used when this is unset.
    #[arg(long, env = "PUBLIC_URL")]
    public_url: Option<String>,

    /// Directory endpoint to announce this server to, e.g.
    /// `https://lobby.phase-rs.dev/servers/announce`. Requires --public-url,
    /// which must be an `https://` URL — the directory lists `wss://`
    /// addresses only. Off by default.
    #[arg(long, env = "PHASE_ANNOUNCE_TO", requires = "public_url")]
    announce_to: Option<Url>,
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
    /// Tournament codes this socket created, mirroring
    /// `ConnState::organized_tournaments`.
    ///
    /// Threaded rather than defaulted on every broker call: the broker appends
    /// to its `ConnState` view, and without a home here that append would be
    /// discarded the instant the view drops — a field that looks populated in
    /// the core and is permanently empty in the native shell. Explicitly NOT
    /// an authority (the `organizer_token` is), and deliberately not cleared
    /// on disconnect.
    ///
    /// Growth is bounded by the core, not here: the broker is the only writer
    /// (`absorb_conn_state` copies back whatever it produced), and it appends
    /// through `push_conn_tournament`, which holds the list at
    /// `lobby_broker::broker::MAX_CONN_TOURNAMENT_ENTRIES`. The shell must not
    /// add a second, independently-drifting bound.
    lobby_organized_tournaments: Vec<String>,
    /// Tournament codes this socket joined, mirroring
    /// `ConnState::joined_tournaments`. Same threading rationale as
    /// [`SocketIdentity::lobby_organized_tournaments`].
    ///
    /// Codes only. The core deliberately does not retain the `player_token`
    /// here, precisely because this per-socket identity is where such a
    /// secret would be held long past the operation that minted it.
    lobby_joined_tournaments: Vec<String>,
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
    wire_format: Option<WireFormat>,
}

const MAX_ADVERTISED_WIRE_FORMATS: usize = 8;

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
                wire_formats,
            },
        ) => {
            // The `server` field on RejectProtocol surfaces the version this
            // server speaks on whichever surface it gated, so the error message
            // tells the client what to upgrade (or downgrade) to.
            if let Some((client, server)) =
                acceptance.reject(*protocol_version, *lobby_protocol_version)
            {
                HelloGateOutcome::RejectProtocol { client, server }
            } else if wire_formats.len() > MAX_ADVERTISED_WIRE_FORMATS {
                HelloGateOutcome::RejectInvalidHello(
                    "ClientHello advertises too many wire formats".to_string(),
                )
            } else if let Err(reason) = guard_client_hello(client_version, build_commit) {
                HelloGateOutcome::RejectInvalidHello(reason)
            } else {
                HelloGateOutcome::Accept(ClientHelloInfo {
                    client_version: client_version.clone(),
                    build_commit: build_commit.clone(),
                    wire_format: wire_formats
                        .contains(&WireFormat::GzipEnvelopeV1)
                        .then_some(WireFormat::GzipEnvelopeV1),
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
        | ClientMessage::Interaction { .. }
        | ClientMessage::PreviewInteraction { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::ExportAuthoritativeState
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

        // Tournament messages — allowed in BOTH modes, unlike the
        // lobby-only-exclusive pair below. A tournament is an event record
        // the broker owns outright: it runs no server-side game session (so
        // there is nothing for Full mode to be missing), and it is not a
        // P2P-host bookkeeping message that only makes sense when the server
        // is not running the game (so there is nothing for Full mode to
        // refuse). Both shells hold the same `TournamentManager`, so gating
        // by mode here would refuse a capability the server demonstrably has.
        ClientMessage::CreateTournament { .. }
        | ClientMessage::JoinTournament { .. }
        | ClientMessage::GetTournament { .. }
        | ClientMessage::StartTournamentRound { .. }
        | ClientMessage::ReportMatchResult { .. }
        | ClientMessage::DropFromTournament { .. }
        | ClientMessage::EndTournament { .. } => None,

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

/// Whether the engine procedure permits this pod's requested seat count.
///
/// The socket preflight deliberately delegates the full range and
/// tournament-pairing bracket policy to `DraftProcedure`; this boundary only
/// reports that policy before resolving pool data.
fn draft_pod_size_is_allowed(
    kind: draft_core::types::DraftKind,
    tournament_format: draft_core::types::TournamentFormat,
    pod_size: u8,
) -> bool {
    kind.procedure()
        .allows_pod_size(tournament_format, pod_size)
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
            organized_tournaments: self.lobby_organized_tournaments.clone(),
            joined_tournaments: self.lobby_joined_tournaments.clone(),
        }
    }

    /// Write the broker's [`ConnState`] mutations back into the shell identity
    /// after a broker call. `client_hello` is shell-owned (set by the handshake
    /// gate, never by the broker in the native shell) so it is not copied back.
    fn absorb_conn_state(&mut self, conn: ConnState) {
        self.lobby_subscribed = conn.subscribed;
        self.lobby_host_game = conn.host_game;
        self.lobby_reservations = conn.reservations;
        self.lobby_organized_tournaments = conn.organized_tournaments;
        self.lobby_joined_tournaments = conn.joined_tournaments;
    }

    /// The Full-game seat claimed by this socket. A complete triple is required
    /// before the socket can exercise a seat-scoped capability.
    fn full_seat(&self) -> Option<(&str, PlayerId, &str)> {
        Some((
            self.game_code.as_deref()?,
            self.player_id?,
            self.player_token.as_deref()?,
        ))
    }

    /// The draft seat claimed by this socket. A complete triple is required
    /// before the socket can exercise a draft seat-scoped capability.
    fn draft_seat(&self) -> Option<(&str, usize, &str)> {
        Some((
            self.draft_code.as_deref()?,
            self.draft_seat?,
            self.draft_token.as_deref()?,
        ))
    }
}

/// Full-mode authority required before a message reaches its handler.
///
/// This is deliberately exhaustive: adding a protocol variant must make its
/// attachment policy explicit rather than accidentally inheriting access from
/// a neighbouring handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullSocketAuthority {
    /// Does not use a Full-game seat (terminal, lobby, draft, or spectator).
    Independent,
    /// Must originate from a socket that has not attached to a Full-game seat.
    FreshSocket,
    /// Requires the identity's seat to still own its sender-map entry.
    CurrentSeat,
    /// A fresh socket may reconnect; an attached socket may only renew its
    /// exact current seat.
    Reconnect,
}

fn full_socket_authority(message: &ClientMessage) -> FullSocketAuthority {
    match message {
        ClientMessage::ClientHello { .. }
        | ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Ping { .. }
        | ClientMessage::BootstrapTerminalDelivery { .. }
        | ClientMessage::ReadTerminalResult { .. }
        | ClientMessage::AckTerminalDelivery { .. }
        | ClientMessage::SpectatorJoin { .. }
        | ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. }
        | ClientMessage::UpdateLobbyMetadata { .. }
        | ClientMessage::UnregisterLobby { .. }
        // Tournaments use no Full-game seat at all: authority is the
        // `organizer_token`/`player_token` in the payload, checked by the
        // broker against the stored value, and a tournament entrant is not a
        // seat in a running game. `Independent` is what lets an organizer act
        // from a socket that is also mid-game, or from a fresh one after a
        // reconnect — the property the token model exists to provide.
        | ClientMessage::CreateTournament { .. }
        | ClientMessage::JoinTournament { .. }
        | ClientMessage::GetTournament { .. }
        | ClientMessage::StartTournamentRound { .. }
        | ClientMessage::ReportMatchResult { .. }
        | ClientMessage::DropFromTournament { .. }
        | ClientMessage::EndTournament { .. } => FullSocketAuthority::Independent,

        ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::CreateGameWithSettings { .. }
        | ClientMessage::JoinGameWithPassword { .. } => FullSocketAuthority::FreshSocket,

        // A plain lookup is read-only lobby discovery. Reserving a seat is
        // mutating, so it is fresh-socket-only like a join.
        ClientMessage::LookupJoinTarget { reserve, .. } => {
            if *reserve {
                FullSocketAuthority::FreshSocket
            } else {
                FullSocketAuthority::Independent
            }
        }

        ClientMessage::Action { .. }
        | ClientMessage::Interaction { .. }
        | ClientMessage::PreviewInteraction { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::ExportAuthoritativeState
        | ClientMessage::AbandonGame
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::Concede
        | ClientMessage::ConcedeMatch
        | ClientMessage::RequestTakeback(_)
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback
        | ClientMessage::Emote { .. } => FullSocketAuthority::CurrentSeat,

        ClientMessage::Reconnect { .. } => FullSocketAuthority::Reconnect,
    }
}

const FULL_SOCKET_AUTHORITY_REJECTION: &str =
    "This socket's game session identity is no longer current";
const FULL_SOCKET_FRESH_REJECTION: &str = "This socket is already attached to a game session";
const DRAFT_SOCKET_AUTHORITY_REJECTION: &str =
    "This socket's draft session identity is no longer current";
const DRAFT_SOCKET_FRESH_REJECTION: &str = "This socket is already attached to a draft session";

/// Draft creation and joining install a new seat identity. They therefore need
/// the same fresh-socket rule as full-game creation and joining: an attached
/// socket, including one whose sender has since been replaced, must not be
/// allowed to overwrite the identity its close handler will later clean up.
///
/// Reconnecting is deliberately excluded. A new websocket has no draft seat
/// identity and may reconnect, while an attached websocket is governed by the
/// exact-seat checks in [`reconnect_draft_seat`].
fn draft_socket_admission_rejection(
    message: &ClientMessage,
    identity: &SocketIdentity,
) -> Option<&'static str> {
    (matches!(
        message,
        ClientMessage::CreateDraftWithSettings { .. } | ClientMessage::JoinDraftWithPassword { .. }
    ) && identity.draft_seat().is_some())
    .then_some(DRAFT_SOCKET_FRESH_REJECTION)
}

/// Verifies a Full seat while the caller already owns the session-state lock.
///
/// This is the authority linearization point for every state mutation: the
/// token resolves against the exact `SessionManager` snapshot being mutated,
/// then the sender map is checked before that mutation begins. Callers release
/// both guards before persistence, socket I/O, or any fan-out.
async fn full_socket_is_current_while_state_locked(
    manager: &SessionManager,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> bool {
    let Some((game_code, player_id, player_token)) = identity.full_seat() else {
        return false;
    };
    let resolved_player = manager
        .sessions
        .get(game_code)
        .and_then(|session| session.player_for_token(player_token));
    if resolved_player != Some(player_id) {
        return false;
    }

    let conns = connections.lock().await;
    conns
        .get(game_code)
        .and_then(|players| players.get(&player_id))
        .is_some_and(|sender| sender.same_channel(tx))
}

/// A pre-dispatch rejection fast path. It improves stale-socket feedback, but
/// it is deliberately not the mutation authority: every attached-seat state
/// mutator rechecks through [`full_socket_is_current_while_state_locked`].
async fn full_socket_is_current_preflight(
    state: &SharedState,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> bool {
    let manager = state.lock().await;
    full_socket_is_current_while_state_locked(&manager, connections, identity, tx).await
}

/// Install a Full-seat sender while the caller owns the session-state lock.
/// The state -> connections nesting makes sender replacement and any related
/// session mutation one transaction.
async fn install_full_sender_while_state_locked(
    connections: &SharedConnections,
    game_code: &str,
    player_id: PlayerId,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let mut conns = connections.lock().await;
    conns
        .entry(game_code.to_string())
        .or_default()
        .insert(player_id, tx.clone());
}

/// Resolve and install Full-seat authority before mutating per-socket identity.
/// Token resolution comes from `SessionManager`, never a client-supplied seat.
async fn attach_full_seat(
    state: &SharedState,
    connections: &SharedConnections,
    identity: &mut SocketIdentity,
    game_code: String,
    player_token: String,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<PlayerId, String> {
    let mgr = state.lock().await;
    let player_id = mgr
        .sessions
        .get(&game_code)
        .and_then(|session| session.player_for_token(&player_token))
        .ok_or_else(|| "Game session identity is no longer valid".to_string())?;
    install_full_sender_while_state_locked(connections, &game_code, player_id, tx).await;
    drop(mgr);
    identity.set_session(game_code, player_id, player_token);
    Ok(player_id)
}

/// Applies the central Full-mode socket authority policy before dispatch.
async fn full_socket_authority_rejection(
    message: &ClientMessage,
    state: &SharedState,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Option<&'static str> {
    match full_socket_authority(message) {
        FullSocketAuthority::Independent => None,
        FullSocketAuthority::FreshSocket => identity
            .full_seat()
            .is_some()
            .then_some(FULL_SOCKET_FRESH_REJECTION),
        FullSocketAuthority::CurrentSeat => {
            if identity.full_seat().is_some()
                && !full_socket_is_current_preflight(state, connections, identity, tx).await
            {
                Some(FULL_SOCKET_AUTHORITY_REJECTION)
            } else {
                None
            }
        }
        FullSocketAuthority::Reconnect => {
            let (attached_game_code, attached_player, _) = identity.full_seat()?;
            let ClientMessage::Reconnect {
                game_code,
                player_token,
                ..
            } = message
            else {
                unreachable!("Reconnect authority only receives Reconnect messages");
            };
            let requested_player = {
                let mgr = state.lock().await;
                mgr.sessions
                    .get(game_code)
                    .and_then(|session| session.player_for_token(player_token))
            };
            (game_code != attached_game_code
                || requested_player != Some(attached_player)
                || !full_socket_is_current_preflight(state, connections, identity, tx).await)
                .then_some(FULL_SOCKET_AUTHORITY_REJECTION)
        }
    }
}

/// Disconnect a Full-game seat only when this socket still owns its sender-map
/// entry. A replacement socket therefore cannot be disconnected by the close
/// event from the socket it replaced.
async fn disconnect_full_seat_if_current(
    state: &SharedState,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let Some((game_code, player_id, player_token)) = identity.full_seat() else {
        return;
    };

    let (notify_senders, full_key) = {
        // The only nested acquisition in this path is state -> connections.
        // Both guards are released before the notification fan-out below.
        let mut mgr = state.lock().await;
        if mgr
            .sessions
            .get(game_code)
            .and_then(|session| session.player_for_token(player_token))
            != Some(player_id)
        {
            return;
        }
        let full_key = mgr
            .sessions
            .get(game_code)
            .and_then(|session| session.full_runtime.as_ref())
            .map(|runtime| runtime.key.clone());

        let mut conns = connections.lock().await;
        let Some(players) = conns.get_mut(game_code) else {
            return;
        };
        if !players
            .get(&player_id)
            .is_some_and(|sender| sender.same_channel(tx))
        {
            return;
        }

        players.remove(&player_id);
        mgr.handle_disconnect(game_code, player_id);
        (players.values().cloned().collect::<Vec<_>>(), full_key)
    };

    let message = ServerMessage::OpponentDisconnected {
        full_key,
        grace_seconds: 120,
        player: Some(player_id),
    };
    for sender in notify_senders {
        let _ = sender.send(message.clone());
    }
}

/// Verifies a draft seat while the caller already owns the draft-state lock.
///
/// This is the authority linearization point for draft mutations: the token
/// resolves against the exact [`DraftSessionManager`] snapshot being mutated,
/// then the draft sender-map entry must still belong to this socket. Both
/// guards are released before persistence, socket I/O, or fan-out.
async fn draft_socket_is_current_while_state_locked(
    manager: &DraftSessionManager,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> bool {
    let Some((draft_code, seat, player_token)) = identity.draft_seat() else {
        return false;
    };
    let resolved_seat = manager
        .sessions
        .get(draft_code)
        .and_then(|session| session.seat_for_token(player_token));
    if resolved_seat != Some(seat) {
        return false;
    }

    let conns = connections.lock().await;
    conns
        .get(draft_code)
        .and_then(|players| players.get(&PlayerId(seat as u8)))
        .is_some_and(|sender| sender.same_channel(tx))
}

/// A pre-dispatch authority check for reconnect attempts from an already
/// attached draft socket. The reconnect transaction below rechecks under the
/// draft-state lock before it replaces the sender.
async fn draft_socket_is_current_preflight(
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> bool {
    let manager = draft_state.lock().await;
    draft_socket_is_current_while_state_locked(&manager, connections, identity, tx).await
}

/// Install a draft-seat sender while the caller owns the draft-state lock.
/// The draft state -> connections nesting makes replacement and reconnect one
/// transaction, so later broadcasts and match launches observe the new sender.
async fn install_draft_sender_while_state_locked(
    connections: &SharedConnections,
    draft_code: &str,
    seat: usize,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let mut conns = connections.lock().await;
    conns
        .entry(draft_code.to_string())
        .or_default()
        .insert(PlayerId(seat as u8), tx.clone());
}

/// Reconnect a draft seat and install its sender before assigning socket
/// identity. The session operation and map replacement share draft-state ->
/// connections lock ordering, preventing a fan-out from observing a renewed
/// seat paired with its former socket.
async fn reconnect_draft_seat(
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    identity: &mut SocketIdentity,
    draft_code: String,
    player_token: String,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let mut manager = draft_state.lock().await;
    if let Some((attached_code, _attached_seat, attached_token)) = identity.draft_seat() {
        if attached_code != draft_code
            || attached_token != player_token
            || !draft_socket_is_current_while_state_locked(&manager, connections, identity, tx)
                .await
        {
            return Err(DRAFT_SOCKET_AUTHORITY_REJECTION.to_string());
        }
    }
    let seat = manager
        .sessions
        .get(&draft_code)
        .and_then(|session| session.seat_for_token(&player_token))
        .ok_or_else(|| "Invalid player token".to_string())?;
    let view = manager.handle_reconnect(&draft_code, &player_token)?;
    install_draft_sender_while_state_locked(connections, &draft_code, seat, tx).await;
    drop(manager);

    identity.draft_code = Some(draft_code);
    identity.draft_seat = Some(seat);
    identity.draft_token = Some(player_token);
    Ok(view)
}

#[derive(Debug)]
struct DraftMatchAttachment {
    match_start: ServerMessage,
    game_started: ServerMessage,
}

/// Attach an authenticated draft seat to its current spawned Full game.
///
/// The draft token remains the root capability. The Full token and generation
/// are derived from the current pairing and live game session, never accepted
/// from the client.
async fn attach_current_draft_match(
    draft_state: &SharedDraftState,
    state: &SharedState,
    connections: &SharedConnections,
    identity: &mut SocketIdentity,
    draft_code: &str,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<Option<DraftMatchAttachment>, String> {
    let (attached_draft, draft_seat, draft_token) = identity
        .draft_seat()
        .ok_or_else(|| "Socket is not attached to a draft seat".to_string())?;
    let draft_token = draft_token.to_string();
    if attached_draft != draft_code {
        return Err(DRAFT_SOCKET_AUTHORITY_REJECTION.to_string());
    }

    let draft_manager = draft_state.lock().await;
    if !draft_socket_is_current_while_state_locked(&draft_manager, connections, identity, tx).await
    {
        return Err(DRAFT_SOCKET_AUTHORITY_REJECTION.to_string());
    }
    let draft_session = draft_manager
        .sessions
        .get(draft_code)
        .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
    let Some((pairing, game_code, game_player)) = draft_session.active_match_for_seat(draft_seat)
    else {
        return Ok(None);
    };
    let opponent_index = match game_player {
        PlayerId(0) => 1,
        PlayerId(1) => 0,
        _ => return Err("Draft match player identity is inconsistent".to_string()),
    };
    let opponent_seat = usize::from(pairing.players[opponent_index].0);
    let match_id = pairing.match_id.clone();
    let round = pairing.round;
    let game_code = game_code.to_string();
    let opponent_name = draft_session
        .display_names
        .get(opponent_seat)
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("Player {opponent_seat}"));
    let previous_full = identity
        .full_seat()
        .map(|(code, player, _)| (code.to_string(), player));

    let (player_token, full_key, game_started, was_disconnected) = {
        let mut manager = state.lock().await;
        let player_index = usize::from(game_player.0);
        let (player_token, full_key, was_disconnected) = {
            let Some(session) = manager.sessions.get(&game_code) else {
                warn!(
                    draft = %draft_code,
                    game = %game_code,
                    "draft pairing references a game that is no longer resident"
                );
                return Ok(None);
            };
            let full_key = session
                .full_runtime
                .as_ref()
                .map(|runtime| runtime.key.clone())
                .ok_or_else(|| "Draft match Full identity is unavailable".to_string())?;
            if full_key.game_code != game_code {
                return Err("Draft match Full identity is inconsistent".to_string());
            }
            let player_token = session
                .player_tokens
                .get(player_index)
                .filter(|token| !token.is_empty())
                .cloned()
                .ok_or_else(|| "Draft match player identity is unavailable".to_string())?;
            if session.player_for_token(&player_token) != Some(game_player) {
                return Err("Draft match player identity is inconsistent".to_string());
            }
            (player_token, full_key, !session.connected[player_index])
        };

        manager.handle_reconnect(&game_code, &player_token)?;
        let session = manager
            .sessions
            .get(&game_code)
            .expect("draft match remains present after reconnect");
        let game_started = build_game_started_message(session, game_player, None, Vec::new());

        let mut conns = connections.lock().await;
        if let Some((previous_game, previous_player)) = previous_full {
            if previous_game != game_code || previous_player != game_player {
                if let Some(players) = conns.get_mut(&previous_game) {
                    if players
                        .get(&previous_player)
                        .is_some_and(|sender| sender.same_channel(tx))
                    {
                        players.remove(&previous_player);
                        manager.handle_disconnect(&previous_game, previous_player);
                    }
                }
            }
        }
        conns
            .entry(game_code.clone())
            .or_default()
            .insert(game_player, tx.clone());

        (player_token, full_key, game_started, was_disconnected)
    };
    drop(draft_manager);

    identity.set_session(game_code.clone(), game_player, player_token);

    if was_disconnected {
        let reconnect_senders = {
            let conns = connections.lock().await;
            conns
                .get(&game_code)
                .into_iter()
                .flat_map(|players| players.iter())
                .filter(|(player, _)| **player != game_player)
                .map(|(_, sender)| sender.clone())
                .collect::<Vec<_>>()
        };
        let message = ServerMessage::OpponentReconnected {
            full_key: Some(full_key.clone()),
            player: Some(game_player),
        };
        for sender in reconnect_senders {
            let _ = sender.send(message.clone());
        }
    }

    Ok(Some(DraftMatchAttachment {
        match_start: ServerMessage::DraftMatchStart {
            match_id,
            round,
            game_code,
            full_key,
            player_token: draft_token,
            your_player: game_player,
            opponent_name,
        },
        game_started,
    }))
}

/// Mark a draft seat disconnected only if this socket still owns its sender
/// entry. A superseded socket's close event must leave the replacement's seat
/// connected and must not remove its sender from broadcasts or match launches.
async fn disconnect_draft_seat_if_current(
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    identity: &SocketIdentity,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let Some((draft_code, seat, player_token)) = identity.draft_seat() else {
        return;
    };

    let mut manager = draft_state.lock().await;
    if manager
        .sessions
        .get(draft_code)
        .and_then(|session| session.seat_for_token(player_token))
        != Some(seat)
    {
        return;
    }

    let mut conns = connections.lock().await;
    let Some(players) = conns.get_mut(draft_code) else {
        return;
    };
    let player_id = PlayerId(seat as u8);
    if !players
        .get(&player_id)
        .is_some_and(|sender| sender.same_channel(tx))
    {
        return;
    }

    players.remove(&player_id);
    manager.handle_disconnect(draft_code, seat);
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

    let (_log_guard, game_log) = logging::init_logging(cli.log_dir.as_deref(), cli.log_json);
    let mode: Mode = if cli.lobby_only {
        ServerMode::LobbyOnly
    } else {
        ServerMode::Full
    };
    info!(?mode, "server mode selected");
    let server_context = ServerContext {
        limits: Limits {
            max_connections: cli.max_connections,
            max_games: cli.max_games,
        },
        replica_ordinal: cli.replica_ordinal,
        metrics: Arc::new(metrics::ServerMetrics::default()),
    };
    info!(
        max_connections = server_context.limits.max_connections,
        max_games = server_context.limits.max_games,
        replica_ordinal = ?server_context.replica_ordinal,
        "admission limits resolved"
    );
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
    let mut session_manager = if cli.single_user {
        SessionManager::single_user(Duration::from_secs(10 * 365 * 24 * 60 * 60))
    } else {
        SessionManager::new()
    };
    // Every session this manager creates or restores gets this cache
    // (`SessionManager`/`GameSession` re-stamp it, same lifecycle as
    // `hosting`) — see `server_core::game_log`.
    session_manager.game_log = Arc::clone(&game_log);
    let state: SharedState = Arc::new(Mutex::new(session_manager));
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
                        Ok(session) => match finish_restored_full_startup(
                            &mut mgr, &game_db, snapshot, session,
                        ) {
                            Ok(RestoredFullStartup::Terminal) => {
                                info!(game = %game_code, "terminalized restored Full session");
                            }
                            Ok(RestoredFullStartup::Active) => {
                                let (
                                    lobby_meta,
                                    is_started,
                                    reconnect_players,
                                    current_players,
                                    max_players,
                                    format_config,
                                    match_config,
                                ) = {
                                    let session = mgr
                                        .sessions
                                        .get(game_code)
                                        .expect("active startup handoff retains its session");
                                    let reconnect_players: Vec<PlayerId> = session
                                        .player_tokens
                                        .iter()
                                        .enumerate()
                                        .filter_map(|(index, token)| {
                                            let player = PlayerId(index as u8);
                                            (!token.is_empty()
                                                && !session.ai_seats.contains(&player))
                                            .then_some(player)
                                        })
                                        .collect();
                                    (
                                        session.lobby_meta.clone(),
                                        session.game_started,
                                        reconnect_players,
                                        session.current_player_count(),
                                        session.player_count as u32,
                                        session.state.format_config.clone(),
                                        session.state.match_config,
                                    )
                                };

                                // Register all non-AI human players as disconnected
                                // to start the 120s grace period from now. This is
                                // deliberately after the durable startup handoff:
                                // a failed resume is not reconnectable.
                                let default_grace = mgr.reconnect.grace_period;
                                for player in reconnect_players {
                                    mgr.reconnect.record_disconnect(
                                        game_code,
                                        player,
                                        default_grace,
                                    );
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
                                                current_players,
                                                max_players,
                                                format_config: Some(format_config),
                                                match_config,
                                                ..Default::default()
                                            },
                                            &SysEnv,
                                        );
                                    }
                                }

                                restored += 1;
                            }
                            Err(error) => {
                                warn!(game = %game_code, %error, "restored Full session remains private for recovery");
                            }
                        },
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
                        Ok(ps) => match restore_persisted_draft_session(draft_code, ps, &mut dsm) {
                            Ok((register_req, timer_ms)) => {
                                if let Some(req) = register_req {
                                    lob.register_game(draft_code, req, &SysEnv);
                                }
                                if let Some(ms) = timer_ms {
                                    info!(draft = %draft_code, remaining_ms = ms, "draft session has pending timer");
                                }
                                restored_drafts += 1;
                            }
                            Err(error) => {
                                warn!(draft = %draft_code, error = %error, "invalid persisted draft session, deleting");
                                let _ = game_db.delete_draft_session(draft_code);
                            }
                        },
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
                // Nothing else is held here: the branch below takes
                // `bg_game_spectators` at its end and the lobby-expiry branch
                // takes `bg_lobby` before `bg_state`, so the lobby lock this
                // call takes is nested under neither.
                delist_removed_sessions(&bg_lobby, &bg_lobby_subs, &removed).await;
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
                // `expired_lobby` is deliberately lobby-filtered: it drives the
                // Full-mode session/db cleanup below, and a tournament has no
                // server-run session to retire. But `reap_outbounds` now also
                // carries tournament lifecycle events, so reporting only the
                // lobby count would print a misleading `count=0` for a sweep
                // that reaped tournaments and nothing else. Both counts are
                // named explicitly rather than summed — they are different
                // kinds of expiry with different cleanup, and a single total
                // would hide which one actually fired.
                let tournament_events = reap_outbounds.len() - expired_lobby.len();
                info!(
                    lobby_games = expired_lobby.len(),
                    tournament_events, "expiring stale lobby entries"
                );
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
                    broadcast_draft_views(draft_code, &bg_connections, &bg_draft_state).await;
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

    // Administrative endpoints are destructive and information-disclosing, and
    // reachable through the same reverse proxy as `/ws` (see deploy nginx).
    // Mount them only when PHASE_ADMIN_TOKEN is set; otherwise absent (404).
    let admin_token = admin_token_from_env();
    match admin_token.as_deref() {
        Some(_) => info!("admin HTTP endpoints enabled (bearer-token authenticated)"),
        None => info!("admin HTTP endpoints disabled (set PHASE_ADMIN_TOKEN to enable)"),
    }

    let app_state = AppState {
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
        context: server_context,
        public_url: advertised_public_url,
        allowed_origin: cli.allowed_origin.clone(),
    };

    // `/p2p-draft-backup*` is part of the normal P2P draft flow; only the
    // administrative `/admin/*` routes are gated. `build_router` keeps `/ws`
    // out from under `CompressionLayer` (see its doc comment) while every
    // other public and admin response gets compressed.
    let app = build_router(app_state.clone(), cors, admin_token.as_deref());

    // Rejected before anything binds. Left alone this surfaces later as "address
    // in use" on one of the two listeners, which reads like a stale process
    // rather than the configuration error it is.
    assert!(
        cli.metrics_port != Some(cli.port),
        "--metrics-port {} is also --port; give the metrics listener its own port",
        cli.port
    );

    // Both fallible announce-setup steps happen once, here, and share one
    // `error!` arm: a bad configuration is a single startup error the operator
    // is watching for, rather than a warning every 60 s forever — and, for the
    // HTTP client, rather than a panic inside a detached task whose handle
    // nothing holds. `requires = "public_url"` guarantees the flag was
    // supplied; it guarantees nothing about the value surviving
    // `validate_public_url`.
    if let Some(endpoint) = cli.announce_to.clone() {
        let announce_setup = announcement_from_state(&app_state).and_then(|base| {
            reqwest::Client::builder()
                .timeout(ANNOUNCE_REQUEST_TIMEOUT)
                .build()
                .map(|client| (base, client))
                .map_err(|error| format!("could not build the announce HTTP client: {error}"))
        });
        match announce_setup {
            Ok((base, client)) => {
                // The handle is discarded as a bare statement, matching the
                // expiry task above: `JoinHandle` is not `#[must_use]`.
                spawn_announce_task(
                    endpoint,
                    client,
                    base,
                    app_state.player_count.clone(),
                    ANNOUNCE_INTERVAL,
                );
            }
            Err(error) => {
                error!(%error, "--announce-to is set but this server cannot announce itself")
            }
        }
    }

    let listener = tokio::net::TcpListener::bind((cli.bind, cli.port))
        .await
        .expect("failed to bind");
    info!(bind = %cli.bind, port = %cli.port, "phase-server listening");

    // A second listener, only when asked for, and only once the public one holds
    // its port: metrics are strictly additive, so nothing about them may be the
    // reason the game server fails to start.
    if let Some(metrics_port) = cli.metrics_port {
        match tokio::net::TcpListener::bind((cli.bind, metrics_port)).await {
            Ok(metrics_listener) => {
                info!(bind = %cli.bind, port = metrics_port, "serving Prometheus metrics on /metrics");
                tokio::spawn(async move {
                    let router = Router::new()
                        .route("/metrics", get(metrics::handler))
                        .with_state(app_state);
                    if let Err(error) = axum::serve(metrics_listener, router).await {
                        error!(%error, "metrics listener stopped");
                    }
                });
            }
            Err(error) => error!(
                %error,
                port = metrics_port,
                "failed to bind metrics port; continuing without metrics"
            ),
        }
    }

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

/// `lobby_broker::ServerMode` mirrors `server_core::protocol::ServerMode`
/// variant-for-variant and serde-shape-for-serde-shape (its own doc comment
/// says so), so this is a total translation with no fallback. Same shape as
/// `to_server_message`'s conversion in the other direction.
fn directory_mode(mode: Mode) -> lobby_broker::ServerMode {
    match mode {
        ServerMode::Full => lobby_broker::ServerMode::Full,
        ServerMode::LobbyOnly => lobby_broker::ServerMode::LobbyOnly,
    }
}

/// Public identity document — the same values `ServerHello` advertises
/// (`handle_socket`), served over plain HTTP so a directory can verify an
/// announcement without opening a WebSocket. `lobby_broker::directory` owns the
/// shape; the Cloudflare Durable Object serves the same document.
async fn server_info(State(app_state): State<AppState>) -> Json<ServerInfoDocument> {
    Json(ServerInfoDocument {
        mode: directory_mode(app_state.mode),
        protocol_version: PROTOCOL_VERSION,
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        build_commit: Some(build_commit().to_string()),
        public_url: app_state.public_url,
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};
    use clap::Parser;
    use tokio::sync::Mutex;

    use url::Url;

    use super::{
        bootstrap_required, build_state_update_message, delist_removed_sessions, origin_is_allowed,
        prune_game_connections, select_card_data_source, validate_public_url, CardDataSource, Cli,
        RegisterGameRequest, ServerMessage, SharedConnections, SharedLobby, SharedLobbySubscribers,
        SysEnv,
    };

    /// CR 118.3 + CR 117.1 — matrix row 21 (actor axis), at a REAL
    /// `ServerMessage` construction site rather than at the engine entry point.
    ///
    /// `build_state_update_message` is the sync builder behind `phase-server`'s
    /// `StateUpdate` fan-out. It derives `activation_block_reasons` from the
    /// SAME `raw_state` binding its sibling `spell_costs` is destructured from,
    /// and passes no `if is_actor` wrapper — the engine entry point self-gates
    /// on `turn_control::is_authorized_submitter`, which is exactly what
    /// `server_core::is_acting` delegates to.
    ///
    /// Both halves asserted in one test so neither is vacuous: the acting seat
    /// receives a NON-EMPTY read-out, and the non-acting seat receives an empty
    /// one from the SAME `ActionResult`.
    ///
    /// RUNNER: CI. Tilt's test resources are scoped `-p phase-engine -p phase-ai`
    /// and never compile `phase-server`, so a green `test-engine` is no evidence
    /// for this test.
    #[test]
    fn state_update_publishes_the_cost_block_readout_only_to_the_acting_seat() {
        // DB-free by construction: `GameState::new_two_player` + `create_object`
        // only, mirroring `mana_display_self_sacrifice_clone_gate.rs`. `phase-server`
        // does not enable the engine's `test-support` feature, so `GameScenario`
        // is not available here.
        use engine::game::game_object::GameObject;
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
        };
        use engine::types::game_state::{GameState, WaitingFor};
        use engine::types::identifiers::CardId;
        use engine::types::phase::Phase;
        use engine::types::player::PlayerId;
        use engine::types::zones::Zone;
        use std::sync::Arc as StdArc;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);

        let mut raw_state = GameState::new_two_player(42);
        raw_state.phase = Phase::PreCombatMain;
        raw_state.waiting_for = WaitingFor::Priority { player: P0 };
        raw_state.priority_player = P0;
        raw_state.players[0].life = 1;

        // `game::zones` is `pub(crate)`, so the object is placed directly:
        // insert into `objects` and push onto the battlefield, which is exactly
        // what `create_object` does for a pre-existing permanent.
        let obj = engine::types::identifiers::ObjectId(raw_state.next_object_id);
        raw_state.next_object_id += 1;
        raw_state.objects.insert(
            obj,
            GameObject::new(
                obj,
                CardId(1),
                P0,
                "Costly Engine".to_string(),
                Zone::Battlefield,
            ),
        );
        raw_state.battlefield.push_back(obj);
        // One unaffordable, resource-verdict ability: `Pay 5 life: draw` at 1 life.
        {
            let o = raw_state.objects.get_mut(&obj).expect("object exists");
            StdArc::make_mut(&mut o.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                }),
            );
        }

        let (legal_actions, spell_costs, by_object) =
            engine::ai_support::legal_actions_full(&raw_state);
        let result: server_core::session::ActionResult = (
            raw_state,
            Vec::new(),
            legal_actions,
            Vec::new(),
            false,
            spell_costs,
            by_object,
        );

        let full_key = server_core::FullSessionKey {
            game_code: "GAME01".to_string(),
            generation: 1,
        };
        let readout_for =
            |player| match build_state_update_message(&result, 1, &full_key, player, Vec::new())
                .expect("the snapshot guard admits this small board")
            {
                ServerMessage::StateUpdate {
                    activation_block_reasons,
                    ..
                } => activation_block_reasons,
                other => panic!("expected a StateUpdate, got {other:?}"),
            };

        let acting = readout_for(P0);
        let other = readout_for(P1);

        // PAIRED POSITIVE, mandatory: without it, a builder that dropped the
        // field entirely would satisfy the negative below.
        assert!(
            acting.get(&obj).is_some_and(|entries| entries.len() == 1),
            "the acting seat's StateUpdate carries the CR 118.3 read-out; got {acting:?}"
        );
        assert!(
            other.is_empty(),
            "a non-acting recipient of the SAME ActionResult gets an empty map; got {other:?}"
        );
    }

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

    /// `PUBLIC_URL` is advertised verbatim to clients and becomes the host half
    /// of every `CODE@host` share string, so the boundary check is what stops a
    /// typo from being handed out as a join address.
    #[test]
    fn public_url_is_accepted_only_as_an_absolute_url_with_a_host() {
        assert_eq!(
            validate_public_url("https://play.example.com"),
            Some("https://play.example.com".to_string())
        );
        assert_eq!(
            validate_public_url("https://play.example.com/"),
            Some("https://play.example.com".to_string()),
            "a trailing slash is trimmed so the join string is not doubled"
        );
        assert_eq!(
            validate_public_url("http://localhost:9374"),
            Some("http://localhost:9374".to_string())
        );

        // Whitespace survives `Url::parse`, so without an explicit trim the
        // padded value is advertised verbatim and ends up inside the
        // "CODE@host" string a player copies out.
        assert_eq!(
            validate_public_url("  https://play.example.com/  "),
            Some("https://play.example.com".to_string())
        );
        assert_eq!(
            validate_public_url("\thttps://play.example.com\n"),
            Some("https://play.example.com".to_string())
        );

        // A bare host is the likeliest operator mistake: it is not a URL.
        assert_eq!(validate_public_url("phase.example.com"), None);
        assert_eq!(validate_public_url("   "), None);
        assert_eq!(validate_public_url("https://"), None);
        assert_eq!(validate_public_url(""), None);

        // These parse cleanly and still have no host. They are what separates
        // the real guard from an `Url::parse(..).is_ok()` check, which would
        // accept both and advertise them to clients.
        assert!(Url::parse("mailto:someone@example.com").is_ok());
        assert_eq!(validate_public_url("mailto:someone@example.com"), None);
        assert!(Url::parse("file:///var/lib/phase-server").is_ok());
        assert_eq!(validate_public_url("file:///var/lib/phase-server"), None);
    }

    /// V13. The flag pair is a coupling clap enforces, not a runtime check:
    /// a directory row is an address, and there is no address to announce
    /// without `--public-url`.
    #[test]
    fn announce_to_requires_a_public_url() {
        // `Cli` is not `Debug`, so `expect_err` is not available here.
        let alone = match Cli::try_parse_from([
            "phase-server",
            "--announce-to",
            "https://d.example/servers/announce",
        ]) {
            Ok(_) => panic!("--announce-to alone must not parse"),
            Err(error) => error,
        };
        // The `kind`, not merely that it errored: a typo'd flag name would
        // otherwise pass this as `UnknownArgument`.
        assert_eq!(
            alone.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        // Paired positive: the same invocation WITH --public-url parses, and
        // both fields are populated.
        let both = Cli::try_parse_from([
            "phase-server",
            "--announce-to",
            "https://d.example/servers/announce",
            "--public-url",
            "https://play.example.com",
        ])
        .expect("--announce-to with --public-url parses");
        assert_eq!(
            both.announce_to.as_ref().map(Url::to_string),
            Some("https://d.example/servers/announce".to_string())
        );
        assert_eq!(both.public_url.as_deref(), Some("https://play.example.com"));

        // Absent by default: announcing is opt-in.
        let default = Cli::try_parse_from(["phase-server"]).expect("default CLI parses");
        assert!(default.announce_to.is_none());
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

    #[tokio::test]
    async fn delist_removed_sessions_delists_every_removed_room_and_leaves_the_rest() {
        // The reaper hands this helper every session it removed in one tick,
        // which is more than one, so a single-room fixture would admit a
        // projection that stranded all but the first.
        use engine::game::deck_loading::PlayerDeckPayload;
        let mut mgr = server_core::session::SessionManager::new();
        let (code_a, _) = mgr.create_game(PlayerDeckPayload::default(), None);
        let (code_b, _) = mgr.create_game(PlayerDeckPayload::default(), None);
        let (kept_code, _) = mgr.create_game(PlayerDeckPayload::default(), None);

        let lobby: SharedLobby = Arc::new(Mutex::new(Broker::new()));
        {
            let mut lob = lobby.lock().await;
            for code in [&code_a, &code_b, &kept_code] {
                lob.lobby_mut().register_game(
                    code,
                    RegisterGameRequest {
                        host_name: "Host".to_string(),
                        public: true,
                        ..Default::default()
                    },
                    &SysEnv,
                );
            }
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(vec![tx]));

        // Reach guard: all three rooms really are listed, so the assertions
        // below cannot pass against a lobby that was never populated.
        {
            let lob = lobby.lock().await;
            for code in [&code_a, &code_b, &kept_code] {
                assert!(
                    lob.lobby().has_game(code),
                    "fixture did not register {code}"
                );
            }
        }

        let removed: Vec<_> = [&code_a, &code_b]
            .into_iter()
            .map(|code| mgr.remove_game(code).expect("the session is removed"))
            .collect();
        delist_removed_sessions(&lobby, &subscribers, &removed).await;

        {
            let lob = lobby.lock().await;
            for code in [&code_a, &code_b] {
                assert!(
                    !lob.lobby().has_game(code),
                    "a removed session must not stay listed: {code}"
                );
            }
            assert!(
                lob.lobby().has_game(&kept_code),
                "a session the reaper did not remove must stay listed"
            );
        }

        let mut announced = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ServerMessage::LobbyGameRemoved { game_code } => announced.push(game_code),
                other => panic!("expected only LobbyGameRemoved, got {other:?}"),
            }
        }
        announced.sort();
        let mut expected = vec![code_a, code_b];
        expected.sort();
        assert_eq!(
            announced, expected,
            "every removed public room announces once"
        );
    }

    // -- Tournament reaper fan-out -----------------------------------------

    use lobby_broker::{
        Broker, BrokerEnv, ConnState, LobbyClientMessage, LobbyServerMessage, Outbound,
    };
    use server_core::protocol::{BracketShape, MatchArity, ScoringPolicy};
    use std::cell::Cell;

    /// Settable-clock `BrokerEnv`. `SysEnv` reads the real wall clock, and the
    /// tournament lifecycle windows are fixed multi-day constants, so the
    /// production env cannot reach an expiry inside a test.
    struct FakeEnv {
        now: Cell<u64>,
        token: Cell<u64>,
        code: Cell<u64>,
    }
    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn new_token(&self) -> String {
            let n = self.token.get();
            self.token.set(n + 1);
            format!("token-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code.get();
            self.code.set(n + 1);
            format!("CODE{n:02}")
        }
    }

    /// Verification Matrix row 10. `Broker::reap_expired` returning the right
    /// outbounds is necessary but NOT sufficient — the shell's reap block also
    /// has to forward them. This drives the block's two real steps in the same
    /// order the production code does:
    ///
    ///   1. the `expired_lobby` `filter_map`, which is lobby-only by design
    ///      (a tournament has no Full-mode session to retire), and
    ///   2. the fan-out loop, which iterates the FULL `reap_outbounds` rather
    ///      than the filtered list — the reason tournaments reach subscribers
    ///      with no new code in the loop.
    ///
    /// If step 2 were ever narrowed to `expired_lobby`, a subscribed client
    /// would receive nothing when a tournament it is watching expires, while
    /// every broker-level test kept passing.
    #[tokio::test]
    async fn the_reaper_fans_tournament_expiry_out_to_subscribed_clients() {
        let env = FakeEnv {
            now: Cell::new(1_000_000),
            token: Cell::new(0),
            code: Cell::new(0),
        };
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        let created = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            &env,
        );
        let tour_code = match &created[0] {
            Outbound::ToSelf(LobbyServerMessage::TournamentCreated { code, .. }) => code.clone(),
            other => panic!("expected TournamentCreated, got {other:?}"),
        };

        // Past the registration window, so the sweep reaps it.
        env.now
            .set(env.now.get() + (lobby_broker::REGISTRATION_TIMEOUT_SECS + 1) * 1000);
        let reap_outbounds = broker.reap_expired(300, &env);
        assert!(!reap_outbounds.is_empty());

        // Step 1, verbatim from the reap block.
        let expired_lobby: Vec<String> = reap_outbounds
            .iter()
            .filter_map(|ob| match ob {
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code }) => {
                    Some(game_code.clone())
                }
                _ => None,
            })
            .collect();
        assert!(
            expired_lobby.is_empty(),
            "a tournament must not be mistaken for a lobby game needing session cleanup"
        );

        // Step 2, verbatim: one subscribed client, and the fan-out loop.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for ob in reap_outbounds {
            if let Outbound::ToSubscribers(msg) = ob {
                let server_msg = super::to_server_message(msg);
                let _ = tx.send(server_msg);
            }
        }
        drop(tx);

        let mut received = Vec::new();
        while let Some(msg) = rx.recv().await {
            received.push(msg);
        }

        assert!(
            received.iter().any(|m| matches!(
                m,
                server_core::protocol::ServerMessage::TournamentRemoved { code } if *code == tour_code
            )),
            "a subscribed client must be told the tournament is gone: {received:?}"
        );
        assert!(
            received.iter().any(|m| matches!(
                m,
                server_core::protocol::ServerMessage::TournamentListUpdate { .. }
            )),
            "...and receive the refreshed list: {received:?}"
        );
    }
}

#[cfg(test)]
mod restored_draft_startup_tests {
    use super::restore_persisted_draft_session;
    use draft_core::types::{
        DeckAddableCards, DraftConfig, DraftKind, DraftSource, PodPolicy, SpectatorVisibility,
        TournamentFormat,
    };
    use server_core::draft_session::DraftSessionManager;
    use server_core::persist::{PersistedDraftSession, PersistedLobbyMeta};

    fn uniform_lobby_snapshot() -> PersistedDraftSession {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let mut drafts = DraftSessionManager::new();
        let (draft_code, _, _) = drafts.create_draft(config, "Alice".to_string());
        drafts.sessions.get_mut(&draft_code).unwrap().lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });
        drafts.sessions[&draft_code].to_persisted()
    }

    #[test]
    fn persisted_draft_row_key_must_match_before_admission_or_lobby_registration() {
        let snapshot = uniform_lobby_snapshot();
        let mut restored = DraftSessionManager::new();

        let rejected_registration =
            match restore_persisted_draft_session("WRONG1", snapshot.clone(), &mut restored) {
                Ok((registration, _)) => registration,
                Err(_) => None,
            };
        assert!(rejected_registration.is_none());
        assert!(restored.sessions.is_empty());

        let (registration, _) =
            restore_persisted_draft_session(&snapshot.draft_code, snapshot.clone(), &mut restored)
                .expect("a valid Uniform snapshot restores under its own row key");
        assert!(registration.is_some());
        assert!(restored.sessions.contains_key(&snapshot.draft_code));
    }
}

#[cfg(test)]
mod restored_full_startup_tests {
    use std::sync::Arc;

    use engine::database::CardDatabase;
    use engine::game::deck_loading::PlayerDeckPayload;
    use engine::types::game_state::WaitingFor;
    use engine::types::player::PlayerId;
    use server_core::session::GameSession;
    use server_core::{FullPersistSnapshot, FullSessionKey, SessionManager};
    use tempfile::NamedTempFile;

    use super::{finish_restored_full_startup, persistence, RestoredFullStartup, SharedGameDb};

    fn test_db() -> (NamedTempFile, SharedGameDb) {
        let file = NamedTempFile::new().expect("temporary game database");
        let db = Arc::new(
            persistence::GameDb::open(file.path(), persistence::SessionRetention::Multiplayer)
                .expect("open temporary game database"),
        );
        (file, db)
    }

    fn snapshot_for(
        game_code: String,
        generation: u64,
        session: &GameSession,
    ) -> FullPersistSnapshot {
        FullPersistSnapshot {
            key: FullSessionKey {
                game_code,
                generation,
            },
            mutation_revision: session.state_revision,
            activation_epoch: None,
            persisted: session.to_persisted(),
        }
    }

    fn restore(snapshot: &FullPersistSnapshot) -> GameSession {
        GameSession::from_persisted(
            snapshot.persisted.clone(),
            &Arc::new(CardDatabase::default()),
        )
        .expect("persisted test session restores")
    }

    #[test]
    fn startup_restore_keeps_ordinary_priority_as_a_revision_preserving_noop() {
        let mut source_manager = SessionManager::new();
        let (game_code, _) = source_manager.create_game(PlayerDeckPayload::default(), None);
        let source = source_manager
            .sessions
            .get(&game_code)
            .expect("source session exists");
        let snapshot = snapshot_for(game_code.clone(), 1, source);
        let (_file, db) = test_db();
        db.save_full_session(&snapshot).expect("seed snapshot");

        let mut target_manager = SessionManager::new();
        assert_eq!(
            finish_restored_full_startup(&mut target_manager, &db, &snapshot, restore(&snapshot))
                .expect("ordinary restore remains active"),
            RestoredFullStartup::Active
        );

        assert_eq!(
            target_manager.sessions[&game_code].state_revision, snapshot.mutation_revision,
            "ordinary priority is not an implicit pass"
        );
        assert_eq!(
            db.load_active_full_sessions().expect("read seeded row")[0].mutation_revision,
            snapshot.mutation_revision,
            "a no-op does not manufacture a persistence revision"
        );
    }

    #[test]
    fn startup_game_over_uses_terminal_persistence_instead_of_exposure() {
        let mut source_manager = SessionManager::new();
        let (game_code, token) = source_manager.create_game(PlayerDeckPayload::default(), None);
        let source = source_manager
            .sessions
            .get_mut(&game_code)
            .expect("source session exists");
        source.state.waiting_for = WaitingFor::GameOver { winner: None };
        let snapshot = snapshot_for(game_code.clone(), 1, source);
        let (_file, db) = test_db();
        db.save_full_session(&snapshot)
            .expect("seed terminal snapshot");

        let mut target_manager = SessionManager::new();
        assert_eq!(
            finish_restored_full_startup(&mut target_manager, &db, &snapshot, restore(&snapshot))
                .expect("terminal startup handoff commits"),
            RestoredFullStartup::Terminal
        );

        assert!(
            !target_manager.sessions.contains_key(&game_code),
            "a terminal session is never exposed as an active game"
        );
        assert_eq!(
            target_manager.game_for_token(&token),
            None,
            "terminal startup cleanup removes the private token index too"
        );
        assert!(
            db.load_active_full_sessions()
                .expect("read active sessions")
                .is_empty(),
            "terminal preparation retires the active persistence row"
        );
        assert!(
            db.current_terminal_delivery_for_recipient(&snapshot.key, PlayerId(0), &token)
                .expect("read terminal delivery")
                .is_some(),
            "the normal terminal recovery path remains available"
        );
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

/// Assembles the public HTTP/WS surface. `/ws` is kept on its own sub-router,
/// deliberately outside `CompressionLayer`: the WebSocket upgrade response
/// (`101 Switching Protocols`) carries no body to compress, and axum's
/// upgrade takes the connection over via `hyper`'s `OnUpgrade` extension
/// rather than through the response body a compressing layer would wrap —
/// keeping the layer off that route entirely removes any dependence on that
/// implementation detail continuing to hold across axum/tower-http upgrades.
/// Every other route (health check, the `lobby_broker::directory::INFO_PATH`
/// identity document, the P2P draft backup API, and — when `admin_token` is
/// set — the bearer-guarded `/admin/*` routes) is served through gzip
/// `CompressionLayer` so JSON/text responses shrink whenever the client
/// advertises `Accept-Encoding: gzip`.
fn build_router(app_state: AppState, cors: CorsLayer, admin_token: Option<&str>) -> Router {
    let ws_router: Router<AppState> = Router::new().route("/ws", get(ws_handler));

    let mut http_router: Router<AppState> = Router::new()
        .route("/health", get(health))
        // The constant, never a `"/info"` literal: every server kind must
        // answer its info document on the same path a directory probes.
        .route(INFO_PATH, get(server_info))
        .route("/p2p-draft-backup", post(admin::p2p_backup_store))
        .route(
            "/p2p-draft-backup/{code}",
            get(admin::p2p_backup_get).delete(admin::p2p_backup_delete),
        );
    if let Some(token) = admin_token.filter(|t| !t.is_empty()) {
        http_router = mount_admin_routes(http_router, token);
    }
    let http_router = http_router.layer(CompressionLayer::new());

    ws_router
        .merge(http_router)
        .layer(cors)
        .with_state(app_state)
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
    // `Url::parse` tolerates surrounding whitespace, so returning `raw` would
    // advertise it verbatim in ServerHello and bake it into the "CODE@host"
    // share string.
    let trimmed = raw.trim();
    match Url::parse(trimmed) {
        Ok(u) if u.host_str().is_some() => Some(trimmed.trim_end_matches('/').to_string()),
        _ => {
            warn!(value = %raw, "ignoring malformed PUBLIC_URL (need an absolute URL with a host)");
            None
        }
    }
}

/// How often a server re-asserts its directory listing. The directory expires
/// rows that stop being re-asserted, so this is a heartbeat, not a one-shot
/// registration.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
/// Ceiling on one announce POST. A directory that hangs must never accumulate
/// in-flight requests behind a 60 s heartbeat.
const ANNOUNCE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build this server's directory announcement from live state. `Err` when the
/// server has no announceable address: no `--public-url` (a malformed value is
/// dropped by [`validate_public_url`], so the flag being present guarantees
/// nothing about the value), a non-`https` one (the directory lists `wss://`
/// only, and an `http://` public URL would announce an address this server does
/// not serve), or one whose host the directory contract refuses.
///
/// Assembly only, with one deliberate exception: every rule it could have
/// inlined — scheme, host shape, normalisation — lives in
/// `lobby_broker::directory` and is applied by the same
/// [`validate_announcement`] the directory itself runs. The exception is the
/// name cap: `name` is the host truncated to [`MAX_SERVER_NAME_LEN`], while
/// `url` keeps the full host, so a host too long to be a display name still
/// yields an announceable address rather than no announcement at all.
fn announcement_from_state(state: &AppState) -> Result<ServerAnnouncement, String> {
    let public_url = state.public_url.as_deref().ok_or_else(|| {
        "no public URL to announce (--public-url is unset, or was dropped as malformed)".to_string()
    })?;
    let parsed =
        Url::parse(public_url).map_err(|error| format!("unparseable --public-url: {error}"))?;
    if parsed.scheme() != "https" {
        return Err(format!(
            "--public-url scheme is `{}`; a directory listing must be wss://, which requires an \
             https:// public URL",
            parsed.scheme()
        ));
    }
    // `host_str()` + `port()`, never `authority()` — the latter carries
    // userinfo through into the announced address.
    let host = parsed
        .host_str()
        .ok_or_else(|| "--public-url has no host".to_string())?;
    let port_suffix = parsed
        .port()
        .map_or(String::new(), |port| format!(":{port}"));
    // A host longer than the NAME cap must not disable announcing outright:
    // `MAX_SERVER_URL_LEN` is 256 while `MAX_SERVER_NAME_LEN` is 64, so a host
    // between those bounds yields an address the directory contract accepts and
    // a display name it rejects. Truncate the name rather than fail the whole
    // announcement — `url()` keeps the full host, and that is the field
    // identity turns on.
    //
    // Truncating by CHARACTERS, which is the unit `validate_required_label`
    // itself bounds (`value.chars().count()`), so this is the exact inverse of
    // the check it has to satisfy and cannot split a codepoint. Characters and
    // bytes also coincide here, but the guarantee for that is `Url::host_str()`
    // having already applied IDNA ToASCII on a special scheme — and the scheme
    // was checked to be `https` above (measured: `https://bücher.example`
    // parses to host `xn--bcher-kva.example`). That guarantee is UPSTREAM of
    // this line. The directory contract's own ASCII rule is not: it runs inside
    // `validate_announcement` below, so nothing here may lean on it.
    let name: String = host.chars().take(MAX_SERVER_NAME_LEN).collect();
    // `/ws` by construction: this server is describing its own route, and
    // `build_router` registers exactly that path.
    let raw = RawAnnouncement {
        directory_version: DIRECTORY_VERSION,
        url: format!("wss://{host}{port_suffix}/ws"),
        name,
        mode: directory_mode(state.mode),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: PROTOCOL_VERSION,
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
        current_players: state.player_count.load(Ordering::Relaxed),
    };
    validate_announcement(&raw)
}

/// Heartbeat POSTing `base` every `period`, refreshing only the live player
/// count. Warn-and-retry: a directory that is down, slow or rejecting must
/// never affect the game server. Detached like the expiry task in `serve` — it
/// ends with the runtime at process exit.
///
/// `base` is already validated (built once by [`announcement_from_state`] at
/// startup) and `client` is already built (by the caller, carrying
/// [`ANNOUNCE_REQUEST_TIMEOUT`]), so the task has no *fallible* setup — it
/// cannot fail to build an announcement and it cannot fail to build a client.
/// Both of those failures are the caller's single startup `error!`; building
/// the client in here instead would put either a fourth outcome or a silent
/// panic inside a task that is required never to die and whose handle
/// production discards. Three per-tick failure classes remain, each logged at
/// warn with the tick skipped and the loop continuing: a transport error, a
/// non-success status, and a serialization error from `serde_json::to_vec`.
/// `period` is a parameter so the test drives the real loop without a
/// wall-clock minute; production passes [`ANNOUNCE_INTERVAL`]. The handle is
/// returned only so that test can assert the task is still alive.
///
/// # Panics
///
/// `period` must be non-zero. `tokio::time::interval` panics on a zero period,
/// and it would do so at the first poll *inside* the spawned task — precisely
/// the unobservable failure mode the caller-built `client` exists to avoid,
/// since production discards the handle. The `debug_assert!` below moves that
/// failure onto the caller's thread instead.
fn spawn_announce_task(
    endpoint: Url,
    client: reqwest::Client,
    base: ServerAnnouncement,
    player_count: SharedPlayerCount,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    // Deliberately in the synchronous body rather than beside the `interval`
    // call it guards: a panic raised inside the spawned task is exactly the
    // unobservable failure this assertion exists to catch, so it has to fire on
    // the caller's thread to be worth anything. No runtime branch — the loop
    // keeps exactly its three per-tick failure classes.
    debug_assert!(!period.is_zero(), "announce period must be non-zero");
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        // A stalled process must not fire a burst of announcements on catch-up.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // `interval`'s first tick completes immediately, so the server
            // announces at startup and then every `period`.
            interval.tick().await;
            let announcement = base.with_current_players(player_count.load(Ordering::Relaxed));
            let body = match serde_json::to_vec(&announcement) {
                Ok(body) => body,
                Err(error) => {
                    warn!(%error, "could not serialize the directory announcement; skipping this tick");
                    continue;
                }
            };
            let sent = client
                .post(endpoint.clone())
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await;
            match sent {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => warn!(
                    status = %response.status(),
                    endpoint = %endpoint,
                    "directory refused this announcement; retrying next tick"
                ),
                Err(error) => warn!(
                    %error,
                    endpoint = %endpoint,
                    "could not reach the directory; retrying next tick"
                ),
            }
        }
    })
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
    /// Limits, replica identity and the rejection counters. See [`ServerContext`].
    context: ServerContext,
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
        app_state
            .context
            .metrics
            .record_reject(metrics::RejectReason::OriginNotAllowed);
        return (http::StatusCode::FORBIDDEN, "WebSocket Origin not allowed").into_response();
    }
    // Reserved in the same atomic operation that tests it. A load, then a check,
    // then an increment further down admits every handshake that raced into the
    // gap, so a cap of N can be overshot by however many arrive together.
    //
    // `Relaxed` throughout, as everywhere else on this counter: the read-modify
    // -write is atomic whatever the ordering, and no other state is published
    // through it — the value is only ever compared against the cap.
    let reserved =
        app_state
            .player_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |online| {
                (online < app_state.context.limits.max_connections).then_some(online + 1)
            });
    let online_count = match reserved {
        Ok(previous) => previous + 1,
        Err(current) => {
            warn!(
                online_count = current,
                limit = app_state.context.limits.max_connections,
                "connection limit reached, rejecting"
            );
            app_state
                .context
                .metrics
                .record_reject(metrics::RejectReason::ConnectionLimit);
            return (http::StatusCode::SERVICE_UNAVAILABLE, "Server full").into_response();
        }
    };
    let slot = ConnectionSlot::new(app_state.player_count.clone());

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
                app_state.context,
                app_state.public_url,
                online_count,
                slot,
            )
        })
        .into_response()
}

/// A connection slot reserved before the WebSocket upgrade.
///
/// `ws_handler` reserves atomically so racing handshakes cannot overshoot the
/// cap, but the upgrade may never reach `handle_socket` — axum drops the
/// callback when the handshake fails — and a reservation leaked that way would
/// wedge the server one slot below capacity forever. Dropping the guard
/// releases it. `handle_socket` disarms it and owns the release from then on,
/// because that path also has to broadcast the new count, which `Drop` cannot.
struct ConnectionSlot {
    player_count: SharedPlayerCount,
    armed: bool,
}

/// Per-connection WebSocket writer. Once the text handshake negotiates a wire
/// format, every later JSON text send passes through the same envelope path —
/// including direct errors as well as queued broadcasts.
struct NegotiatedSocket {
    inner: WebSocket,
    wire_format: Option<WireFormat>,
}

impl NegotiatedSocket {
    fn new(inner: WebSocket) -> Self {
        Self {
            inner,
            wire_format: None,
        }
    }

    fn set_wire_format(&mut self, wire_format: Option<WireFormat>) {
        self.wire_format = wire_format;
    }

    fn uses_gzip_envelope(&self) -> bool {
        self.wire_format == Some(WireFormat::GzipEnvelopeV1)
    }

    async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        let message = match message {
            Message::Text(json) => {
                wire::encode_json_message(json.to_string(), self.uses_gzip_envelope())
                    .await
                    .map_err(axum::Error::new)?
            }
            other => other,
        };
        self.inner.send(message).await
    }

    async fn recv(&mut self) -> Option<Result<Message, axum::Error>> {
        self.inner.recv().await
    }
}

impl ConnectionSlot {
    fn new(player_count: SharedPlayerCount) -> Self {
        Self {
            player_count,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        if self.armed {
            self.player_count.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket(
    socket: WebSocket,
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
    context: ServerContext,
    public_url: Option<String>,
    online_count: u32,
    slot: ConnectionSlot,
) {
    let mut socket = NegotiatedSocket::new(socket);
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // The slot was reserved before the upgrade; from here the two `fetch_sub`
    // paths below own the release, so the guard must not also fire.
    slot.disarm();
    let count = online_count;
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
        lobby_organized_tournaments: Vec::new(),
        lobby_joined_tournaments: Vec::new(),
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
        wire_formats: vec![WireFormat::GzipEnvelopeV1],
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
                    if socket.send(Message::text(json)).await.is_err() { break; }
                }
            }

            result = socket.recv() => {
                match result {
                    Some(Ok(msg)) => {
                        if !rate_limiter.check() {
                            debug!("rate limit exceeded, dropping message");
                            continue;
                        }

                        let text = match msg {
                            Message::Text(t) => t.to_string(),
                            Message::Binary(bytes) if socket.uses_gzip_envelope() => {
                                match wire::decode_client_envelope(bytes.to_vec(), MAX_WS_MESSAGE_BYTES).await {
                                    Ok(text) => text,
                                    Err(error) => {
                                        warn!(%error, "failed to decode client binary envelope");
                                        continue;
                                    }
                                }
                            }
                            Message::Close(_) => break,
                            _ => continue,
                        };

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
                            &context,
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
    disconnect_draft_seat_if_current(&draft_state, &connections, &identity, &tx).await;

    disconnect_full_seat_if_current(&state, &connections, &identity, &tx).await;

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

/// The single place a `PlayerSlotsUpdate` is built: the message is rebuilt per
/// recipient, so no send site can skip `slots_for_recipient`.
fn send_player_slots<'a>(
    recipients: impl IntoIterator<Item = (&'a PlayerId, &'a mpsc::UnboundedSender<ServerMessage>)>,
    slots: &[server_core::PlayerSlotInfo],
) {
    for (pid, sender) in recipients {
        let _ = sender.send(ServerMessage::PlayerSlotsUpdate {
            slots: GameSession::slots_for_recipient(slots, *pid),
        });
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
    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        send_player_slots(players.iter(), &slots);
    }
}

/// Delists every destroyed code and returns those that were still publicly
/// listed, for the `LobbyGameRemoved` fan-out.
///
/// The guarantee is one-sided: destroying a session or draft removes its lobby
/// entry. The reverse does not hold — `LobbyManager::check_expired` reaps on a
/// registration timestamp that is never refreshed, so an entry is an
/// advertisement with a TTL and can lapse while its subject is alive.
///
/// `public_game` returns `None` both for a private entry and for a missing one,
/// which is exactly the `existed && public_before` condition the game-start
/// delists compute by hand; `unregister_game` on an absent code is a no-op.
fn delist_destroyed_sessions<'a>(
    lob: &mut lobby_broker::lobby::LobbyManager,
    codes: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let mut announce = Vec::new();
    for code in codes {
        if lob.public_game(code).is_some() {
            announce.push(code.to_string());
        }
        lob.unregister_game(code);
    }
    announce
}

/// Takes the lobby lock alone and drops the guard before awaiting the
/// subscriber fan-out, so a caller must hold neither the lobby lock nor any
/// lock the loops order after it.
async fn delist_and_announce<'a>(
    lobby: &SharedLobby,
    subscribers: &SharedLobbySubscribers,
    codes: impl Iterator<Item = &'a str>,
) {
    let announce = {
        let mut lob_guard = lobby.lock().await;
        delist_destroyed_sessions(lob_guard.lobby_mut(), codes)
    };
    announce_delisted(subscribers, announce).await;
}

/// Delists the sessions the reaper removed. Projecting them onto their game
/// codes inside the helper puts that step under test and leaves the wider
/// `expired` list the removals were filtered from unable to typecheck here.
async fn delist_removed_sessions(
    lobby: &SharedLobby,
    subscribers: &SharedLobbySubscribers,
    removed: &[server_core::session::GameSession],
) {
    delist_and_announce(
        lobby,
        subscribers,
        removed.iter().map(|session| session.game_code.as_str()),
    )
    .await;
}

/// Fans out one `LobbyGameRemoved` per code returned by
/// [`delist_destroyed_sessions`].
async fn announce_delisted(lobby_subscribers: &SharedLobbySubscribers, codes: Vec<String>) {
    for game_code in codes {
        broadcast_to_lobby_subscribers(
            lobby_subscribers,
            ServerMessage::LobbyGameRemoved { game_code },
        )
        .await;
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
            // Socket negotiation uses the canonical ServerHello sent directly
            // by handle_socket; broker-originated hellos are only projections.
            wire_formats: Vec::new(),
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
        // Tournament variants. `view`/`tournaments` move rather than clone —
        // the view types are re-exported from `lobby_broker::protocol`, so
        // both enums name the same struct and this stays a pure re-tag.
        L::TournamentCreated {
            code,
            organizer_token,
            view,
        } => ServerMessage::TournamentCreated {
            code,
            organizer_token,
            view,
        },
        L::TournamentJoined {
            code,
            player_token,
            view,
        } => ServerMessage::TournamentJoined {
            code,
            player_token,
            view,
        },
        L::TournamentUpdate { code, view } => ServerMessage::TournamentUpdate { code, view },
        L::TournamentRemoved { code } => ServerMessage::TournamentRemoved { code },
        L::TournamentListUpdate { tournaments } => {
            ServerMessage::TournamentListUpdate { tournaments }
        }
        // The two correlated settlement replies. `request_id` is the same
        // re-exported `TournamentRequestId` on both enums, so this stays a pure
        // re-tag like every arm above it.
        L::TournamentActionAck {
            request_id,
            code,
            view,
        } => ServerMessage::TournamentActionAck {
            request_id,
            code,
            view,
        },
        L::TournamentActionRejected {
            request_id,
            message,
        } => ServerMessage::TournamentActionRejected {
            request_id,
            message,
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
            wire_formats: _,
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
        // Tournament variants. NOTE the asymmetry with `to_server_message`:
        // that function is wildcard-free, so a forgotten arm there is a
        // compile error. THIS function ends in `_ => return None`, because
        // most `ClientMessage` variants are legitimately non-lobby — so a
        // forgotten arm here is NOT a compile error. It silently returns
        // `None`, which `dispatch_broker` reads as "not a lobby message" and
        // simply does not dispatch, leaving the client's request answered by
        // nothing at all. `tournament_variants_survive_the_canonical_lobby_roundtrip`
        // asserts `.is_some()` for every one of these seven precisely because
        // the compiler cannot.
        ClientMessage::CreateTournament {
            name,
            arity,
            scoring,
            bracket,
            total_rounds,
        } => L::CreateTournament {
            name: name.clone(),
            arity: *arity,
            scoring: *scoring,
            bracket: *bracket,
            total_rounds: *total_rounds,
        },
        ClientMessage::JoinTournament {
            code,
            player_key,
            display_name,
        } => L::JoinTournament {
            code: code.clone(),
            player_key: player_key.clone(),
            display_name: display_name.clone(),
        },
        ClientMessage::GetTournament { code } => L::GetTournament { code: code.clone() },
        // The four GATED actions FORWARD their correlator rather than dropping
        // it. Binding it by name is compiler-forced; forwarding the bound value
        // is not — `request_id: _` on the pattern plus `request_id: None` on the
        // construction compiles perfectly and is exactly the bug: it would
        // silently uncorrelate every request from a native `phase-server`
        // client while the Cloudflare Worker path, which never re-tags, kept
        // working. `tournament_variants_survive_the_canonical_lobby_roundtrip`
        // is the guard, and it only bites because one fixture carries a real
        // `Some(..)` — under `skip_serializing_if = "Option::is_none"` a
        // forwarded `None` and a discarded one serialize identically.
        ClientMessage::StartTournamentRound {
            code,
            organizer_token,
            request_id,
        } => L::StartTournamentRound {
            code: code.clone(),
            organizer_token: organizer_token.clone(),
            request_id: *request_id,
        },
        ClientMessage::ReportMatchResult {
            code,
            pairing_id,
            player_token,
            outcome,
            request_id,
        } => L::ReportMatchResult {
            code: code.clone(),
            pairing_id: *pairing_id,
            player_token: player_token.clone(),
            outcome: outcome.clone(),
            request_id: *request_id,
        },
        ClientMessage::DropFromTournament {
            code,
            player_token,
            request_id,
        } => L::DropFromTournament {
            code: code.clone(),
            player_token: player_token.clone(),
            request_id: *request_id,
        },
        ClientMessage::EndTournament {
            code,
            organizer_token,
            request_id,
        } => L::EndTournament {
            code: code.clone(),
            organizer_token: organizer_token.clone(),
            request_id: *request_id,
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
    /// The host's submitted card-name list — seat 0's deck provenance.
    host_choice: seat_reducer::types::DeckChoice,
    display_name: String,
    timer_seconds: Option<u32>,
    pc: u8,
    match_config: engine::types::match_config::MatchConfig,
    format_config: Option<engine::types::format::FormatConfig>,
    start_when_full: bool,
    ranked: bool,
    ai_requests: Vec<server_core::session::AiSeatSetup>,
    public: bool,
    password: Option<String>,
    host_tx: mpsc::UnboundedSender<ServerMessage>,
    context: ServerContext,
}

/// The single authority for what deck each AI seat gets and what choice
/// describes it. `Err` refuses the create, matching every other deck failure
/// in the handler this replaces the inline loop in — a seat whose deck cannot
/// be resolved has no honest provenance to record.
fn ai_seat_setups(
    db: &engine::database::CardDatabase,
    ai_seats: &[server_core::protocol::AiSeatRequest],
    pc: u8,
    format_config: Option<&engine::types::format::FormatConfig>,
    match_type: engine::types::match_config::MatchType,
) -> Result<Vec<server_core::session::AiSeatSetup>, String> {
    use seat_reducer::types::DeckChoice;

    let mut setups = Vec::new();
    for seat in ai_seats {
        if seat.seat_index == 0 || seat.seat_index >= pc {
            continue;
        }
        let effective = match &seat.deck {
            Some(DeckChoice::DeckList(deck)) => deck.as_ref().clone(),
            Some(DeckChoice::Named(name)) => named_starter_or_random(name),
            Some(DeckChoice::Random) | None => match &seat.deck_name {
                Some(name) if name.eq_ignore_ascii_case("random") => {
                    server_core::starter_decks::random_starter_deck()
                }
                Some(name) => named_starter_or_random(name),
                None => server_core::starter_decks::random_starter_deck(),
            },
        };
        if let Some(fc) = format_config {
            if let Err(reasons) = validate_name_deck_for_format_full(
                db,
                &effective.main_deck,
                &effective.sideboard,
                &effective.commander,
                &effective.companion,
                &effective.planar_deck,
                &effective.scheme_deck,
                &effective.signature_spell,
                // Constructed play: no draft, no concession.
                &[],
                fc,
                Some(match_type),
                usize::from(pc),
            ) {
                return Err(format!(
                    "AI deck for seat {} not legal for {}: {}",
                    seat.seat_index,
                    fc.format.label(),
                    reasons.join("; ")
                ));
            }
        }
        let resolved = resolve_deck(db, &effective)?;
        setups.push(server_core::session::AiSeatSetup {
            seat_index: seat.seat_index,
            difficulty: seat.difficulty,
            // The deck the seat actually got. `seat.deck` would report
            // `Random` for a seat holding a named starter — a field whose whole
            // purpose is the choice, naming something that is not it.
            choice: DeckChoice::DeckList(Box::new(effective)),
            resolved,
        });
    }
    Ok(setups)
}

fn named_starter_or_random(name: &str) -> engine::starter_decks::DeckData {
    server_core::starter_decks::find_starter_deck(name).unwrap_or_else(|| {
        warn!(deck = %name, "unknown AI deck name, using random");
        server_core::starter_decks::random_starter_deck()
    })
}

/// Phases 1–2 of the `CreateGameWithSettings` full multiplayer path.
///
/// Creates the session, configures AI seats and lobby metadata, then registers
/// the host sender while retaining state -> connections ordering. Both locks
/// are released before this function returns, so callers may
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
) -> Result<(String, String, PlayerId, u32, server_core::FullSessionKey), String> {
    let MultiplayerSessionRequest {
        resolved,
        host_choice,
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
        context,
    } = req;

    // State lock; host sender is installed under the same transaction.
    let (game_code, player_token, host_player, initial_player_count, full_key) = {
        let mut mgr = state.lock().await;
        // Sole capacity check for the multiplayer path, under the lock that
        // inserts — see the `CreateGame` arm for why it cannot move ahead of
        // deck resolution.
        if mgr.sessions.len() >= context.limits.max_games {
            warn!(
                limit = context.limits.max_games,
                "max games reached, rejecting CreateGameWithSettings"
            );
            context
                .metrics
                .record_reject(metrics::RejectReason::GameLimit);
            return Err("Server is at game capacity, please try again later".to_string());
        }
        let (game_code, player_token) = mgr.create_game_n_players(
            resolved,
            Some(host_choice),
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
            for setup in ai_requests {
                session.seat_ai(setup);
            }
        }

        let initial_player_count = mgr
            .sessions
            .get(&game_code)
            .map(|s| s.current_player_count())
            .unwrap_or(1);
        let host_player = mgr
            .sessions
            .get(&game_code)
            .and_then(|session| session.player_for_token(&player_token))
            .expect("new Full session must resolve its host token");

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

        install_full_sender_while_state_locked(connections, &game_code, host_player, &host_tx)
            .await;

        (
            game_code,
            player_token,
            host_player,
            initial_player_count,
            full_key,
        )
    }; // state lock released here

    Ok((
        game_code,
        player_token,
        host_player,
        initial_player_count,
        full_key,
    ))
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

/// Whether terminal delivery preparation failed before or after its database
/// transaction committed. The latter has already retired the active runtime,
/// so callers must never restore it in memory.
enum TerminalPreparationFailure {
    BeforeTerminalCommit(String),
    AfterTerminalCommit(String),
}

impl TerminalPreparationFailure {
    fn message(self) -> String {
        match self {
            Self::BeforeTerminalCommit(message) | Self::AfterTerminalCommit(message) => message,
        }
    }
}

async fn prepare_full_terminal_with_commit_status(
    game_db: &SharedGameDb,
    artifact: persistence::FullTerminalArtifact,
) -> Result<Vec<(PlayerId, server_core::CurrentTerminalDelivery)>, TerminalPreparationFailure> {
    let db = game_db.clone();
    tokio::task::spawn_blocking(move || -> Result<_, TerminalPreparationFailure> {
        db.prepare_full_terminal(&artifact).map_err(|error| {
            TerminalPreparationFailure::BeforeTerminalCommit(format!(
                "Failed to prepare terminal result: {error}"
            ))
        })?;
        artifact
            .recipients
            .iter()
            .map(|recipient| {
                db.current_terminal_delivery_for_recipient(
                    &artifact.key,
                    recipient.player_id,
                    &recipient.pre_terminal_player_token,
                )
                .map_err(|error| {
                    TerminalPreparationFailure::AfterTerminalCommit(format!(
                        "Failed to load terminal delivery: {error}"
                    ))
                })?
                .map(|delivery| (recipient.player_id, delivery))
                .ok_or_else(|| {
                    TerminalPreparationFailure::AfterTerminalCommit(
                        "Prepared terminal delivery is missing".to_string(),
                    )
                })
            })
            .collect()
    })
    .await
    .map_err(|error| {
        TerminalPreparationFailure::AfterTerminalCommit(format!(
            "Terminal persistence task failed: {error}"
        ))
    })?
}

async fn prepare_full_terminal(
    game_db: &SharedGameDb,
    artifact: persistence::FullTerminalArtifact,
) -> Result<Vec<(PlayerId, server_core::CurrentTerminalDelivery)>, String> {
    prepare_full_terminal_with_commit_status(game_db, artifact)
        .await
        .map_err(TerminalPreparationFailure::message)
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

    {
        let mut mgr = draft_state.lock().await;
        let action = draft_core::types::DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        };
        match mgr.apply_system_action(&draft_code, action, None) {
            Ok(_) => {}
            Err(e) => {
                warn!(draft = %draft_code, error = %e, "failed to auto-report draft match result");
                return;
            }
        }
    };

    broadcast_draft_views(&draft_code, connections, draft_state).await;
}

/// Bind a spawned draft game to its exact Full lifetime before announcing it.
fn initialize_draft_match_runtime(
    game_manager: &mut SessionManager,
    game_db: &SharedGameDb,
    spawn: &DraftMatchSpawn,
) -> Result<server_core::FullSessionKey, String> {
    let full_key = game_db
        .create_full_session_key(&spawn.game_code)
        .map_err(|error| format!("Failed to bind draft match identity: {error}"))?;
    let session = game_manager
        .sessions
        .get_mut(&spawn.game_code)
        .ok_or_else(|| format!("Spawned draft match is missing: {}", spawn.game_code))?;
    // These seats have not attached yet. Keep their presence accurate without
    // starting reconnect expiry before an actual socket has disconnected.
    session.connected.fill(false);
    initialize_full_runtime(game_db, session, full_key.clone())?;

    Ok(full_key)
}

/// Build the initial match announcement from the draft seat's stable credential.
fn build_draft_match_start_message(
    draft_manager: &DraftSessionManager,
    draft_code: &str,
    spawn: &DraftMatchSpawn,
    full_key: &server_core::FullSessionKey,
    player: &DraftMatchPlayer,
    opponent_name: &str,
) -> Result<ServerMessage, String> {
    let draft_token = draft_manager
        .sessions
        .get(draft_code)
        .and_then(|session| session.player_tokens.get(usize::from(player.draft_seat)))
        .filter(|token| !token.is_empty())
        .cloned()
        .ok_or_else(|| "Draft match credential disappeared before announcement".to_string())?;

    Ok(ServerMessage::DraftMatchStart {
        match_id: spawn.match_id.clone(),
        round: spawn.round,
        game_code: spawn.game_code.clone(),
        full_key: full_key.clone(),
        player_token: draft_token,
        your_player: player.game_player,
        opponent_name: opponent_name.to_string(),
    })
}

/// Generate pairings and spawn 2-player game sessions for pending draft tables.
async fn maybe_spawn_draft_matches(
    draft_code: &str,
    draft_state: &SharedDraftState,
    game_state: &SharedState,
    db: &SharedDb,
    game_db: &SharedGameDb,
    connections: &SharedConnections,
) {
    let spawns: Vec<(DraftMatchSpawn, server_core::FullSessionKey)> = {
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
            Ok(spawned) => spawned
                .into_iter()
                .filter_map(|spawn| {
                    match initialize_draft_match_runtime(&mut game_mgr, game_db, &spawn) {
                        Ok(full_key) => Some((spawn, full_key)),
                        Err(error) => {
                            warn!(
                                draft = %draft_code,
                                match_id = %spawn.match_id,
                                game = %spawn.game_code,
                                %error,
                                "failed to initialize draft match"
                            );
                            game_mgr.remove_game(&spawn.game_code);
                            if let Some(session) = draft_mgr.sessions.get_mut(draft_code) {
                                session.active_matches.remove(&spawn.match_id);
                            }
                            None
                        }
                    }
                })
                .collect(),
            Err(e) => {
                warn!(draft = %draft_code, error = %e, "draft match spawn skipped");
                return;
            }
        }
    };

    if spawns.is_empty() {
        return;
    }

    // Reacquire draft state before the sender map so a reconnect cannot renew
    // its engine state between pairing generation and match-start delivery.
    // Draft reconnects use the same draft state -> connections ordering.
    let draft_manager = draft_state.lock().await;
    let conns = connections.lock().await;
    let Some(players) = conns.get(draft_code) else {
        return;
    };

    for (spawn, full_key) in spawns {
        info!(
            draft = %draft_code,
            match_id = %spawn.match_id,
            game = %spawn.game_code,
            "draft match game spawned"
        );
        for (player, seat) in [(&spawn.player_a, 0usize), (&spawn.player_b, 1usize)] {
            let msg = match build_draft_match_start_message(
                &draft_manager,
                draft_code,
                &spawn,
                &full_key,
                player,
                &spawn.opponent_names[seat],
            ) {
                Ok(message) => message,
                Err(error) => {
                    warn!(
                        draft = %draft_code,
                        match_id = %spawn.match_id,
                        seat = player.draft_seat,
                        %error
                    );
                    continue;
                }
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
    connections: &SharedConnections,
    draft_state: &SharedDraftState,
) {
    // Serialize sender fan-out with reconnect's state -> connections
    // transaction. Without this guard, a reconnect could mark its seat live
    // while a concurrent broadcast still selected its superseded sender.
    let draft_manager = draft_state.lock().await;
    let Some(session) = draft_manager.sessions.get(draft_code) else {
        return;
    };
    let conns = connections.lock().await;
    // Draft connections are stored under the draft_code in the connections map
    if let Some(players) = conns.get(draft_code) {
        for (pid, sender) in players.iter() {
            let seat = pid.0 as usize;
            if seat < session.player_tokens.len() {
                let msg = ServerMessage::DraftStateUpdate {
                    view: session.view_for_seat(seat),
                };
                let _ = sender.send(msg);
            }
        }
    }
}

async fn broadcast_draft_timer_sync(
    draft_code: &str,
    remaining_ms: u32,
    connections: &SharedConnections,
    draft_state: &SharedDraftState,
) {
    let msg = ServerMessage::DraftTimerSync { remaining_ms };
    let _draft_manager = draft_state.lock().await;
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

            broadcast_draft_timer_sync(
                &timer_draft_code,
                remaining_ms,
                &timer_connections,
                &timer_draft_state,
            )
            .await;

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
                    // CR 903.13b: the expired pick timer takes the kind's WHOLE
                    // pick step — one card for the four CR 905.1a kinds, two for
                    // CommanderDraft, dropping to the remainder on an odd final
                    // pick. Read from the procedure so this and the reducer's
                    // `expected` agree by construction. Was a hardcoded single
                    // id, which stalled a Commander pod at `WrongPickCardCount`.
                    // Same mechanism as `server-core`'s disconnected-seat
                    // auto-pick, which carries the full derivation.
                    let cards_per_pick =
                        usize::from(session.session.config.kind.procedure().cards_per_pick)
                            .min(pack.0.len());
                    let mut rng = rand::rng();
                    // Distinct ids: drawing twice by index into the pack could
                    // pick the same card twice, which the reducer rejects.
                    let mut remaining: Vec<String> =
                        pack.0.iter().map(|c| c.instance_id.clone()).collect();
                    let card_instance_ids: Vec<String> = (0..cards_per_pick)
                        .map(|_| remaining.swap_remove(rng.random_range(0..remaining.len())))
                        .collect();
                    let action = draft_core::types::DraftAction::Pick {
                        seat: seat_idx as u8,
                        card_instance_ids,
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

        drop(mgr);
        broadcast_draft_views(&timer_draft_code, &timer_connections, &timer_draft_state).await;

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
        // `apply_seat_delta` as `None` and `start_game` refuses the whole
        // table with `SeatDeckMissing`, naming a seat rather than the deck
        // that caused it. Validating here causes the reducer to return `Err`,
        // which phase-server then surfaces to the client.
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
    let (player_messages, spectator_msg, ai_failure) = {
        let mut mgr = state.lock().await;
        let Some(session) = mgr.sessions.get_mut(game_code) else {
            return;
        };

        let ai_failure = session.run_ai().fault;
        persist_full_session_async(game_db, session);
        (
            build_game_started_messages(session),
            build_spectator_game_started_message(session),
            ai_failure,
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

    match spectator_msg {
        Ok(spectator_msg) => {
            let mut specs = game_spectators.lock().await;
            if let Some(spectators) = specs.get_mut(game_code) {
                spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
                if spectators.is_empty() {
                    specs.remove(game_code);
                }
            }
        }
        Err(reason) => {
            warn!(game = %game_code, %reason, "skipping spectator GameStarted: snapshot too large");
        }
    }

    broadcast_ai_failure(connections, game_code, ai_failure).await;
}

async fn require_host(identity: &SocketIdentity, socket: &mut NegotiatedSocket) -> Result<(), ()> {
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
    socket: &mut NegotiatedSocket,
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
    // The SOURCE, not `config.set_code`. `set_code` is the whole-source display
    // label, which a multi-set pod joins into `"ISD+DKA+AVR"` — a string no pool
    // map can key on. The persisted core source is the single authority for
    // both Uniform and Chaos rebuilds; Chaos assignments were resolved once at
    // admission and are never drawn again here.
    let (source, pod_size, pack_count) = {
        let mgr = draft_state.lock().await;
        let session = mgr
            .sessions
            .get(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        (
            session.config.source.clone(),
            session.config.pod_size,
            session.config.pack_count,
        )
    };

    match source {
        draft_core::types::DraftSource::Set {
            layout: draft_core::types::SetLayout::UniformByRound { codes },
        } => draft_pools.generator_for_sequence(&codes),
        draft_core::types::DraftSource::Set {
            layout:
                draft_core::types::SetLayout::Chaos {
                    candidate_codes,
                    assignments,
                },
        } => draft_pools.generator_for_chaos(&candidate_codes, &assignments, pod_size, pack_count),
        draft_core::types::DraftSource::Cube { .. } => {
            Err("Server-hosted drafts require a set pool".to_string())
        }
    }
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
    full_key: &server_core::FullSessionKey,
    player_count: u8,
    eliminated: &[PlayerId],
    ai_results: &[RevisionedActionResult],
    rewind_targets: &[RewindOption],
) {
    let game_code = &full_key.game_code;
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
                    // CR 118.3: the STALENESS axis is the one part of disclosure
                    // the engine cannot self-gate — "is this the final transition
                    // of the fan-out" is a property of the transport's batch, not
                    // of any `GameState`. An intermediate transition advertises no
                    // actions, so it must advertise no read-out either. The
                    // `is_actor` half is redundant with the entry point's own gate
                    // and is kept only to mirror its `p_spell_costs` sibling above.
                    let p_activation_block_reasons = if is_last && is_actor {
                        engine_activation_block_reasons_for_viewer(ai_raw_state, *pid)
                    } else {
                        HashMap::new()
                    };
                    debug_assert!(
                        p_activation_block_reasons.is_empty() || is_actor,
                        "a non-empty CR 118.3 read-out implies an authorized viewer"
                    );
                    let _ = s.send(ServerMessage::StateUpdate {
                        full_key: Some(full_key.clone()),
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
                        activation_block_reasons: p_activation_block_reasons,
                        derived: derive_transport_views(ai_raw_state, pstate, Some(*pid)),
                        viewer_interaction: derive_viewer_interaction(ai_raw_state, pstate, *pid),
                        rewind_targets: rewind_targets.to_vec(),
                    });
                }
            }
        }
        let (ai_raw_state, ai_events, _, ai_log_entries, _, _, _) = result;
        if let Ok(spectator_msg) = build_spectator_state_update_message(
            full_key,
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

async fn broadcast_ai_failure(
    connections: &SharedConnections,
    game_code: &str,
    failure: Option<server_core::AiDriverFault>,
) {
    let Some(fault) = failure else {
        return;
    };

    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        for sender in players.values() {
            let _ = sender.send(ServerMessage::AiDriverFault {
                fault: fault.clone(),
            });
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
    full_key: &server_core::FullSessionKey,
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
                // CR 118.3 + CR 117.1: derived from `raw_state` (`snapshot.0`) —
                // the SAME binding the sibling `spell_costs` (`snapshot.3`) came
                // from. This path has no `session` in scope, and reading a
                // different state here is precisely the freshness hazard the
                // per-site state-binding rule exists to prevent.
                let p_activation_block_reasons =
                    engine_activation_block_reasons_for_viewer(&raw_state, *pid);
                debug_assert!(
                    p_activation_block_reasons.is_empty() || is_actor,
                    "a non-empty CR 118.3 read-out implies an authorized viewer"
                );
                let _ = s.send(ServerMessage::StateUpdate {
                    full_key: Some(full_key.clone()),
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
                    activation_block_reasons: p_activation_block_reasons,
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
        build_spectator_state_update_message(full_key, &raw_state, &[], &[], state_revision)
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

/// Maps engine and lifecycle session refusals onto their ordinary response
/// channels. Pending game operations route operational failures through their
/// own `*Failed` frames at the call site.
fn session_action_error_message(error: SessionActionError) -> ServerMessage {
    match error {
        SessionActionError::Rejected(rejection) => ServerMessage::ActionRejected { rejection },
        SessionActionError::RequestRejected(reason) => ServerMessage::RequestRejected { reason },
        SessionActionError::Operational(error) => ServerMessage::error(error),
    }
}

/// Keep an operational failure attached to the operation whose client promise
/// is waiting for it. Engine legality remains on the structured rejection
/// frames above; this function is only for failures outside the engine action
/// boundary.
fn operation_failed_message(msg: &ClientMessage, message: String) -> Option<ServerMessage> {
    match msg {
        ClientMessage::Action { .. }
        | ClientMessage::Interaction { .. }
        | ClientMessage::Concede => Some(ServerMessage::ActionFailed { message }),
        ClientMessage::PreviewManaPayment { request_id, .. } => {
            Some(ServerMessage::ManaPaymentPreviewFailed {
                request_id: *request_id,
                message,
            })
        }
        ClientMessage::ExportAuthoritativeState => {
            Some(ServerMessage::AuthoritativeStateExportFailed { message })
        }
        ClientMessage::PreviewInteraction { request } => {
            Some(ServerMessage::InteractionPreviewFailed {
                request_id: request.request_id.clone(),
                message,
            })
        }
        ClientMessage::ClientHello { .. }
        | ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::AbandonGame
        | ClientMessage::ConcedeMatch
        | ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::RequestTakeback(_)
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback
        | ClientMessage::BootstrapTerminalDelivery { .. }
        | ClientMessage::ReadTerminalResult { .. }
        | ClientMessage::AckTerminalDelivery { .. }
        | ClientMessage::CreateGameWithSettings { .. }
        | ClientMessage::JoinGameWithPassword { .. }
        | ClientMessage::LookupJoinTarget { .. }
        | ClientMessage::Emote { .. }
        | ClientMessage::SpectatorJoin { .. }
        | ClientMessage::Ping { .. }
        | ClientMessage::UpdateLobbyMetadata { .. }
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::UnregisterLobby { .. }
        | ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. }
        // No correlated `*Failed` frame: a tournament request that fails is
        // answered by the broker's own `Error` outbound, carrying the
        // manager's reason verbatim. `ActionFailed` is for the game-action
        // promise a client is awaiting, which none of these is.
        | ClientMessage::CreateTournament { .. }
        | ClientMessage::JoinTournament { .. }
        | ClientMessage::GetTournament { .. }
        | ClientMessage::StartTournamentRound { .. }
        | ClientMessage::ReportMatchResult { .. }
        | ClientMessage::DropFromTournament { .. }
        | ClientMessage::EndTournament { .. } => None,
    }
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
            GameSubmission::Action(action) => {
                guard_game_action_payload(action).map_err(|_reason| {
                    Box::new(ServerMessage::ActionRejected {
                        rejection: ActionRejection::new(ActionRejectionCode::InvalidAction),
                    })
                })
            }
            GameSubmission::Interaction(submission) => {
                guard_interaction_submission_payload(submission).map_err(|_reason| {
                    Box::new(ServerMessage::ActionRejected {
                        rejection: ActionRejection::new(
                            ActionRejectionCode::InteractionPayloadTooLarge,
                        ),
                    })
                })
            }
        }
    }
}

/// Answer one interaction preview to its requester alone.
///
/// Takes NO socket: the reply rides the connection's own `tx`, which shares the
/// one `biased` `tokio::select!` loop with `socket.recv()` on a single task, so
/// it reaches exactly this socket with no interleaving hazard. That is also why
/// this is a free function rather than an inline dispatch arm — `axum`'s
/// `WebSocket` has no test constructor, so a socket-taking handler could not be
/// driven from a test at all.
///
/// `connections` is read only through the socket-currency check; nothing is
/// broadcast, and no lock is held across the send.
async fn handle_preview_interaction(
    request: InteractionPreviewRequest,
    state: &SharedState,
    connections: &SharedConnections,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &SocketIdentity,
) {
    let request_id = request.request_id.clone();
    let response = match (identity.game_code.clone(), identity.player_token.clone()) {
        (Some(game_code), Some(player_token)) => {
            let mgr = state.lock().await;
            if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
                ServerMessage::InteractionPreviewFailed {
                    request_id,
                    message: FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
                }
            } else {
                // No handler-level payload re-guard, deliberately:
                // `guard_client_message_before_dispatch` already charged this
                // frame at the wire, and `preview_interaction` re-charges every
                // member and answers its own `PayloadTooLarge` INSIDE the
                // preview — a correlated, non-tearing answer on this request's
                // own channel, identical to what the local WASM seat gets.
                match mgr.preview_interaction_with_rejection(&game_code, &player_token, &request) {
                    Ok(preview) => ServerMessage::InteractionPreview { preview },
                    Err(PreviewRefusal::Rejected(rejection)) => {
                        ServerMessage::InteractionPreviewFailed {
                            request_id,
                            message: rejection.message,
                        }
                    }
                    Err(PreviewRefusal::Operational(error)) => {
                        ServerMessage::InteractionPreviewFailed {
                            request_id,
                            message: error,
                        }
                    }
                }
            }
        }
        _ => ServerMessage::InteractionPreviewFailed {
            request_id,
            message: "Not in a game".to_string(),
        },
    };

    let _ = tx.send(response);
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
    socket: &mut NegotiatedSocket,
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
            let msg = ServerMessage::ActionFailed {
                message: "Not in a game".to_string(),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
    };
    let player_token = match &identity.player_token {
        Some(t) => t.clone(),
        None => {
            let msg = ServerMessage::ActionFailed {
                message: "No player token".to_string(),
            };
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
        if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
            drop(mgr);
            let msg = ServerMessage::ActionFailed {
                message: FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        let applied = match submission {
            GameSubmission::Action(action) => mgr.handle_action_with_card_db_outcome(
                &game_code,
                &player_token,
                action,
                Some(db.as_ref()),
            ),
            GameSubmission::Interaction(submission) => {
                mgr.handle_interaction_with_rejection(&game_code, &player_token, submission)
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
                let ai_outcome = mgr
                    .sessions
                    .get_mut(&game_code)
                    .expect("handled action must retain its session")
                    .run_ai();
                let ai_failure = ai_outcome.fault;
                let ai_results = ai_outcome.transitions;
                let session = mgr.sessions.get(&game_code).unwrap();
                let eliminated = session.state.eliminated_players.clone();
                // Captured once, AFTER `run_ai`, and reused for both the human
                // and the AI fan-out below: the list is a live session
                // affordance rather than a per-snapshot projection, so the
                // freshest value under this lock is the correct one for every
                // message this transition produces.
                let rewind_targets = session.rewind_options();
                let player_count = session.player_count;
                let full_key = session
                    .full_runtime
                    .as_ref()
                    .expect("authenticated Full action retains its exact runtime")
                    .key
                    .clone();
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

                terminal
                    .map_err(SessionActionError::Operational)
                    .map(|terminal| {
                        (
                            human_revision,
                            human_result,
                            ai_results,
                            eliminated,
                            player_count,
                            game_over_winner,
                            terminal,
                            rewind_targets,
                            ai_failure,
                            full_key,
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
            ai_failure,
            full_key,
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
                let msg = ServerMessage::ActionFailed { message: reason };
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
                        let msg = ServerMessage::ActionFailed { message: error };
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
                            // CR 118.3: the STALENESS axis again — a human action
                            // followed by AI play must not advertise the pre-AI
                            // action set, so it must not advertise a read-out
                            // describing a board the player is not being offered
                            // actions on. `is_actor` mirrors the `p_spell_costs`
                            // sibling above and is redundant with the entry
                            // point's own gate.
                            let p_activation_block_reasons = if ai_results.is_empty() && is_actor {
                                engine_activation_block_reasons_for_viewer(&raw_state, *pid)
                            } else {
                                HashMap::new()
                            };
                            debug_assert!(
                                p_activation_block_reasons.is_empty() || is_actor,
                                "a non-empty CR 118.3 read-out implies an authorized viewer"
                            );
                            let _ = s.send(ServerMessage::StateUpdate {
                                full_key: Some(full_key.clone()),
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
                                activation_block_reasons: p_activation_block_reasons,
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
                &full_key,
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
                &full_key,
                player_count,
                &eliminated,
                &ai_results,
                &rewind_targets,
            )
            .await;

            broadcast_ai_failure(connections, &game_code, ai_failure).await;

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
        Err(error) => {
            let msg = match error {
                SessionActionError::Operational(message) => ServerMessage::ActionFailed { message },
                error => session_action_error_message(error),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
        }
    }
}

/// Handle the authenticated native Resolve All capability. Unlike ordinary
/// actions, this sends one compact final state snapshot and a requester-only
/// progress acknowledgement rather than replaying the entire batch event log.
#[cfg(any())]
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
        let msg = ServerMessage::ResolveAllFailed {
            request_id,
            message: "Not in a game".to_string(),
        };
        let _ = tx.send(msg);
        return;
    };

    let processed = {
        let mut mgr = state.lock().await;
        if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
            Err(SessionActionError::Operational(
                FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
            ))
        } else {
            match mgr.resolve_all_for_player_with_rejection(
                &game_code,
                &player_token,
                max_resolutions,
            ) {
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
                        let ai_outcome = session.run_ai();
                        let ai_failure = ai_outcome.fault;
                        let (raw_state, legal_actions, _auto_pass, spell_costs, by_object) =
                            session.current_broadcast_snapshot();
                        let revision = session.state_revision;
                        let log_entries =
                            resolve_all_log_tail(&batch_log_entries, &ai_outcome.transitions);
                        let eliminated = session.state.eliminated_players.clone();
                        let rewind_targets = session.rewind_options();
                        let player_count = session.player_count;
                        let game_over_winner = match &session.state.waiting_for {
                            engine::types::game_state::WaitingFor::GameOver { winner } => {
                                Some(*winner)
                            }
                            _ => None,
                        };
                        let terminal = if let Some(winner) = game_over_winner {
                            let ranked_result = ranked_duel_players(session).and_then(|players| {
                                ranked_result_for_duel(game_db, &game_code, &players, winner)
                            });
                            terminal_artifact(
                                session,
                                winner,
                                "Game ended".to_string(),
                                ranked_result,
                            )
                            .map(Some)
                        } else {
                            persist_full_session_async(game_db, session);
                            Ok(None)
                        };
                        terminal
                            .map_err(SessionActionError::Operational)
                            .map(|terminal| {
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
                                        ai_failure,
                                    )),
                                )
                            })
                    }
                },
                Err(error) => Err(error),
            }
        }
    };

    let (summary, payload) = match processed {
        Ok(processed) => processed,
        Err(SessionActionError::Rejected(rejection)) => {
            let _ = tx.send(ServerMessage::ResolveAllRejected {
                request_id,
                rejection,
            });
            return;
        }
        Err(SessionActionError::RequestRejected(reason)) => {
            let _ = tx.send(ServerMessage::RequestRejected { reason });
            return;
        }
        Err(SessionActionError::Operational(error)) => {
            let _ = tx.send(ServerMessage::ResolveAllFailed {
                request_id,
                message: error,
            });
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
        ai_failure,
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

    broadcast_ai_failure(connections, &game_code, ai_failure).await;

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
    socket: &mut NegotiatedSocket,
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
    context: &ServerContext,
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
            socket.set_wire_format(info.wire_format);
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
        let reason = reason.to_string();
        let msg = operation_failed_message(&client_msg, reason.clone())
            .unwrap_or_else(|| ServerMessage::error(reason));
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

    if matches!(mode, ServerMode::Full) {
        if let Some(reason) =
            full_socket_authority_rejection(&client_msg, state, connections, identity, tx).await
        {
            let msg = operation_failed_message(&client_msg, reason.to_string())
                .unwrap_or_else(|| ServerMessage::error(reason.to_string()));
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }

        if let Some(reason) = draft_socket_admission_rejection(&client_msg, identity) {
            let msg = ServerMessage::DraftActionRejected {
                reason: reason.to_string(),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
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
            // The only capacity check on this path, and deliberately so: it
            // holds `mgr` through the insert below. Checking before
            // `resolve_deck` instead would release the lock in between and let
            // every create that raced into that window past a stale count.
            if mgr.sessions.len() >= context.limits.max_games {
                drop(mgr);
                warn!(
                    limit = context.limits.max_games,
                    "max games reached, rejecting CreateGame"
                );
                context
                    .metrics
                    .record_reject(metrics::RejectReason::GameLimit);
                let msg = ServerMessage::error(
                    "Server is at game capacity, please try again later".to_string(),
                );
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            // Seat 0's provenance, not just its resolved deck: a restart
            // rebuilds `decks` from `deck_choices` alone, and seat 0 is
            // immutable, so a host seat recorded without its submitted list
            // restores empty and no reseat can ever refill it.
            let (game_code, player_token) =
                mgr.create_game(resolved, Some(DeckChoice::DeckList(Box::new(deck))));
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
            drop(mgr);

            if let Err(error) = attach_full_seat(
                state,
                connections,
                identity,
                game_code.clone(),
                player_token.clone(),
                tx,
            )
            .await
            {
                let msg = ServerMessage::error(error);
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

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
            // Same provenance record as `JoinGameWithPassword` below: a seat
            // filled without its submitted list restores empty on restart.
            match mgr.join_game_with_name_and_reservation(
                &game_code,
                resolved,
                Some(DeckChoice::DeckList(Box::new(deck))),
                String::new(),
                None,
            ) {
                Ok((player_token, _filtered_state)) => {
                    mgr.set_card_names(&game_code, db.card_names());
                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    let joiner = session.player_for_token(&player_token).unwrap();
                    let started_messages = if session.is_full() {
                        let ai_failure = session.run_ai().fault;
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
                        Some((joiner_msg, build_game_started_messages(session), ai_failure))
                    } else {
                        None
                    };
                    info!(game = %game_code, player = ?joiner, "player joined");
                    drop(mgr);

                    if let Err(error) = attach_full_seat(
                        state,
                        connections,
                        identity,
                        game_code.clone(),
                        player_token.clone(),
                        tx,
                    )
                    .await
                    {
                        let msg = ServerMessage::error(error);
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }

                    // Only send GameStarted when the game is full (all seats claimed)
                    if let Some((msg, other_messages, ai_failure)) = started_messages {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Clone recipient handles while the map is locked, then
                        // send after releasing it so no connection lock crosses
                        // a socket write or fan-out.
                        let (other_sends, fault_senders) = {
                            let conns = connections.lock().await;
                            let Some(players) = conns.get(&game_code) else {
                                return;
                            };
                            let other_sends = other_messages
                                .into_iter()
                                .filter(|(pid, _)| *pid != joiner)
                                .filter_map(|(pid, msg)| {
                                    players.get(&pid).cloned().map(|sender| (sender, msg))
                                })
                                .collect::<Vec<_>>();
                            let fault_senders = players.values().cloned().collect::<Vec<_>>();
                            (other_sends, fault_senders)
                        };
                        for (sender, msg) in other_sends {
                            let _ = sender.send(msg);
                        }
                        if let Some(fault) = ai_failure {
                            for sender in fault_senders {
                                let _ = sender.send(ServerMessage::AiDriverFault {
                                    fault: fault.clone(),
                                });
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
                    if guard_game_action_payload(&action).is_err() {
                        ServerMessage::ManaPaymentPreviewRejected {
                            request_id,
                            rejection: ActionRejection::new(ActionRejectionCode::InvalidAction),
                        }
                    } else {
                        let mgr = state.lock().await;
                        if !full_socket_is_current_while_state_locked(
                            &mgr,
                            connections,
                            identity,
                            tx,
                        )
                        .await
                        {
                            ServerMessage::ManaPaymentPreviewFailed {
                                request_id,
                                message: FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
                            }
                        } else {
                            match mgr.preview_mana_payment_with_rejection(
                                &game_code,
                                &player_token,
                                &action,
                            ) {
                                Ok(source_ids) => ServerMessage::ManaPaymentPreview {
                                    request_id,
                                    source_ids,
                                },
                                Err(PreviewRefusal::Rejected(rejection)) => {
                                    ServerMessage::ManaPaymentPreviewRejected {
                                        request_id,
                                        rejection,
                                    }
                                }
                                Err(PreviewRefusal::Operational(error)) => {
                                    ServerMessage::ManaPaymentPreviewFailed {
                                        request_id,
                                        message: error,
                                    }
                                }
                            }
                        }
                    }
                }
                _ => ServerMessage::ManaPaymentPreviewFailed {
                    request_id,
                    message: "Not in a game".to_string(),
                },
            };

            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::ExportAuthoritativeState => {
            // The native P2P host is always Player 1 (seat zero). Its local
            // server deliberately redacts normal broadcast snapshots per
            // viewer, but the host needs an unredacted snapshot to report an
            // engine stall. The export is a trusted engine envelope, not a
            // PersistedSession, so reconnect tokens and server metadata never
            // leave the server.
            let response = match (identity.game_code.as_deref(), identity.player_id) {
                (Some(game_code), Some(PlayerId(0))) => {
                    let mut manager = state.lock().await;
                    match manager.sessions.get_mut(game_code) {
                        Some(session) if session.hosting == HostingMode::SingleUser => {
                            let mut snapshot = session.state.clone();
                            snapshot.capture_rng_word_pos();
                            match serde_json::to_string(&TrustedGameStateEnvelope::capture(
                                snapshot,
                            )) {
                                Ok(state) => ServerMessage::AuthoritativeStateExport { state },
                                Err(error) => ServerMessage::AuthoritativeStateExportFailed {
                                    message: format!(
                                        "Failed to serialize authoritative game state: {error}"
                                    ),
                                },
                            }
                        }
                        Some(_) => ServerMessage::AuthoritativeStateExportFailed {
                            message:
                                "Authoritative state export is available only from a local host"
                                    .to_string(),
                        },
                        None => ServerMessage::AuthoritativeStateExportFailed {
                            message: "Game session is no longer available".to_string(),
                        },
                    }
                }
                (Some(_), Some(_)) => ServerMessage::AuthoritativeStateExportFailed {
                    message: "Only the game host can export authoritative state".to_string(),
                },
                _ => ServerMessage::AuthoritativeStateExportFailed {
                    message: "Not in a game".to_string(),
                },
            };

            if let Ok(json) = serde_json::to_string(&response) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::PreviewInteraction { request } => {
            handle_preview_interaction(request, state, connections, tx, identity).await;
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
                    ai_failure: Option<server_core::AiDriverFault>,
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
                if identity.full_seat().is_some()
                    && !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx)
                        .await
                {
                    ReconnectOutcome::Err(FULL_SOCKET_AUTHORITY_REJECTION.to_string())
                } else {
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
                                install_full_sender_while_state_locked(
                                    connections,
                                    &game_code,
                                    player,
                                    tx,
                                )
                                .await;
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
                                let ai_outcome = session.run_ai();
                                let ai_failure = ai_outcome.fault;
                                let ai_result =
                                    ai_outcome.transitions.last().cloned().map(Box::new);
                                if ai_result.is_some() || ai_failure.is_some() {
                                    persist_full_session_async(game_db, session);
                                }
                                // Reconnect: no contest dice (the player must not
                                // re-see the first-player roll).
                                let game_started_msg =
                                    build_game_started_message(session, player, None, Vec::new());
                                let pending_takeback_msg =
                                    session.pending_takeback_message().map(Box::new);
                                let rewind_targets = session.rewind_options();
                                install_full_sender_while_state_locked(
                                    connections,
                                    &game_code,
                                    player,
                                    tx,
                                )
                                .await;
                                ReconnectOutcome::InGame {
                                    player,
                                    game_started_msg: Box::new(game_started_msg),
                                    ai_result,
                                    ai_failure,
                                    pending_takeback_msg,
                                    rewind_targets,
                                }
                            }
                            Err(e) => ReconnectOutcome::Err(e),
                        }
                    }
                }
            }; // lock dropped

            match outcome {
                ReconnectOutcome::HostingOk { player, slot_info } => {
                    info!(game = %game_code, player = ?player, "hosting reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token.clone());

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
                    send_player_slots(std::iter::once((&player, tx)), &slot_info);
                }

                ReconnectOutcome::InGame {
                    player,
                    game_started_msg,
                    ai_result,
                    ai_failure,
                    pending_takeback_msg,
                    rewind_targets,
                } => {
                    info!(game = %game_code, player = ?player, "reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token);

                    let reconnect_senders = {
                        let conns = connections.lock().await;
                        conns
                            .get(&game_code)
                            .into_iter()
                            .flat_map(|players| players.iter())
                            .filter(|(pid, _)| **pid != player)
                            .map(|(_, sender)| sender.clone())
                            .collect::<Vec<_>>()
                    };
                    let reconnect_msg = ServerMessage::OpponentReconnected {
                        full_key: Some(full_key.clone()),
                        player: Some(player),
                    };
                    for sender in reconnect_senders {
                        let _ = sender.send(reconnect_msg.clone());
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
                        let ai_recipients = {
                            let conns = connections.lock().await;
                            conns
                                .get(&game_code)
                                .into_iter()
                                .flat_map(|players| players.iter())
                                .filter(|(pid, _)| **pid != player)
                                .map(|(pid, sender)| (*pid, sender.clone()))
                                .collect::<Vec<_>>()
                        };
                        for (pid, sender) in ai_recipients {
                            if let Ok(msg) = build_state_update_message(
                                &result,
                                state_revision,
                                &full_key,
                                pid,
                                rewind_targets.clone(),
                            ) {
                                let _ = sender.send(msg);
                            }
                        }
                    }

                    broadcast_ai_failure(connections, &game_code, ai_failure).await;
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
                // Server-hosted constructed play has no draft behind it, so
                // the EMPTY set-code list is the accurate answer here, and it
                // means constructed play — not a placeholder for a value this
                // path could have supplied.
                if let Err(reasons) = validate_name_deck_for_format_full(
                    db,
                    &deck.main_deck,
                    &deck.sideboard,
                    &deck.commander,
                    &deck.companion,
                    &deck.planar_deck,
                    &deck.scheme_deck,
                    &deck.signature_spell,
                    &[],
                    fc,
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

            let ai_requests = match ai_seat_setups(
                db,
                &ai_seats,
                pc,
                format_config.as_ref(),
                match_config.match_type,
            ) {
                Ok(setups) => setups,
                Err(reason) => {
                    let msg = ServerMessage::error(reason);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            if !ai_requests.is_empty() && ai_requests.len() as u8 == pc - 1 {
                // --- AI game path: create, start, and run initial AI actions ---
                let (game_code, player_token, full_key, game_started_msg, ai_failure) = {
                    let mut mgr = state.lock().await;
                    // Sole capacity check for the AI path, under the lock that
                    // inserts — see the `CreateGame` arm for why it cannot move
                    // ahead of deck resolution.
                    if mgr.sessions.len() >= context.limits.max_games {
                        warn!(
                            limit = context.limits.max_games,
                            "max games reached, rejecting CreateGameWithSettings"
                        );
                        context
                            .metrics
                            .record_reject(metrics::RejectReason::GameLimit);
                        let _ = tx.send(ServerMessage::error(
                            "Server is at game capacity, please try again later".to_string(),
                        ));
                        return;
                    }
                    let (game_code, player_token) = match mgr.create_game_with_ai(
                        resolved,
                        DeckChoice::DeckList(Box::new(deck)),
                        display_name.clone(),
                        timer_seconds,
                        match_config,
                        ai_requests,
                        db.card_names(),
                        format_config.clone(),
                        db,
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
                    if let Err(error) = initialize_full_runtime(game_db, session, full_key.clone())
                    {
                        mgr.remove_game(&game_code);
                        let _ = tx.send(ServerMessage::error(error));
                        return;
                    }
                    let ai_failure = session.run_ai().fault;
                    persist_full_session_async(game_db, session);
                    // Initial start of a Play-vs-AI game: the human seat sees
                    // the first-player contest dice. Drain so they are not
                    // re-sent on reconnect.
                    let start_events = std::mem::take(&mut session.start_events);
                    let game_started_msg =
                        build_game_started_message(session, PlayerId(0), None, start_events);

                    (
                        game_code,
                        player_token,
                        full_key,
                        game_started_msg,
                        ai_failure,
                    )
                }; // lock dropped

                if let Err(error) = attach_full_seat(
                    state,
                    connections,
                    identity,
                    game_code.clone(),
                    player_token.clone(),
                    tx,
                )
                .await
                {
                    let msg = ServerMessage::error(error);
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
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
                if let Some(fault) = ai_failure {
                    let _ = tx.send(ServerMessage::AiDriverFault { fault });
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
                let (game_code, player_token, host_player, initial_player_count, full_key) =
                    match create_and_connect_multiplayer_session(
                        state,
                        connections,
                        game_db,
                        MultiplayerSessionRequest {
                            resolved,
                            host_choice: DeckChoice::DeckList(Box::new(deck)),
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
                            context: context.clone(),
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

                // `create_and_connect_multiplayer_session` installs the host
                // sender before returning, so local identity is intentionally
                // recorded only after the sender-map authority exists.
                identity.set_session(game_code.clone(), host_player, player_token.clone());

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
                    player_id: host_player,
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

            // Collects a refused-start message to broadcast after the state lock releases and
            // after the joiner receives their direct error (mirrors the seat-delta path).
            let mut start_error_broadcast: Option<String> = None;

            let join_outcome = {
                let mut mgr = state.lock().await;
                match mgr.join_game_with_name_and_reservation(
                    &game_code,
                    resolved,
                    Some(DeckChoice::DeckList(Box::new(deck))),
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
                        let started = should_start
                            && match session.start_game(db) {
                                Ok(()) => true,
                                Err(start_err) => {
                                    // A refused start writes nothing: `start_game` returns
                                    // ahead of its first mutation on both Err arms, and
                                    // `apply_seat_delta` withholds the reducer's started
                                    // flag. So the room is still pregame and the joiner —
                                    // already holding a token, deck and display name — is
                                    // a waiting player, indistinguishable from one who
                                    // joined a room that was not yet full.
                                    //
                                    // Whether the refusal can be cleared depends on the
                                    // seat it names: seat 0 is `SeatImmutable`, and only
                                    // `SeatKind::Ai` carries a rewritable deck, so a
                                    // human seat's deck is fixed at join.
                                    //
                                    // The message is fanned out to the whole room after
                                    // the state lock releases (mirrors the seat-delta
                                    // path). `Display` carries the cEDH prefix on that
                                    // arm, so the bracket message is unchanged while a
                                    // missing-deck refusal reports itself honestly.
                                    start_error_broadcast = Some(start_err.to_string());
                                    false
                                }
                            };
                        persist_full_session_async(game_db, session);
                        if started {
                            Ok(JoinOutcome::Started {
                                player_token,
                                joiner,
                                public_before,
                            })
                        } else {
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
                    let attached_player = match attach_full_seat(
                        state,
                        connections,
                        identity,
                        game_code.clone(),
                        player_token.clone(),
                        tx,
                    )
                    .await
                    {
                        Ok(player) => player,
                        Err(error) => {
                            let msg = ServerMessage::error(error);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    };
                    debug_assert_eq!(joiner, attached_player);

                    let attached = ServerMessage::SessionAttached {
                        game_code: game_code.clone(),
                        player_id: joiner,
                        player_token,
                        full_key: Some(full_key.clone()),
                    };
                    if let Ok(json) = serde_json::to_string(&attached) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Notify all connected players about the updated room state
                    let slot_senders = {
                        let conns = connections.lock().await;
                        conns
                            .get(&game_code)
                            .map(|players| {
                                players
                                    .iter()
                                    .map(|(pid, sender)| (*pid, sender.clone()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };
                    send_player_slots(
                        slot_senders.iter().map(|(pid, sender)| (pid, sender)),
                        &slot_info,
                    );

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
                        full_key: Some(full_key),
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
                        // CR 117.1b: the game has not started, so no player has
                        // priority and no activated ability can be activated —
                        // the read-out has nothing to describe. Empty by the
                        // same existing design as its two siblings above.
                        activation_block_reasons: HashMap::new(),
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
                    let attached_player = match attach_full_seat(
                        state,
                        connections,
                        identity,
                        game_code.clone(),
                        player_token,
                        tx,
                    )
                    .await
                    {
                        Ok(player) => player,
                        Err(error) => {
                            let msg = ServerMessage::error(error);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    };
                    debug_assert_eq!(joiner, attached_player);

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

            // If the auto-start was refused, fan the error out to every player
            // connected to the room. The joiner is one of them: `attach_full_seat`
            // on the `JoinOutcome::Waiting` arm registered their sender, so this is
            // the single delivery of the refusal to them — the room-wide message and
            // their own notice are the same message, not a duplicate pair.
            if let Some(err_msg) = start_error_broadcast {
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

            // Removing a started session is the in-memory terminal commit. It
            // happens under the same authority transaction as the host check,
            // before terminal persistence performs database I/O. A reconnect
            // that arrives after this point therefore cannot replace the
            // committing socket and then observe a database-retired runtime.
            enum AbandonCommit {
                Terminal(persistence::FullTerminalArtifact, Box<GameSession>),
                Unstarted,
                Missing,
            }

            let commit = {
                let mut mgr = state.lock().await;
                if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await
                {
                    drop(mgr);
                    let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                match mgr.sessions.get(&game_code) {
                    Some(session) if session.game_started => {
                        match terminal_artifact(session, None, "Game abandoned".to_string(), None) {
                            Ok(artifact) => {
                                let removed = mgr
                                    .remove_game(&game_code)
                                    .expect("started session was present while committing abandon");
                                AbandonCommit::Terminal(artifact, Box::new(removed))
                            }
                            Err(error) => {
                                let _ = tx.send(ServerMessage::error(error));
                                return;
                            }
                        }
                    }
                    Some(_) => AbandonCommit::Unstarted,
                    None => AbandonCommit::Missing,
                }
            };
            let (terminal_deliveries, committed_started_session) = match commit {
                AbandonCommit::Terminal(artifact, removed) => {
                    let terminal_key = artifact.key.clone();
                    let deliveries = match prepare_full_terminal_with_commit_status(
                        game_db, artifact,
                    )
                    .await
                    {
                        Ok(deliveries) => deliveries,
                        Err(TerminalPreparationFailure::BeforeTerminalCommit(error)) => {
                            // A failed prepare call is not enough to prove the
                            // transaction rolled back: a commit error can be
                            // indeterminate. Restore only after the database
                            // still confirms this exact Full key is active.
                            let still_active = matches!(
                                game_db.load_active_full_key(&game_code),
                                Ok(Some(active_key)) if active_key == terminal_key
                            );
                            let recovered = if still_active {
                                let mut mgr = state.lock().await;
                                if mgr.sessions.contains_key(&game_code) {
                                    false
                                } else {
                                    mgr.restore_session(*removed);
                                    true
                                }
                            } else {
                                false
                            };
                            error!(game = %game_code, %error, "terminal preparation failed");
                            if recovered {
                                let _ = tx.send(ServerMessage::error(error));
                                return;
                            }
                            connections.lock().await.remove(&game_code);
                            game_spectators.lock().await.remove(&game_code);
                            delist_and_announce(
                                lobby,
                                lobby_subscribers,
                                std::iter::once(game_code.as_str()),
                            )
                            .await;
                            error!(game = %game_code, "terminal preparation did not leave this Full key active; retaining in-memory retirement");
                            let _ = tx.send(ServerMessage::error(error));
                            return;
                        }
                        Err(TerminalPreparationFailure::AfterTerminalCommit(error)) => {
                            // The database has already retired the active runtime.
                            // Keep the in-memory session retired as well, then
                            // clean up stale routes before reporting the delivery
                            // read failure to the committing client.
                            connections.lock().await.remove(&game_code);
                            game_spectators.lock().await.remove(&game_code);
                            delist_and_announce(
                                lobby,
                                lobby_subscribers,
                                std::iter::once(game_code.as_str()),
                            )
                            .await;
                            error!(game = %game_code, %error, "terminal delivery preparation failed after terminal commit");
                            let _ = tx.send(ServerMessage::error(error));
                            return;
                        }
                    };
                    (deliveries, true)
                }
                AbandonCommit::Unstarted => (Vec::new(), false),
                AbandonCommit::Missing => {
                    let msg = ServerMessage::GameAbandoned {
                        game_code: game_code.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let removed = if committed_started_session {
                None
            } else {
                let mut mgr = state.lock().await;
                if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await
                {
                    drop(mgr);
                    let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                mgr.remove_game(&game_code)
            };
            if let Some(session) = removed.filter(|session| !session.game_started) {
                retire_unstarted_session_async(game_db, &session);
            }

            if !terminal_deliveries.is_empty() {
                let terminal_sends = {
                    let conns = connections.lock().await;
                    let mut sends = Vec::new();
                    if let Some(players) = conns.get(&game_code) {
                        for (player, delivery) in &terminal_deliveries {
                            if let Some(sender) = players.get(player) {
                                sends.push((
                                    sender.clone(),
                                    ServerMessage::TerminalResult {
                                        delivery: Some(delivery.clone()),
                                    },
                                ));
                            }
                        }
                    }
                    sends
                };
                for (sender, message) in terminal_sends {
                    let _ = sender.send(message);
                }
            }

            connections.lock().await.remove(&game_code);
            game_spectators.lock().await.remove(&game_code);
            delist_and_announce(
                lobby,
                lobby_subscribers,
                std::iter::once(game_code.as_str()),
            )
            .await;

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
                    let msg = ServerMessage::ActionFailed {
                        message: "Not in a game".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            info!(game = %game_code, player = ?player_id, "player conceded game");
            let outcome = {
                let mut mgr = state.lock().await;
                if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await
                {
                    Err(SessionActionError::Operational(
                        FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
                    ))
                } else {
                    match mgr.handle_action_with_card_db_outcome(
                        &game_code,
                        &player_token,
                        engine::types::actions::GameAction::Concede { player_id },
                        None,
                    ) {
                        Ok(result) => {
                            let session = mgr
                                .sessions
                                .get_mut(&game_code)
                                .expect("handled concession must retain its session");
                            let revision = session.advance_state_revision();
                            let winner = match &session.state.waiting_for {
                                engine::types::game_state::WaitingFor::GameOver { winner } => {
                                    *winner
                                }
                                _ => None,
                            };
                            let terminal = if matches!(
                                &session.state.waiting_for,
                                engine::types::game_state::WaitingFor::GameOver { .. }
                            ) {
                                let ranked_result =
                                    ranked_duel_players(session).and_then(|players| {
                                        ranked_result_for_duel(
                                            game_db, &game_code, &players, winner,
                                        )
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
                            let full_key = session
                                .full_runtime
                                .as_ref()
                                .expect("authenticated concession retains its exact runtime")
                                .key
                                .clone();
                            terminal
                                .map_err(SessionActionError::Operational)
                                .map(|terminal| {
                                    (revision, result, winner, terminal, rewind_targets, full_key)
                                })
                        }
                        Err(error) => Err(error),
                    }
                }
            };

            match outcome {
                Err(error) => {
                    let msg = match error {
                        SessionActionError::Operational(message) => {
                            ServerMessage::ActionFailed { message }
                        }
                        error => session_action_error_message(error),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok((revision, result, winner, terminal, rewind_targets, full_key)) => {
                    let terminal_deliveries = match terminal {
                        Some(artifact) => match prepare_full_terminal(game_db, artifact).await {
                            Ok(deliveries) => deliveries,
                            Err(error) => {
                                error!(game = %game_code, %error, "terminal preparation failed");
                                let _ = tx.send(ServerMessage::ActionFailed { message: error });
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
                                &full_key,
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
                if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await
                {
                    Err(SessionActionError::Operational(
                        FULL_SOCKET_AUTHORITY_REJECTION.to_string(),
                    ))
                } else {
                    match mgr.handle_match_concede_outcome(&game_code, &player_token) {
                        Ok((revision, result)) => {
                            let winner = match &result.0.waiting_for {
                                engine::types::game_state::WaitingFor::GameOver { winner } => {
                                    *winner
                                }
                                _ => None,
                            };
                            let session = mgr
                                .sessions
                                .get(&game_code)
                                .expect("handled match concession must retain its session");
                            let ranked_result = winner.and_then(|winner| {
                                ranked_duel_players(session).and_then(|players| {
                                    ranked_result_for_duel(
                                        game_db,
                                        &game_code,
                                        &players,
                                        Some(winner),
                                    )
                                })
                            });
                            let rewind_targets = session.rewind_options();
                            let full_key = session
                                .full_runtime
                                .as_ref()
                                .expect("authenticated concession retains its exact runtime")
                                .key
                                .clone();
                            terminal_artifact(
                                session,
                                winner,
                                "Match conceded".to_string(),
                                ranked_result,
                            )
                            .map(|terminal| {
                                (revision, result, winner, terminal, rewind_targets, full_key)
                            })
                            .map_err(SessionActionError::Operational)
                        }
                        Err(error) => Err(error),
                    }
                }
            };

            match outcome {
                Err(error) => {
                    if let Ok(json) = serde_json::to_string(&session_action_error_message(error)) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Ok((revision, result, winner, terminal, rewind_targets, full_key)) => {
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
                                &full_key,
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
            if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
                drop(mgr);
                let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
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
            let (ai_results, ai_failure) = if approved_snapshot.is_some() {
                let ai_outcome = session.run_ai();
                (ai_outcome.transitions, ai_outcome.fault)
            } else {
                (Vec::new(), None)
            };
            // Both read AFTER `run_ai`, matching the shipped action path: an AI
            // follow-up can cross a turn (new rewind boundary) or finish a
            // player off (new elimination), and the AI fan-out below must not
            // describe the state as it stood before its own results.
            // `snapshot.0` is the *pre*-`run_ai` rollback state, so sourcing
            // eliminations from it would be exactly that staleness.
            let rewind_targets = session.rewind_options();
            let eliminated = session.state.eliminated_players.clone();
            let full_key = session
                .full_runtime
                .as_ref()
                .expect("authenticated takeback retains its exact runtime")
                .key
                .clone();
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
                    let msg = ServerMessage::RequestRejected { reason };
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
                        &full_key,
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
                        &full_key,
                        player_count,
                        &eliminated,
                        &ai_results,
                        &rewind_targets,
                    )
                    .await;
                    broadcast_ai_failure(connections, &game_code, ai_failure).await;
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
            if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
                drop(mgr);
                let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
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
            let (ai_results, ai_failure) = if approved_snapshot.is_some() {
                let ai_outcome = session.run_ai();
                (ai_outcome.transitions, ai_outcome.fault)
            } else {
                (Vec::new(), None)
            };
            // Read AFTER `run_ai` for the same reason as the `RequestTakeback`
            // arm above.
            let rewind_targets = session.rewind_options();
            let eliminated = session.state.eliminated_players.clone();
            let full_key = session
                .full_runtime
                .as_ref()
                .expect("authenticated takeback retains its exact runtime")
                .key
                .clone();
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
                    let msg = ServerMessage::RequestRejected { reason };
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
                        &full_key,
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
                        &full_key,
                        player_count,
                        &eliminated,
                        &ai_results,
                        &rewind_targets,
                    )
                    .await;
                    broadcast_ai_failure(connections, &game_code, ai_failure).await;
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
            if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await {
                drop(mgr);
                let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
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
                    let msg = ServerMessage::RequestRejected { reason };
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

            // Linearize the emote against replacement attachment while state
            // remains held, then fan out only after both locks are released.
            let recipients = {
                let manager = state.lock().await;
                if !full_socket_is_current_while_state_locked(&manager, connections, identity, tx)
                    .await
                {
                    return;
                }
                let conns = connections.lock().await;
                conns
                    .get(&game_code)
                    .into_iter()
                    .flat_map(|players| players.iter())
                    .filter(|(pid, _)| **pid != player_id)
                    .map(|(_, sender)| sender.clone())
                    .collect::<Vec<_>>()
            };
            for sender in recipients {
                let _ = sender.send(msg.clone());
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
                start_error,
            ) = {
                let mut mgr = state.lock().await;
                if !full_socket_is_current_while_state_locked(&mgr, connections, identity, tx).await
                {
                    drop(mgr);
                    let msg = ServerMessage::error(FULL_SOCKET_AUTHORITY_REJECTION.to_string());
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
                let Some(session) = mgr.sessions.get_mut(&game_code) else {
                    let msg = ServerMessage::error(format!("Game not found: {game_code}"));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                };

                if let Some(fault) = session.ai_driver_fault() {
                    let msg = ServerMessage::error(format!(
                        "Native AI driver fault {}: {}",
                        fault.id,
                        fault.cause.message()
                    ));
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

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
                // Collect a refused-start message to broadcast after releasing the state lock.
                // `start_game` mutates nothing on Err, and `apply_seat_delta` above no longer
                // commits the reducer's started flag, so a refusal leaves the room editable.
                let start_error: Option<String> = if started {
                    match session.start_game(db) {
                        Ok(()) => None,
                        Err(start_err) => {
                            started = false;
                            Some(start_err.to_string())
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
                    start_error,
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

                    // If the start was refused, notify all players.
                    if let Some(ref err_msg) = start_error {
                        let msg = ServerMessage::error(err_msg.clone());
                        for sender in players.values() {
                            let _ = sender.send(msg.clone());
                        }
                    }

                    send_player_slots(players.iter(), &slot_info);
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
            source,
            set_codes,
            kind,
            public,
            password,
            timer_seconds,
            tournament_format,
            pod_policy,
            pod_size,
        } => {
            let source_intent = match resolve_draft_source_intent(source, set_codes) {
                Ok(intent) => intent,
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };
            let source_codes = source_intent.set_codes();
            info!(
                display_name = %display_name,
                source = ?source_intent,
                kind = ?kind,
                public,
                pod_size,
                "CreateDraftWithSettings"
            );

            if let Err(reason) = guard_create_draft_with_settings(
                &display_name,
                source_codes,
                &password,
                timer_seconds,
                pod_size,
                kind,
                tournament_format,
            ) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let procedure = kind.procedure();
            let resolved_source = match source_intent {
                // A Uniform sequence names the booster set for each round. A
                // short sequence retains its legacy repeat-final shorthand;
                // a longer one would silently name boosters that never open.
                server_core::protocol::DraftSourceIntent::Uniform { set_codes } => {
                    if set_codes.len() > usize::from(procedure.packs_per_player) {
                        Err(format!(
                            "{kind:?} opens {} packs, but {} sets were named",
                            procedure.packs_per_player,
                            set_codes.len()
                        ))
                    } else {
                        draft_pools.generator_for_sequence(&set_codes).and_then(|_| {
                            let first = set_codes
                                .first()
                                .ok_or_else(|| "A pod must name at least one set".to_string())?;
                            draft_pools
                                .pool_for_set(first)
                                .and_then(|pool| pool.cards_per_pack())
                                .ok_or_else(|| {
                                    format!(
                                        "Set {first} has no single MTGJSON pack size across its booster variants"
                                    )
                                })
                                .map(|cards_per_pack| {
                                    (
                                        draft_core::types::DraftSource::Set {
                                            layout: draft_core::types::SetLayout::UniformByRound {
                                                codes: set_codes,
                                            },
                                        },
                                        cards_per_pack,
                                    )
                                })
                        })
                    }
                }
                // The OS entropy draw is made once at admission. The persisted
                // core layout holds the result, so reconnect/start never reroll
                // a pod and a client never transmits assignments.
                server_core::protocol::DraftSourceIntent::Chaos { candidate_codes } => {
                    let seed = rand::rngs::OsRng.try_next_u64().map_err(|error| {
                        format!("Unable to seed Chaos draft assignments: {error}")
                    });
                    seed.and_then(|seed| {
                        draft_pools
                            .resolve_chaos_layout(
                                &candidate_codes,
                                pod_size,
                                procedure.packs_per_player,
                                seed,
                            )
                            .map(|(layout, cards_per_pack)| {
                                (
                                    draft_core::types::DraftSource::Set { layout },
                                    cards_per_pack,
                                )
                            })
                    })
                }
            };
            let (source, cards_per_pack) = match resolved_source {
                Ok(resolved) => resolved,
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            if !draft_pod_size_is_allowed(kind, tournament_format, pod_size) {
                let msg = ServerMessage::DraftActionRejected {
                    reason: "Pod size is not allowed by the draft procedure".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            // The session label names only the resolved core source. The lobby
            // row uses `lobby_set_label` below so a Chaos listing advertises
            // candidate intent without leaking the assignment union.
            let set_label = source.set_code();
            let lobby_set_label = server_core::persist::draft_lobby_source_label(&source);
            let config = draft_core::types::DraftConfig {
                set_code: set_label.clone(),
                source,
                kind,
                pod_size,
                // A SET-POOL property, not a `DraftProcedure` axis, and now read
                // off the pool itself rather than hardcoded to 14 — the pods
                // this creates open boosters of the size MTGJSON declares.
                cards_per_pack,
                pack_count: procedure.packs_per_player,
                min_deck_size: procedure.min_deck_size,
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
                            set_code: lobby_set_label,
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
                if !draft_socket_is_current_while_state_locked(&mgr, connections, identity, tx)
                    .await
                {
                    Err(DRAFT_SOCKET_AUTHORITY_REJECTION.to_string())
                } else {
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
                    result.map(|_| should_rearm_timer)
                }
            };

            match result {
                Ok(should_rearm_timer) => {
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
                    broadcast_draft_views(&draft_code, connections, draft_state).await;

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

                    maybe_spawn_draft_matches(
                        &draft_code,
                        draft_state,
                        state,
                        db,
                        game_db,
                        connections,
                    )
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

            if let Some((attached_code, _attached_seat, attached_token)) = identity.draft_seat() {
                if attached_code != draft_code
                    || attached_token != player_token
                    || !draft_socket_is_current_preflight(draft_state, connections, identity, tx)
                        .await
                {
                    let msg = ServerMessage::DraftActionRejected {
                        reason: DRAFT_SOCKET_AUTHORITY_REJECTION.to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            let result = reconnect_draft_seat(
                draft_state,
                connections,
                identity,
                draft_code.clone(),
                player_token,
                tx,
            )
            .await;

            match result {
                Ok(view) => {
                    match attach_current_draft_match(
                        draft_state,
                        state,
                        connections,
                        identity,
                        &draft_code,
                        tx,
                    )
                    .await
                    {
                        Ok(Some(attachment)) => {
                            for message in [attachment.match_start, attachment.game_started] {
                                if let Ok(json) = serde_json::to_string(&message) {
                                    let _ = socket.send(Message::text(json)).await;
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(reason) => {
                            let msg = ServerMessage::DraftActionRejected { reason };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
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

        // Every tournament variant routes straight to the broker, which owns
        // the whole surface: token authority, the `TournamentManager` call,
        // and outbound assembly. Mode-agnostic, matching `reject_if_disabled`
        // — there is no Full-mode alternative path for any of them, so unlike
        // `CreateGameWithSettings` these need no per-mode branch here.
        ClientMessage::CreateTournament { .. }
        | ClientMessage::JoinTournament { .. }
        | ClientMessage::GetTournament { .. }
        | ClientMessage::StartTournamentRound { .. }
        | ClientMessage::ReportMatchResult { .. }
        | ClientMessage::DropFromTournament { .. }
        | ClientMessage::EndTournament { .. } => {
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
    use engine::types::ability::SearchSelectionConstraint;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{
        ActiveSearchDecisionAuthority, ActiveSearchDecisionControl, PriorityPassingMode, WaitingFor,
    };
    use engine::types::identifiers::ObjectId;
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
        let full_key = server_core::FullSessionKey {
            game_code: "ABC123".to_string(),
            generation: 1,
        };
        match build_state_update_message(result, 1, &full_key, viewer, Vec::new())
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

    #[cfg(any())]
    mod legacy_resolve_all_transport_tests {
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

        #[cfg(any())]
        #[tokio::test]
        async fn resolve_all_handler_sends_the_final_snapshot_before_its_acknowledgement() {
            let mut manager = SessionManager::new();
            let (game_code, player_token) = manager.create_game(PlayerDeckPayload::default(), None);
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
                GameAction::BeginResolveAll {
                    max_resolutions: 1,
                    scope: ResolveAllScope::Shared,
                },
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
                persistence::GameDb::open(
                    db_file.path(),
                    persistence::SessionRetention::Multiplayer,
                )
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
                lobby_organized_tournaments: Vec::new(),
                lobby_joined_tournaments: Vec::new(),
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

        #[tokio::test]
        async fn resolve_all_limit_rejection_keeps_request_correlation() {
            let mut manager = SessionManager::new();
            let (game_code, player_token) = manager.create_game(PlayerDeckPayload::default(), None);
            let state: SharedState = Arc::new(Mutex::new(manager));
            let draft_state: SharedDraftState = Arc::new(Mutex::new(DraftSessionManager::new()));
            let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
            let game_spectators: SharedGameSpectators = Arc::new(Mutex::new(HashMap::new()));
            let db_file = tempfile::NamedTempFile::new().expect("temporary game database");
            let game_db = Arc::new(
                persistence::GameDb::open(
                    db_file.path(),
                    persistence::SessionRetention::Multiplayer,
                )
                .expect("open temporary game database"),
            );
            let (requester_tx, mut requester_rx) = mpsc::unbounded_channel();
            let identity = SocketIdentity {
                game_code: Some(game_code),
                player_id: Some(PlayerId(0)),
                player_token: Some(player_token),
                lobby_subscribed: false,
                session_span: None,
                client_hello: None,
                lobby_host_game: None,
                seat_reservations: Vec::new(),
                lobby_reservations: Vec::new(),
                lobby_organized_tournaments: Vec::new(),
                lobby_joined_tournaments: Vec::new(),
                draft_code: None,
                draft_seat: None,
                draft_token: None,
                spectator_draft_code: None,
                spectator_visibility: None,
                spectator_game_code: None,
            };

            handle_resolve_all(
                73,
                5_001,
                &state,
                &draft_state,
                &connections,
                &requester_tx,
                &game_db,
                &game_spectators,
                &identity,
            )
            .await;

            assert!(matches!(
                requester_rx
                    .recv()
                    .await
                    .expect("limit rejection keeps the requester channel open"),
                ServerMessage::ResolveAllRejected {
                    request_id: 73,
                    rejection,
                } if rejection.code == ActionRejectionCode::InvalidAction
            ));
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
mod interaction_preview_route_tests {
    use super::*;
    use engine::game::deck_loading::PlayerDeckPayload;
    use engine::types::interaction::{
        InteractionAvailability, InteractionPreview, InteractionPreviewRequest,
        InteractionPreviewStatus, InteractionSubmission, PreviewRequestId,
    };
    use server_core::filter_state_for_player;
    use server_core::{AiDriverFailure, AiDriverFault};
    use tokio::sync::mpsc::error::TryRecvError;

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
            lobby_organized_tournaments: Vec::new(),
            lobby_joined_tournaments: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
            spectator_game_code: None,
        }
    }

    /// The witness is minted by `derive_viewer_interaction`, never hand-built:
    /// a fabricated id would be indistinguishable from a stale one.
    fn live_witness(
        manager: &SessionManager,
        game_code: &str,
        token0: &str,
        token1: &str,
    ) -> (String, String, InteractionSubmission) {
        let state = &manager.sessions.get(game_code).expect("session").state;
        for (player, token, other) in [(PlayerId(0), token0, token1), (PlayerId(1), token1, token0)]
        {
            let filtered = filter_state_for_player(state, player);
            let view = derive_viewer_interaction(state, &filtered, player);
            if let InteractionAvailability::ProgressAvailable { witness } = view.availability {
                return (token.to_string(), other.to_string(), witness);
            }
        }
        panic!("a started session must publish a progress witness for some seat");
    }

    /// Two seats, both senders installed through the production
    /// `attach_full_seat`, plus a live engine-minted preview request for
    /// whichever seat holds the capability.
    async fn two_seat_table() -> (
        SharedState,
        SharedConnections,
        String,
        InteractionPreviewRequest,
        (
            SocketIdentity,
            mpsc::UnboundedSender<ServerMessage>,
            mpsc::UnboundedReceiver<ServerMessage>,
        ),
        (
            SocketIdentity,
            mpsc::UnboundedSender<ServerMessage>,
            mpsc::UnboundedReceiver<ServerMessage>,
        ),
    ) {
        let mut manager = SessionManager::new();
        let (game_code, token0) = manager.create_game(PlayerDeckPayload::default(), None);
        let (token1, _) = manager
            .join_game(&game_code, PlayerDeckPayload::default(), None)
            .expect("the second seat joins and starts the game");
        let (acting_token, other_token, witness) =
            live_witness(&manager, &game_code, &token0, &token1);

        let request = InteractionPreviewRequest {
            request_id: PreviewRequestId("req-1".to_string()),
            interaction_id: witness.interaction_id.clone(),
            response: witness.response.clone(),
        };

        let state: SharedState = Arc::new(Mutex::new(manager));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));

        let (requester_tx, requester_rx) = mpsc::unbounded_channel();
        let mut requester_identity = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut requester_identity,
            game_code.clone(),
            acting_token,
            &requester_tx,
        )
        .await
        .expect("the requesting seat attaches");

        let (other_tx, other_rx) = mpsc::unbounded_channel();
        let mut other_identity = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut other_identity,
            game_code.clone(),
            other_token,
            &other_tx,
        )
        .await
        .expect("the second seat attaches");

        (
            state,
            connections,
            game_code,
            request,
            (requester_identity, requester_tx, requester_rx),
            (other_identity, other_tx, other_rx),
        )
    }

    fn fault() -> AiDriverFault {
        AiDriverFault {
            id: 1,
            after_state_revision: 0,
            cause: AiDriverFailure::ActionSafetyCapReached { limit: 1 },
        }
    }

    /// Row 1. Fan the answer out through `connections` and the negative below
    /// fails: the other seat's `rx` yields a frame.
    #[tokio::test]
    async fn preview_answers_the_requester_and_no_other_seat() {
        let (
            state,
            connections,
            game_code,
            request,
            (requester_identity, requester_tx, mut requester_rx),
            (_other_identity, _other_tx, mut other_rx),
        ) = two_seat_table().await;

        handle_preview_interaction(
            request.clone(),
            &state,
            &connections,
            &requester_tx,
            &requester_identity,
        )
        .await;

        // Positive (i): the route ran. An unreached route yields an empty channel,
        // which would also satisfy the negative below.
        let preview: InteractionPreview = match requester_rx
            .try_recv()
            .expect("the requesting seat is answered")
        {
            ServerMessage::InteractionPreview { preview } => preview,
            other => panic!("the requester must receive a preview, got {other:?}"),
        };
        assert!(
            matches!(preview.status, InteractionPreviewStatus::Confirmable),
            "the answer must reach the reducer simulation: {:?}",
            preview.status
        );
        assert_eq!(preview.request_id, request.request_id);
        assert_eq!(preview.interaction_id, request.interaction_id);
        assert!(matches!(requester_rx.try_recv(), Err(TryRecvError::Empty)));

        // The negative.
        assert!(
            matches!(other_rx.try_recv(), Err(TryRecvError::Empty)),
            "a preview must reach no seat but the requester"
        );

        // Positive (ii): the instrument is alive. The landed production fan-out
        // over the SAME `connections` map fills the SAME receiver, so a dead
        // channel cannot pass both legs of this row.
        broadcast_ai_failure(&connections, &game_code, Some(fault())).await;
        assert!(
            matches!(other_rx.try_recv(), Ok(ServerMessage::AiDriverFault { .. })),
            "the second seat's channel is reachable by a real fan-out"
        );
    }

    /// Row 1's sibling: the socket-currency check refuses a sender that is not
    /// the one registered for this seat, and answers on the correlated channel.
    #[tokio::test]
    async fn preview_from_a_superseded_socket_is_refused() {
        let (
            state,
            connections,
            _game_code,
            request,
            (requester_identity, _requester_tx, mut requester_rx),
            (_other_identity, _other_tx, _other_rx),
        ) = two_seat_table().await;

        let (stale_tx, mut stale_rx) = mpsc::unbounded_channel();
        handle_preview_interaction(
            request.clone(),
            &state,
            &connections,
            &stale_tx,
            &requester_identity,
        )
        .await;

        match stale_rx.try_recv().expect("the stale socket is answered") {
            ServerMessage::InteractionPreviewFailed {
                request_id,
                message,
            } => {
                assert_eq!(request_id, request.request_id);
                assert_eq!(message, FULL_SOCKET_AUTHORITY_REJECTION);
            }
            other => panic!("expected a correlated authority refusal, got {other:?}"),
        }
        assert!(
            matches!(requester_rx.try_recv(), Err(TryRecvError::Empty)),
            "a refused stale socket must not leak an answer to the live seat"
        );
    }

    /// A socket with no attached seat never reaches the session at all, and is
    /// still answered on the correlated channel rather than left hanging.
    #[tokio::test]
    async fn preview_before_joining_is_refused_on_the_correlated_channel() {
        let (state, connections, _game_code, request, _requester, _other) = two_seat_table().await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_preview_interaction(
            request.clone(),
            &state,
            &connections,
            &tx,
            &empty_identity(),
        )
        .await;

        match rx.try_recv().expect("a pre-join preview is answered") {
            ServerMessage::InteractionPreviewFailed {
                request_id,
                message,
            } => {
                assert_eq!(request_id, request.request_id);
                assert_eq!(message, "Not in a game");
            }
            other => panic!("expected a correlated refusal, got {other:?}"),
        }
    }

    /// Row 11's transport half: a session-level refusal reaches the client as a
    /// correlated failure carrying the session's own reason, not as an `Error`
    /// (which the native client treats as a session teardown).
    #[tokio::test]
    async fn an_interlocked_session_answers_a_correlated_failure_not_an_error() {
        let (
            state,
            connections,
            game_code,
            request,
            (requester_identity, requester_tx, mut requester_rx),
            (_other_identity, _other_tx, _other_rx),
        ) = two_seat_table().await;

        {
            let mut manager = state.lock().await;
            let session = manager.sessions.get_mut(&game_code).expect("session");
            session.ai_driver_fault = Some(fault());
        }

        handle_preview_interaction(
            request.clone(),
            &state,
            &connections,
            &requester_tx,
            &requester_identity,
        )
        .await;

        match requester_rx.try_recv().expect("the requester is answered") {
            ServerMessage::InteractionPreviewFailed {
                request_id,
                message,
            } => {
                assert_eq!(request_id, request.request_id);
                assert!(
                    message.contains("Native AI driver fault"),
                    "the session's own reason must ride the correlated channel: {message}"
                );
            }
            other => panic!("an interlock must not tear the session down: {other:?}"),
        }

        // Reach guard: with the interlock cleared the identical request is
        // answered, so the failure above is the interlock and not a broken table.
        {
            let mut manager = state.lock().await;
            manager
                .sessions
                .get_mut(&game_code)
                .expect("session")
                .ai_driver_fault = None;
        }
        handle_preview_interaction(
            request,
            &state,
            &connections,
            &requester_tx,
            &requester_identity,
        )
        .await;
        assert!(matches!(
            requester_rx.try_recv(),
            Ok(ServerMessage::InteractionPreview { .. })
        ));
    }

    /// Row 5's non-compiler-enforced leg. `to_lobby_client_message` carries
    /// `_ => return None`, so no compile error would catch a preview leaking
    /// into a broker clone. `SubscribeLobby` is the in-test positive: without
    /// it a wholesale-`None` regression would satisfy this.
    #[test]
    fn preview_is_never_projected_into_the_lobby_broker() {
        let frame = ClientMessage::PreviewInteraction {
            request: InteractionPreviewRequest {
                request_id: PreviewRequestId("req-1".to_string()),
                interaction_id: engine::types::interaction::InteractionId("i-1".to_string()),
                response: engine::types::interaction::InteractionResponse::Choose {
                    choice_id: engine::types::interaction::InteractionChoiceId("a".to_string()),
                },
            },
        };
        assert!(to_lobby_client_message(&frame).is_none());
        assert!(to_lobby_client_message(&ClientMessage::SubscribeLobby).is_some());
    }

    /// Row 5's three compiler-enforced legs, asserted by value rather than by
    /// "it compiled": the preview is classified exactly as `Interaction` is at
    /// each, and its operational failure is CORRELATED where `Interaction`'s is
    /// not — an uncorrelated `ActionFailed` would leave the pending promise
    /// hanging. Move it to `reject_if_disabled`'s always-allowed group and the
    /// LobbyOnly leg fails; give it `Independent` and the authority leg fails.
    #[test]
    fn preview_is_classified_exactly_as_its_submission_sibling() {
        let request = InteractionPreviewRequest {
            request_id: PreviewRequestId("req-1".to_string()),
            interaction_id: engine::types::interaction::InteractionId("i-1".to_string()),
            response: engine::types::interaction::InteractionResponse::Choose {
                choice_id: engine::types::interaction::InteractionChoiceId("a".to_string()),
            },
        };
        let preview = ClientMessage::PreviewInteraction {
            request: request.clone(),
        };
        let interaction = ClientMessage::Interaction {
            submission: InteractionSubmission {
                interaction_id: request.interaction_id.clone(),
                response: request.response.clone(),
            },
        };

        assert!(reject_if_disabled(&preview, ServerMode::Full).is_none());
        assert!(reject_if_disabled(&preview, ServerMode::LobbyOnly).is_some());
        assert_eq!(
            reject_if_disabled(&preview, ServerMode::LobbyOnly),
            reject_if_disabled(&interaction, ServerMode::LobbyOnly)
        );

        assert_eq!(
            full_socket_authority(&preview),
            FullSocketAuthority::CurrentSeat
        );
        assert_eq!(
            full_socket_authority(&preview),
            full_socket_authority(&interaction)
        );
        // Non-vacuity for the equality above: not every variant is CurrentSeat.
        assert_eq!(
            full_socket_authority(&ClientMessage::Ping { timestamp: 1 }),
            FullSocketAuthority::Independent
        );

        match operation_failed_message(&preview, "disabled".to_string()) {
            Some(ServerMessage::InteractionPreviewFailed {
                request_id,
                message,
            }) => {
                assert_eq!(request_id, request.request_id);
                assert_eq!(message, "disabled");
            }
            other => panic!("a preview failure must stay correlated, got {other:?}"),
        }
        assert!(matches!(
            operation_failed_message(&interaction, "disabled".to_string()),
            Some(ServerMessage::ActionFailed { .. })
        ));
    }
}

#[cfg(test)]
mod full_socket_authority_tests {
    use super::*;
    use engine::game::deck_loading::PlayerDeckPayload;
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
            lobby_organized_tournaments: Vec::new(),
            lobby_joined_tournaments: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
            spectator_game_code: None,
        }
    }

    fn test_session() -> (SharedState, SharedConnections, String, String) {
        let mut manager = SessionManager::new();
        let (game_code, player_token) = manager.create_game(PlayerDeckPayload::default(), None);
        (
            Arc::new(Mutex::new(manager)),
            Arc::new(Mutex::new(HashMap::new())),
            game_code,
            player_token,
        )
    }

    fn reconnect(game_code: &str, player_token: &str) -> ClientMessage {
        ClientMessage::Reconnect {
            game_code: game_code.to_string(),
            player_token: player_token.to_string(),
            full_key: server_core::FullSessionKey {
                game_code: game_code.to_string(),
                generation: 1,
            },
        }
    }

    #[test]
    fn full_socket_policy_is_explicit_for_attachment_classes() {
        assert_eq!(
            full_socket_authority(&ClientMessage::CreateGame {
                deck: DeckData::default(),
            }),
            FullSocketAuthority::FreshSocket
        );
        assert_eq!(
            full_socket_authority(&ClientMessage::LookupJoinTarget {
                game_code: "ROOM01".to_string(),
                password: None,
                reserve: true,
                display_name: Some("Guest".to_string()),
                release_reservation_token: None,
            }),
            FullSocketAuthority::FreshSocket
        );
        assert_eq!(
            full_socket_authority(&ClientMessage::Action {
                action: GameAction::PassPriority,
            }),
            FullSocketAuthority::CurrentSeat
        );
        assert_eq!(
            full_socket_authority(&ClientMessage::Ping { timestamp: 1 }),
            FullSocketAuthority::Independent
        );
    }

    #[tokio::test]
    async fn fresh_socket_reaches_full_create_join_and_reconnect_validation() {
        let (state, connections, game_code, player_token) = test_session();
        let (tx, _rx) = mpsc::unbounded_channel();
        let fresh = empty_identity();
        let messages = [
            ClientMessage::CreateGame {
                deck: DeckData::default(),
            },
            ClientMessage::JoinGame {
                game_code: game_code.clone(),
                deck: DeckData::default(),
            },
            reconnect(&game_code, &player_token),
        ];
        for message in messages {
            assert_eq!(
                full_socket_authority_rejection(&message, &state, &connections, &fresh, &tx).await,
                None,
                "fresh socket must reach {message:?}'s existing handler validation"
            );
        }
    }

    #[tokio::test]
    async fn stale_socket_cannot_act_reconnect_or_start_another_full_session() {
        let (state, connections, game_code, player_token) = test_session();
        let (a_tx, _a_rx) = mpsc::unbounded_channel();
        let (b_tx, _b_rx) = mpsc::unbounded_channel();
        let mut stale_a = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut stale_a,
            game_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .expect("attach A");
        let mut current_b = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut current_b,
            game_code.clone(),
            player_token.clone(),
            &b_tx,
        )
        .await
        .expect("replace A with B");

        let blocked = [
            ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            reconnect(&game_code, &player_token),
            ClientMessage::LookupJoinTarget {
                game_code: game_code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest".to_string()),
                release_reservation_token: None,
            },
            ClientMessage::CreateGame {
                deck: DeckData::default(),
            },
            ClientMessage::JoinGame {
                game_code: game_code.clone(),
                deck: DeckData::default(),
            },
        ];
        for message in blocked {
            assert!(
                full_socket_authority_rejection(&message, &state, &connections, &stale_a, &a_tx,)
                    .await
                    .is_some(),
                "stale socket must be fenced before {message:?} reaches its handler"
            );
        }
        assert!(full_socket_is_current_preflight(&state, &connections, &current_b, &b_tx).await);
    }

    #[tokio::test]
    async fn mutation_gate_rechecks_after_a_preflight_race() {
        let (state, connections, game_code, player_token) = test_session();
        let (a_tx, _a_rx) = mpsc::unbounded_channel();
        let (b_tx, _b_rx) = mpsc::unbounded_channel();
        let mut identity_a = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut identity_a,
            game_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .unwrap();
        assert!(full_socket_is_current_preflight(&state, &connections, &identity_a, &a_tx).await);

        let mut identity_b = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut identity_b,
            game_code,
            player_token,
            &b_tx,
        )
        .await
        .unwrap();

        let manager = state.lock().await;
        assert!(
            !full_socket_is_current_while_state_locked(&manager, &connections, &identity_a, &a_tx,)
                .await,
            "the state-mutation gate must reject a socket replaced after preflight"
        );
    }

    #[tokio::test]
    async fn stale_close_leaves_replacement_seat_connected() {
        let (state, connections, game_code, player_token) = test_session();
        let (a_tx, _a_rx) = mpsc::unbounded_channel();
        let (b_tx, _b_rx) = mpsc::unbounded_channel();
        let mut stale_a = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut stale_a,
            game_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .unwrap();
        let mut current_b = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut current_b,
            game_code.clone(),
            player_token,
            &b_tx,
        )
        .await
        .unwrap();

        disconnect_full_seat_if_current(&state, &connections, &stale_a, &a_tx).await;

        assert!(full_socket_is_current_preflight(&state, &connections, &current_b, &b_tx).await);
        let manager = state.lock().await;
        assert!(
            manager
                .sessions
                .get(&game_code)
                .expect("session remains present")
                .connected[0],
            "a stale close must not mark B's seat disconnected"
        );
    }

    #[tokio::test]
    async fn attached_socket_cannot_reconnect_another_valid_seat() {
        let (state, connections, game_x, token_x) = test_session();
        let (game_y, token_y) = {
            let mut manager = state.lock().await;
            manager.create_game(PlayerDeckPayload::default(), None)
        };
        let (x_tx, _x_rx) = mpsc::unbounded_channel();
        let mut identity_x = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut identity_x,
            game_x.clone(),
            token_x,
            &x_tx,
        )
        .await
        .unwrap();

        assert_eq!(
            full_socket_authority_rejection(
                &reconnect(&game_y, &token_y),
                &state,
                &connections,
                &identity_x,
                &x_tx,
            )
            .await,
            Some(FULL_SOCKET_AUTHORITY_REJECTION)
        );
        let conns = connections.lock().await;
        assert!(conns.contains_key(&game_x));
        assert!(
            !conns.contains_key(&game_y),
            "Y must not gain an orphan sender"
        );
    }

    #[tokio::test]
    async fn current_socket_may_reconnect_its_own_resolved_seat() {
        let (state, connections, game_code, player_token) = test_session();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut identity = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut identity,
            game_code.clone(),
            player_token.clone(),
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(
            full_socket_authority_rejection(
                &reconnect(&game_code, &player_token),
                &state,
                &connections,
                &identity,
                &tx,
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn fresh_socket_may_reconnect_a_session_restored_from_persistence() {
        let db_file = tempfile::NamedTempFile::new().expect("temporary game database");
        let game_db = Arc::new(
            persistence::GameDb::open(db_file.path(), persistence::SessionRetention::Multiplayer)
                .expect("open game database"),
        );
        let mut original_manager = SessionManager::new();
        let (game_code, player_token) =
            original_manager.create_game(PlayerDeckPayload::default(), None);
        {
            let session = original_manager
                .sessions
                .get_mut(&game_code)
                .expect("new session");
            let key = game_db
                .create_full_session_key(&game_code)
                .expect("allocate Full key");
            initialize_full_runtime(&game_db, session, key).expect("persist Full session");
        }
        let persisted = game_db
            .load_active_full_sessions()
            .expect("read persisted session")
            .pop()
            .expect("persisted Full session");
        let json = serde_json::to_string(&persisted.persisted).expect("serialize persisted state");
        let mut restored = restore_persisted_session(&json, Arc::new(CardDatabase::default()))
            .expect("restore persisted Full session");
        restored.full_runtime = Some(FullRuntime {
            key: persisted.key.clone(),
            activation_epoch: persisted.activation_epoch,
        });
        let mut restored_manager = SessionManager::new();
        restored_manager.restore_session(restored);
        let state: SharedState = Arc::new(Mutex::new(restored_manager));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel();

        assert_eq!(
            full_socket_authority_rejection(
                &ClientMessage::Reconnect {
                    game_code,
                    player_token: player_token.clone(),
                    full_key: persisted.key.clone(),
                },
                &state,
                &connections,
                &empty_identity(),
                &tx,
            )
            .await,
            None,
            "a fresh socket must retain reconnect eligibility after real persistence restore"
        );
        state
            .lock()
            .await
            .handle_reconnect(&persisted.key.game_code, &player_token)
            .expect("restored Full session accepts its persisted token");
        let mut fresh_identity = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut fresh_identity,
            persisted.key.game_code.clone(),
            player_token,
            &tx,
        )
        .await
        .expect("fresh restored reconnect installs its sender");
        assert!(full_socket_is_current_preflight(&state, &connections, &fresh_identity, &tx).await);
    }

    #[tokio::test]
    async fn authority_checks_and_replacement_close_complete_without_lock_cycle() {
        let (state, connections, game_code, player_token) = test_session();
        let (a_tx, _a_rx) = mpsc::unbounded_channel();
        let (b_tx, _b_rx) = mpsc::unbounded_channel();
        let mut identity_a = empty_identity();
        attach_full_seat(
            &state,
            &connections,
            &mut identity_a,
            game_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .unwrap();

        let state_for_close = state.clone();
        let connections_for_close = connections.clone();
        let state_for_attach = state.clone();
        let connections_for_attach = connections.clone();
        tokio::time::timeout(Duration::from_secs(2), async move {
            let close = tokio::spawn(async move {
                disconnect_full_seat_if_current(
                    &state_for_close,
                    &connections_for_close,
                    &identity_a,
                    &a_tx,
                )
                .await;
            });
            let attach = tokio::spawn(async move {
                let mut identity_b = empty_identity();
                attach_full_seat(
                    &state_for_attach,
                    &connections_for_attach,
                    &mut identity_b,
                    game_code,
                    player_token,
                    &b_tx,
                )
                .await
                .expect("replacement attach");
            });
            close.await.expect("close task");
            attach.await.expect("attach task");
        })
        .await
        .expect("state -> connections ordering must not deadlock");
    }
}

#[cfg(test)]
mod draft_socket_authority_tests {
    use super::*;
    use draft_core::types::{
        DeckAddableCards, DraftConfig, DraftKind, DraftPairing, DraftSource, DraftStatus,
        PairingStatus, PodPolicy, SetLayout, SpectatorVisibility, TournamentFormat,
    };
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

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
            lobby_organized_tournaments: Vec::new(),
            lobby_joined_tournaments: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
            spectator_game_code: None,
        }
    }

    fn test_draft() -> (SharedDraftState, SharedConnections, String, String) {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let mut manager = DraftSessionManager::new();
        let (draft_code, player_token, _) = manager.create_draft(config, "Alice".to_string());
        (
            Arc::new(Mutex::new(manager)),
            Arc::new(Mutex::new(HashMap::new())),
            draft_code,
            player_token,
        )
    }

    fn draft_admission_messages(draft_code: &str) -> [ClientMessage; 2] {
        [
            ClientMessage::CreateDraftWithSettings {
                display_name: "Alice".to_string(),
                source: None,
                set_codes: Some(vec!["TST".to_string()]),
                kind: DraftKind::Premier,
                public: false,
                password: None,
                timer_seconds: Some(75),
                tournament_format: TournamentFormat::Swiss,
                pod_policy: PodPolicy::Competitive,
                pod_size: 8,
            },
            ClientMessage::JoinDraftWithPassword {
                draft_code: draft_code.to_string(),
                display_name: "Bob".to_string(),
                password: None,
            },
        ]
    }

    #[tokio::test]
    async fn initial_draft_match_token_attaches_the_current_full_match() {
        use super::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};

        let (url, server, _temp_dir, app_state) = spawn_full_mode_server().await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let (draft_code, draft_token, expected_key, initial_match_start) = {
                let mut drafts = app_state.draft_sessions.lock().await;
                let config = DraftConfig {
                    source: DraftSource::single_set("TST".to_string()),
                    set_code: "TST".to_string(),
                    kind: DraftKind::Premier,
                    pod_size: 8,
                    cards_per_pack: 14,
                    pack_count: 3,
                    min_deck_size: 40,
                    addable_cards: DeckAddableCards::standard_basics(),
                    rng_seed: 42,
                    tournament_format: TournamentFormat::Swiss,
                    pod_policy: PodPolicy::Competitive,
                    spectator_visibility: SpectatorVisibility::default(),
                };
                let (draft_code, draft_token, _) = drafts.create_draft(config, "Alice".to_string());
                let mut games = app_state.sessions.lock().await;
                // Any synthetic disconnect created by initialization would
                // expire before this fixture's first socket attachment.
                games.reconnect.grace_period = Duration::ZERO;
                let (game_code, _host_token) = games
                    .create_game_n_players(
                        engine::game::deck_loading::PlayerDeckPayload::default(),
                        None,
                        "Alice".to_string(),
                        None,
                        2,
                        Default::default(),
                        None,
                    )
                    .expect("create draft match game");
                games
                    .join_game_with_name(
                        &game_code,
                        engine::game::deck_loading::PlayerDeckPayload::default(),
                        None,
                        "Bob".to_string(),
                    )
                    .expect("join draft match game");
                games
                    .sessions
                    .get_mut(&game_code)
                    .expect("draft match game exists")
                    .game_started = true;

                let draft = drafts.sessions.get_mut(&draft_code).expect("draft exists");
                draft.session.status = DraftStatus::MatchInProgress;
                draft.session.current_round = 1;
                draft.session.pairings.push(DraftPairing {
                    round: 1,
                    table: 0,
                    players: [PlayerId(0), PlayerId(1)],
                    match_id: "r1-t0".to_string(),
                    status: PairingStatus::Pending,
                    winner: None,
                });
                draft
                    .active_matches
                    .insert("r1-t0".to_string(), game_code.clone());
                let spawn = DraftMatchSpawn {
                    match_id: "r1-t0".to_string(),
                    round: 1,
                    game_code,
                    player_a: server_core::draft_session::DraftMatchPlayer {
                        draft_seat: 0,
                        game_player: PlayerId(0),
                    },
                    player_b: server_core::draft_session::DraftMatchPlayer {
                        draft_seat: 1,
                        game_player: PlayerId(1),
                    },
                    opponent_names: ["Bob".to_string(), "Alice".to_string()],
                };
                let key = initialize_draft_match_runtime(&mut games, &app_state.game_db, &spawn)
                    .expect("bind draft match runtime");
                let initial_match_start = build_draft_match_start_message(
                    &drafts,
                    &draft_code,
                    &spawn,
                    &key,
                    &spawn.player_a,
                    &spawn.opponent_names[0],
                )
                .expect("build initial draft match announcement");
                (draft_code, draft_token, key, initial_match_start)
            };

            tokio::time::sleep(Duration::from_millis(1)).await;
            {
                let mut games = app_state.sessions.lock().await;
                let session = &games.sessions[&expected_key.game_code];
                assert!(session.game_started);
                assert_eq!(session.connected, vec![false, false]);
                assert!(
                    games.reconnect.check_expired().is_empty(),
                    "awaiting the first attachment must not expire the match"
                );
            }

            let announced_token = match initial_match_start {
                ServerMessage::DraftMatchStart {
                    player_token,
                    full_key,
                    your_player: PlayerId(0),
                    ..
                } => {
                    assert_eq!(player_token, draft_token);
                    assert_eq!(full_key, expected_key);
                    player_token
                }
                other => panic!("expected initial DraftMatchStart, got {other:?}"),
            };

            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::ClientHello {
                        client_version: env!("CARGO_PKG_VERSION").to_string(),
                        build_commit: build_commit().to_string(),
                        protocol_version: PROTOCOL_VERSION,
                        lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
                        wire_formats: Vec::new(),
                    })
                    .expect("hello json")
                    .into(),
                ))
                .await
                .expect("send hello");
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::ReconnectDraft {
                        draft_code,
                        player_token: announced_token.clone(),
                    })
                    .expect("draft reconnect json")
                    .into(),
                ))
                .await
                .expect("send draft reconnect");

            match recv_server_message(&mut socket).await {
                ServerMessage::DraftMatchStart {
                    player_token,
                    full_key,
                    your_player: PlayerId(0),
                    ..
                } => {
                    assert_eq!(player_token, announced_token);
                    assert_eq!(full_key, expected_key);
                }
                other => panic!("expected DraftMatchStart, got {other:?}"),
            }
            match recv_server_message(&mut socket).await {
                ServerMessage::GameStarted {
                    full_key: Some(full_key),
                    your_player: PlayerId(0),
                    ..
                } => assert_eq!(full_key, expected_key),
                other => panic!("expected matching GameStarted, got {other:?}"),
            }
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::DraftStateUpdate { .. }
            ));

            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Action {
                        action: GameAction::GrantDebugPermission {
                            player_id: PlayerId(1),
                        },
                    })
                    .expect("action json")
                    .into(),
                ))
                .await
                .expect("send action");

            loop {
                match recv_server_message(&mut socket).await {
                    ServerMessage::ActionRejected { rejection } => {
                        assert_eq!(rejection.code, ActionRejectionCode::ActionNotAllowed);
                        break;
                    }
                    ServerMessage::ActionFailed { message }
                    | ServerMessage::Error { message, .. } => {
                        panic!("draft match action missed Full authority: {message}")
                    }
                    _ => {}
                }
            }

            socket
                .close(None)
                .await
                .expect("close attached game socket");
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let mut games = app_state.sessions.lock().await;
                if games
                    .reconnect
                    .is_disconnected(&expected_key.game_code, PlayerId(0))
                {
                    assert_eq!(
                        games.reconnect.check_expired(),
                        vec![expected_key.game_code.clone()],
                        "an actual socket disconnect must still expire normally"
                    );
                    break;
                }
            }
        })
        .await;
        server.abort();

        outcome.expect("draft attachment, action, or disconnect response timed out");
    }

    #[tokio::test]
    async fn draft_reconnect_sends_the_view_when_its_active_game_is_missing() {
        use super::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};

        let (url, server, _temp_dir, app_state) = spawn_full_mode_server().await;
        let (drafts, _, draft_code, draft_token) = test_draft();
        {
            let mut drafts = drafts.lock().await;
            let draft = drafts.sessions.get_mut(&draft_code).expect("draft exists");
            draft.session.status = DraftStatus::MatchInProgress;
            draft.session.current_round = 1;
            draft.session.pairings.push(DraftPairing {
                round: 1,
                table: 0,
                players: [PlayerId(0), PlayerId(1)],
                match_id: "r1-t0".to_string(),
                status: PairingStatus::Pending,
                winner: None,
            });
            draft
                .active_matches
                .insert("r1-t0".to_string(), "GONE01".to_string());
            assert!(draft.active_match_for_seat(0).is_some());
            *app_state.draft_sessions.lock().await = std::mem::take(&mut *drafts);
        }

        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::ServerHello { .. }
            ));
            for message in [
                ClientMessage::ClientHello {
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                    build_commit: build_commit().to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
                    wire_formats: Vec::new(),
                },
                ClientMessage::ReconnectDraft {
                    draft_code: draft_code.clone(),
                    player_token: draft_token,
                },
            ] {
                socket
                    .send(WsMessage::Text(
                        serde_json::to_string(&message)
                            .expect("client message json")
                            .into(),
                    ))
                    .await
                    .expect("send client message");
            }

            match recv_server_message(&mut socket).await {
                ServerMessage::DraftStateUpdate { view } => {
                    assert_eq!(view.status, DraftStatus::MatchInProgress);
                    assert_eq!(view.seats[0].seat_index, 0);
                }
                other => panic!("missing Full game must not block the draft view: {other:?}"),
            }
            assert!(app_state.connections.lock().await[&draft_code].contains_key(&PlayerId(0)));
        })
        .await;
        server.abort();
        outcome.expect("draft view response timed out");
    }

    #[tokio::test]
    async fn server_rebuilds_a_persisted_chaos_source_at_draft_start() {
        let (draft_state, _connections, draft_code, _player_token) = test_draft();
        {
            let mut manager = draft_state.lock().await;
            let draft = manager
                .sessions
                .get_mut(&draft_code)
                .expect("test draft exists");
            draft.config.source = DraftSource::Set {
                layout: SetLayout::Chaos {
                    candidate_codes: vec!["TST".to_string()],
                    assignments: vec![vec!["TST".to_string(); 3]; 8],
                },
            };
        }

        let result = draft_pack_generator_for_start(
            &draft_state,
            &Arc::new(draft_pools::DraftPools::default()),
            &draft_code,
        )
        .await;

        let Err(error) = result else {
            panic!("the empty test pool must fail after reaching Chaos generator construction");
        };

        assert_eq!(error, "No draft pool data for set: TST");
    }

    #[test]
    fn fresh_socket_keeps_draft_admission_and_reconnect_paths() {
        let fresh = empty_identity();
        for message in draft_admission_messages("DRAFT01") {
            assert_eq!(
                draft_socket_admission_rejection(&message, &fresh),
                None,
                "a fresh socket must reach {message:?}'s handler"
            );
        }
        assert_eq!(
            draft_socket_admission_rejection(
                &ClientMessage::ReconnectDraft {
                    draft_code: "DRAFT01".to_string(),
                    player_token: "player-token".to_string(),
                },
                &fresh,
            ),
            None,
            "a fresh socket must retain draft reconnect eligibility"
        );
    }

    #[tokio::test]
    async fn attached_draft_socket_cannot_create_or_join_without_replacing_its_seat() {
        let (draft_state, connections, draft_code, player_token) = test_draft();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut identity = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity,
            draft_code.clone(),
            player_token,
            &tx,
        )
        .await
        .expect("attach draft seat");
        let original_identity = (
            identity.draft_code.clone(),
            identity.draft_seat,
            identity.draft_token.clone(),
        );

        for message in draft_admission_messages(&draft_code) {
            assert_eq!(
                draft_socket_admission_rejection(&message, &identity),
                Some(DRAFT_SOCKET_FRESH_REJECTION),
                "the gate must reject {message:?} before its handler mutates draft state"
            );
        }

        assert_eq!(
            (
                identity.draft_code.clone(),
                identity.draft_seat,
                identity.draft_token.clone(),
            ),
            original_identity,
            "the gate must not overwrite the socket's existing draft identity"
        );
        assert_eq!(draft_state.lock().await.sessions.len(), 1);
        assert!(
            connections
                .lock()
                .await
                .get(&draft_code)
                .and_then(|players| players.get(&PlayerId(0)))
                .is_some_and(|sender| sender.same_channel(&tx)),
            "the rejected admission must leave the original sender installed"
        );
    }

    #[tokio::test]
    async fn stale_draft_socket_cannot_create_or_join_another_pod() {
        let (draft_state, connections, draft_code, player_token) = test_draft();
        let (stale_tx, _stale_rx) = mpsc::unbounded_channel();
        let (current_tx, _current_rx) = mpsc::unbounded_channel();
        let mut stale_identity = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut stale_identity,
            draft_code.clone(),
            player_token.clone(),
            &stale_tx,
        )
        .await
        .expect("attach original socket");
        let mut current_identity = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut current_identity,
            draft_code.clone(),
            player_token,
            &current_tx,
        )
        .await
        .expect("replace original socket");

        for message in draft_admission_messages("OTHER01") {
            assert_eq!(
                draft_socket_admission_rejection(&message, &stale_identity),
                Some(DRAFT_SOCKET_FRESH_REJECTION),
                "a stale seat must be rejected before {message:?} can overwrite its identity"
            );
        }
        assert!(
            connections
                .lock()
                .await
                .get(&draft_code)
                .and_then(|players| players.get(&PlayerId(0)))
                .is_some_and(|sender| sender.same_channel(&current_tx)),
            "the stale socket must not displace the current sender"
        );
        assert_eq!(draft_state.lock().await.sessions.len(), 1);
    }

    #[tokio::test]
    async fn reconnect_replaces_the_draft_seat_sender_before_identity_is_exposed() {
        let (draft_state, connections, draft_code, player_token) = test_draft();
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        let mut identity_a = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity_a,
            draft_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .expect("initial reconnect attaches A");
        let mut identity_b = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity_b,
            draft_code.clone(),
            player_token.clone(),
            &b_tx,
        )
        .await
        .expect("replacement reconnect attaches B");

        assert!(
            !draft_socket_is_current_preflight(&draft_state, &connections, &identity_a, &a_tx)
                .await,
            "A must lose draft mutation authority once B replaces its sender"
        );
        assert!(
            draft_socket_is_current_preflight(&draft_state, &connections, &identity_b, &b_tx).await
        );
        let err = reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity_a,
            draft_code.clone(),
            player_token,
            &a_tx,
        )
        .await
        .expect_err("superseded A must not replace B after a stale reconnect");
        assert_eq!(err, DRAFT_SOCKET_AUTHORITY_REJECTION);
        assert!(
            connections
                .lock()
                .await
                .get(&draft_code)
                .and_then(|players| players.get(&PlayerId(0)))
                .is_some_and(|sender| sender.same_channel(&b_tx)),
            "the sender map must point at B before B receives its draft identity"
        );

        broadcast_draft_views(&draft_code, &connections, &draft_state).await;
        assert!(matches!(
            b_rx.try_recv(),
            Ok(ServerMessage::DraftStateUpdate { .. })
        ));
        assert!(
            a_rx.try_recv().is_err(),
            "a broadcast after replacement must not target A's superseded sender"
        );
    }

    #[tokio::test]
    async fn stale_draft_close_cannot_disconnect_the_replacement_seat() {
        let (draft_state, connections, draft_code, player_token) = test_draft();
        let (a_tx, _a_rx) = mpsc::unbounded_channel();
        let (b_tx, _b_rx) = mpsc::unbounded_channel();
        let mut identity_a = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity_a,
            draft_code.clone(),
            player_token.clone(),
            &a_tx,
        )
        .await
        .unwrap();
        let mut identity_b = empty_identity();
        reconnect_draft_seat(
            &draft_state,
            &connections,
            &mut identity_b,
            draft_code.clone(),
            player_token,
            &b_tx,
        )
        .await
        .unwrap();

        disconnect_draft_seat_if_current(&draft_state, &connections, &identity_a, &a_tx).await;

        assert!(
            draft_state
                .lock()
                .await
                .sessions
                .get(&draft_code)
                .is_some_and(|session| session.connected[0]),
            "A's late close must not mark B's draft seat disconnected"
        );
        assert!(
            draft_socket_is_current_preflight(&draft_state, &connections, &identity_b, &b_tx).await
        );
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

        let full_key = server_core::FullSessionKey {
            game_code: "GAME01".to_string(),
            generation: 1,
        };
        let msg = build_spectator_state_update_message(&full_key, &state, &[], &[], 1)
            .expect("fixture snapshot");

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
mod draft_pod_size_policy_tests {
    use super::draft_pod_size_is_allowed;
    use draft_core::types::{DraftKind, TournamentFormat};

    /// Commander completes outside tournament pairings, so its normal 3-seat
    /// floor remains valid even when this transport value is selected.
    #[test]
    fn procedure_keeps_commander_single_elimination_at_its_normal_range() {
        assert!(draft_pod_size_is_allowed(
            DraftKind::CommanderDraft,
            TournamentFormat::SingleElimination,
            3
        ));
    }

    /// The paired positive reach-guard: a procedure that does run tournament
    /// pairings still requires the full single-elimination bracket.
    #[test]
    fn procedure_requires_the_full_pairing_bracket() {
        assert!(!draft_pod_size_is_allowed(
            DraftKind::Traditional,
            TournamentFormat::SingleElimination,
            4
        ));
        assert!(draft_pod_size_is_allowed(
            DraftKind::Traditional,
            TournamentFormat::SingleElimination,
            8
        ));
        assert!(draft_pod_size_is_allowed(
            DraftKind::Traditional,
            TournamentFormat::Swiss,
            4
        ));
    }
}

#[cfg(test)]
mod full_create_guard_tests {
    use super::*;
    use lobby_broker::validation::{MAX_DRAFT_SET_LABEL_LEN, MAX_TOKEN_LEN};
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
            set_code: "s".repeat(MAX_DRAFT_SET_LABEL_LEN + 1),
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

    pub(super) async fn spawn_full_mode_server() -> (
        String,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
        AppState,
    ) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let game_db = Arc::new(
            persistence::GameDb::open(
                &temp_dir.path().join("games.db"),
                persistence::SessionRetention::Multiplayer,
            )
            .expect("game db"),
        );
        let app_state = AppState {
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
            context: ServerContext::default(),
            public_url: None,
            allowed_origin: None,
        };
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(app_state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        (format!("ws://{addr}/ws"), handle, temp_dir, app_state)
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

    async fn recv_enveloped_server_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let frame = socket
            .next()
            .await
            .expect("websocket message")
            .expect("websocket frame");
        let WsMessage::Binary(bytes) = frame else {
            panic!("expected binary server message, got {frame:?}");
        };
        let json = wire::decode_client_envelope(bytes.to_vec(), MAX_WS_MESSAGE_BYTES)
            .await
            .expect("decode server envelope");
        serde_json::from_str(&json).expect("server message")
    }

    fn test_client_hello(wire_formats: Vec<WireFormat>) -> ClientMessage {
        ClientMessage::ClientHello {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            build_commit: build_commit().to_string(),
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            wire_formats,
        }
    }

    async fn send_test_message<S>(
        socket: &mut WebSocketStream<S>,
        message: &ClientMessage,
        enveloped: bool,
    ) where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let json = serde_json::to_vec(message).expect("client message json");
        let frame = if enveloped {
            WsMessage::Binary([vec![0x00], json].concat().into())
        } else {
            WsMessage::Text(String::from_utf8(json).expect("client message utf8").into())
        };
        socket.send(frame).await.expect("send client message");
    }

    #[tokio::test]
    async fn full_mode_create_sends_slots_after_game_created() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
                wire_formats: Vec::new(),
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
    async fn negotiated_gzip_envelope_accepts_and_returns_binary_frames() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url)
                .await
                .expect("connect");

            let server_hello = recv_server_message(&mut socket).await;
            assert!(matches!(
                server_hello,
                ServerMessage::ServerHello { wire_formats, .. }
                    if wire_formats.contains(&WireFormat::GzipEnvelopeV1)
            ));

            send_test_message(
                &mut socket,
                &test_client_hello(vec![WireFormat::GzipEnvelopeV1]),
                false,
            )
            .await;
            send_test_message(&mut socket, &ClientMessage::SubscribeLobby, true).await;
            assert!(matches!(
                recv_enveloped_server_message(&mut socket).await,
                ServerMessage::LobbyUpdate { .. }
            ));
            for _ in 0..RATE_LIMIT_MESSAGES {
                socket
                    .send(WsMessage::Binary(vec![0x01, 1, 2, 3].into()))
                    .await
                    .expect("send malformed gzip");
            }
            send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 7 }, true).await;
            let pong = tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if matches!(
                        recv_enveloped_server_message(&mut socket).await,
                        ServerMessage::Pong { .. }
                    ) {
                        break;
                    }
                }
            })
            .await;
            assert!(pong.is_err());
        })
        .await;
        server.abort();

        assert!(result.is_ok(), "binary envelope roundtrip timed out");
    }

    #[tokio::test]
    async fn lobby_broadcast_uses_each_clients_negotiated_frame_format() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let (mut compressed, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect compressed client");
            recv_server_message(&mut compressed).await;
            let (mut legacy, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect legacy client");
            recv_server_message(&mut legacy).await;

            send_test_message(
                &mut compressed,
                &test_client_hello(vec![WireFormat::GzipEnvelopeV1]),
                false,
            )
            .await;
            send_test_message(&mut legacy, &test_client_hello(Vec::new()), false).await;
            send_test_message(&mut compressed, &ClientMessage::SubscribeLobby, true).await;
            send_test_message(&mut legacy, &ClientMessage::SubscribeLobby, false).await;

            for _ in 0..2 {
                recv_enveloped_server_message(&mut compressed).await;
                recv_server_message(&mut legacy).await;
            }

            let (_trigger, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect broadcast trigger");
            while !matches!(
                recv_enveloped_server_message(&mut compressed).await,
                ServerMessage::PlayerCount { count: 3 }
            ) {}
            while !matches!(
                recv_server_message(&mut legacy).await,
                ServerMessage::PlayerCount { count: 3 }
            ) {}
        })
        .await;
        server.abort();

        assert!(result.is_ok(), "mixed-version lobby broadcast timed out");
    }

    #[tokio::test]
    async fn full_mode_create_rejects_format_invalid_host_deck() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
                wire_formats: Vec::new(),
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
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
                wire_formats: Vec::new(),
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
    use server_core::protocol::{AiSeatRequest, DeckData};
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
    async fn connect_and_hello(
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
            wire_formats: Vec::new(),
        };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&hello).expect("hello json").into(),
            ))
            .await
            .expect("send hello");
        socket
    }

    async fn create_authenticated_game_socket(
        url: String,
    ) -> WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        let mut socket = connect_and_hello(url).await;

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

    async fn create_started_ai_game(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    ) -> (String, String, server_core::FullSessionKey) {
        let create = ClientMessage::CreateGameWithSettings {
            deck: DeckData::default(),
            display_name: "Alice".to_string(),
            public: false,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: Default::default(),
            // An explicit empty list, matching the host's `DeckData::default()`:
            // this harness runs on an empty `CardDatabase`, where a random
            // starter deck resolves to nothing and the create is refused.
            ai_seats: vec![AiSeatRequest {
                seat_index: 1,
                difficulty: phase_ai::config::AiDifficulty::Easy,
                deck_name: None,
                deck: Some(DeckChoice::DeckList(Box::default())),
            }],
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

        let mut created = None;
        let mut started = false;
        while created.is_none() || !started {
            match recv_server_message(socket).await {
                ServerMessage::GameCreated {
                    game_code,
                    player_token,
                    full_key: Some(full_key),
                } => created = Some((game_code, player_token, full_key)),
                ServerMessage::GameStarted { .. } => started = true,
                ServerMessage::Error { message, .. } => {
                    panic!("AI game creation failed: {message}")
                }
                _ => {}
            }
        }
        created.expect("Full game identity")
    }

    async fn reconnect_started_game(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        game_code: &str,
        player_token: &str,
        full_key: server_core::FullSessionKey,
    ) {
        let reconnect = ClientMessage::Reconnect {
            game_code: game_code.to_string(),
            player_token: player_token.to_string(),
            full_key,
        };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&reconnect)
                    .expect("reconnect json")
                    .into(),
            ))
            .await
            .expect("send reconnect");
        loop {
            match recv_server_message(socket).await {
                ServerMessage::GameStarted { .. } => return,
                ServerMessage::Error { message, .. } => {
                    panic!("current reconnect failed: {message}")
                }
                _ => {}
            }
        }
    }

    /// Read frames until one is a submission answer, ignoring unrelated
    /// broadcasts the session emits. The enclosing
    /// `tokio::time::timeout` is the failure mode, as in every sibling test.
    async fn recv_submission_answer<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let msg = recv_server_message(socket).await;
            if matches!(
                msg,
                ServerMessage::ActionRejected { .. }
                    | ServerMessage::ActionFailed { .. }
                    | ServerMessage::RequestRejected { .. }
                    | ServerMessage::Error { .. }
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
    /// it to `false`. The session rejects it as an engine-shaped action
    /// refusal, so the client receives no server-policy prose.
    #[tokio::test]
    async fn action_frame_reaches_the_shared_submission_handler() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
            ServerMessage::ActionRejected { rejection } => {
                assert_eq!(rejection.code, ActionRejectionCode::ActionNotAllowed);
            }
            other => panic!("expected ActionRejected from the shared handler, got {other:?}"),
        }
    }

    /// Drives the production WebSocket route through the replacement sequence:
    /// A attaches, B takes the same Full seat, then stale A tries every
    /// state-changing route relevant to terminal retirement and closes. The
    /// active database row plus the state/map assertions before B reconnects
    /// prove neither stale frame nor stale close can retire or disconnect B.
    #[tokio::test]
    async fn stale_websocket_cannot_retire_or_disconnect_a_replaced_full_seat() {
        let (url, server, _temp_dir, app_state) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let mut socket_a = connect_and_hello(url.clone()).await;
            let (game_code, player_token, full_key) = create_started_ai_game(&mut socket_a).await;

            let mut socket_b = connect_and_hello(url).await;
            reconnect_started_game(&mut socket_b, &game_code, &player_token, full_key.clone())
                .await;

            socket_a
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Action {
                        action: GameAction::PassPriority,
                    })
                    .expect("action json")
                    .into(),
                ))
                .await
                .expect("send stale action");
            match recv_submission_answer(&mut socket_a).await {
                ServerMessage::ActionFailed { message } => {
                    assert_eq!(message, FULL_SOCKET_AUTHORITY_REJECTION);
                }
                other => panic!("stale action must be fenced, got {other:?}"),
            }

            socket_a
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::AbandonGame)
                        .expect("abandon json")
                        .into(),
                ))
                .await
                .expect("send stale abandon");
            match recv_server_message(&mut socket_a).await {
                ServerMessage::Error { message, .. } => {
                    assert_eq!(message, FULL_SOCKET_AUTHORITY_REJECTION);
                }
                other => panic!("stale abandon must be fenced, got {other:?}"),
            }

            socket_a.close(None).await.expect("close stale socket");
            tokio::time::timeout(Duration::from_secs(1), async {
                while app_state.player_count.load(Ordering::Relaxed) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("stale socket close must finish server-side cleanup");

            {
                let manager = app_state.sessions.lock().await;
                let session = manager
                    .sessions
                    .get(&game_code)
                    .expect("stale close must not remove B's session");
                assert!(
                    session.connected[0],
                    "stale close must not mark B's seat disconnected"
                );
                assert!(
                    !manager.reconnect.is_disconnected(&game_code, PlayerId(0)),
                    "stale close must not create B's reconnect lease"
                );
            }
            assert!(
                app_state
                    .connections
                    .lock()
                    .await
                    .get(&game_code)
                    .is_some_and(|players| players.contains_key(&PlayerId(0))),
                "stale close must retain B's current sender-map entry"
            );

            reconnect_started_game(&mut socket_b, &game_code, &player_token, full_key.clone())
                .await;

            assert_eq!(
                app_state
                    .game_db
                    .load_active_full_key(&game_code)
                    .expect("load active Full identity"),
                Some(full_key),
                "stale abandon must not retire the active Full row"
            );
        })
        .await;
        server.abort();
        result.expect("stale socket replacement scenario timed out");
    }

    #[test]
    fn action_and_request_session_refusals_use_their_distinct_channels() {
        let message = session_action_error_message(SessionActionError::Rejected(
            ActionRejection::new(ActionRejectionCode::ActionNotAllowed),
        ));
        assert!(matches!(
            message,
            ServerMessage::ActionRejected { rejection }
                if rejection.code == ActionRejectionCode::ActionNotAllowed
        ));
        assert!(matches!(
            session_action_error_message(SessionActionError::RequestRejected(
                "Match forfeits require a best-of-three match".to_string(),
            )),
            ServerMessage::RequestRejected { reason }
                if reason == "Match forfeits require a best-of-three match"
        ));
        assert!(matches!(
            session_action_error_message(SessionActionError::Operational(
                "session storage failed".to_string(),
            )),
            ServerMessage::Error { message, .. } if message == "session storage failed"
        ));
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
        let handler_rejection = match *GameSubmission::Interaction(oversized.clone())
            .payload_rejection()
            .expect_err("an oversized interaction is refused")
        {
            ServerMessage::ActionRejected { rejection } => rejection,
            ref other => panic!("an oversized paste must not tear the session down: {other:?}"),
        };

        // (ii) an oversized action stays a malformed frame.
        let action_rejection = match *GameSubmission::Action(oversized_action.clone())
            .payload_rejection()
            .expect_err("an oversized action is refused")
        {
            ServerMessage::ActionRejected { rejection } => rejection,
            ref other => panic!("an oversized action is an invalid action, got {other:?}"),
        };

        // (iii) both layers agree, in variant *and* in reason string.
        let wire_interaction = ClientMessage::Interaction {
            submission: oversized,
        };
        let wire_reason =
            guard_client_message_before_dispatch(&wire_interaction, ServerMode::Full).unwrap_err();
        match wire_rejection_message(&wire_interaction, wire_reason) {
            ServerMessage::ActionRejected { rejection } => assert_eq!(rejection, handler_rejection),
            other => panic!("wire and handler disagree on the interaction channel: {other:?}"),
        }

        let wire_action = ClientMessage::Action {
            action: oversized_action,
        };
        let wire_action_reason =
            guard_client_message_before_dispatch(&wire_action, ServerMode::Full).unwrap_err();
        match wire_rejection_message(&wire_action, wire_action_reason) {
            ServerMessage::ActionRejected { rejection } => assert_eq!(rejection, action_rejection),
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
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
            ServerMessage::ActionRejected { rejection } => {
                assert_eq!(rejection.code, ActionRejectionCode::StaleInteraction);
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
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
            ServerMessage::ActionRejected { rejection } => {
                assert_eq!(
                    rejection.code,
                    ActionRejectionCode::InteractionPayloadTooLarge
                );
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
    async fn a_game_submission_without_a_session_is_answered_on_the_failed_channel() {
        let (url, server, _temp_dir, _game_db) = spawn_full_mode_server().await;
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
                wire_formats: Vec::new(),
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
            ServerMessage::ActionFailed { message } => assert_eq!(message, "Not in a game"),
            other => panic!("a pre-session condition must settle the action promise: {other:?}"),
        }
    }
}

#[cfg(test)]
mod refused_auto_start_join_tests {
    use super::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};
    use super::*;
    use futures_util::SinkExt;
    use server_core::protocol::{AiSeatRequest, DeckData};
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    async fn connect_and_hello(url: &str) -> TestSocket {
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");
        assert!(matches!(
            recv_server_message(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ));
        send(
            &mut socket,
            &ClientMessage::ClientHello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                build_commit: build_commit().to_string(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
                wire_formats: Vec::new(),
            },
        )
        .await;
        socket
    }

    async fn send(socket: &mut TestSocket, message: &ClientMessage) {
        socket
            .send(WsMessage::Text(
                serde_json::to_string(message)
                    .expect("client message json")
                    .into(),
            ))
            .await
            .expect("send client message");
    }

    /// Everything the socket says before it goes quiet. The handler's reply to
    /// a refused auto-start is several messages long and the assertions are
    /// about which ones are present, so collecting the whole burst reports a
    /// missing message as a listing rather than as a hung receive.
    async fn drain(socket: &mut TestSocket) -> Vec<ServerMessage> {
        let mut received = Vec::new();
        while let Ok(message) =
            tokio::time::timeout(Duration::from_millis(750), recv_server_message(socket)).await
        {
            received.push(message);
        }
        received
    }

    /// A join that fills the room and then has its auto-start refused must
    /// still hand the joiner their seat.
    ///
    /// The refusal is the cEDH bracket gate in `GameSession::start_game`: seat 1
    /// runs at cEDH difficulty while no deck at the table is declared at that
    /// bracket tier. The gate returns before `start_game` writes anything, so
    /// the room is pregame with the joiner seated — and a joiner who never
    /// learns their token can neither reconnect nor free the seat they hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_refused_auto_start_still_attaches_the_joiner_to_their_seat() {
        let (url, server, _temp_dir, app_state) = spawn_full_mode_server().await;

        let mut host = connect_and_hello(&url).await;
        send(
            &mut host,
            &ClientMessage::CreateGameWithSettings {
                deck: DeckData::default(),
                display_name: "Alice".to_string(),
                public: false,
                password: None,
                timer_seconds: None,
                // Three seats with one AI leaves a human seat open, so the
                // create path does not take its own start-immediately branch.
                player_count: 3,
                match_config: Default::default(),
                ai_seats: vec![AiSeatRequest {
                    seat_index: 1,
                    difficulty: phase_ai::config::AiDifficulty::CEDH,
                    deck_name: None,
                    // An explicit empty list: this harness runs on an empty
                    // `CardDatabase`, where a random starter deck resolves to
                    // nothing and the create is refused.
                    deck: Some(DeckChoice::DeckList(Box::default())),
                }],
                format_config: None,
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
            },
        )
        .await;

        let game_code = drain(&mut host)
            .await
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::GameCreated { game_code, .. } => Some(game_code),
                ServerMessage::Error { message, .. } => panic!("create was refused: {message}"),
                _ => None,
            })
            .expect("the create reported a game code");

        let mut guest = connect_and_hello(&url).await;
        send(
            &mut guest,
            &ClientMessage::JoinGameWithPassword {
                game_code: game_code.clone(),
                deck: DeckData::default(),
                display_name: "Bob".to_string(),
                password: None,
                reservation_token: None,
            },
        )
        .await;
        let replies = drain(&mut guest).await;

        let attached = replies.iter().find_map(|message| match message {
            ServerMessage::SessionAttached {
                game_code,
                player_id,
                player_token,
                ..
            } => Some((game_code.clone(), *player_id, player_token.clone())),
            _ => None,
        });
        let refusal = replies.iter().find_map(|message| match message {
            ServerMessage::Error { message, .. } => Some(message.clone()),
            _ => None,
        });

        let (full, arms_auto_start, pregame, seat_of_token) = {
            let mgr = app_state.sessions.lock().await;
            let session = mgr.sessions.get(&game_code).expect("the room is live");
            (
                session.is_full(),
                session.start_when_full,
                session.is_pregame(),
                attached
                    .as_ref()
                    .and_then(|(_, _, token)| session.player_for_token(token)),
            )
        };
        server.abort();

        // A `StateUpdate` carries the whole GameState, so failures report the
        // burst by message type rather than by value.
        let kinds = replies
            .iter()
            .map(|message| {
                serde_json::to_value(message).expect("server message json")["type"]
                    .as_str()
                    .expect("tagged server message")
                    .to_string()
            })
            .collect::<Vec<_>>();

        // Reach guard: these two are exactly the handler's `should_start`, so a
        // run where the auto-start was never attempted fails here instead of
        // passing the assertions below for the wrong reason.
        assert!(
            full && arms_auto_start,
            "the join did not arm an auto-start (full: {full}, start_when_full: {arms_auto_start})"
        );
        // The tier the estimator reports for an unbracketed deck is incidental;
        // the gate and the seat it names are the reach guard.
        let refusal = refusal.unwrap_or_default();
        assert!(
            refusal.starts_with("Cannot start cEDH game: seat 0 is not declared cEDH"),
            "the bracket gate did not refuse the start (got {refusal:?} out of {kinds:?})"
        );
        assert!(pregame, "a refused start left the room flagged as started");

        let (attached_code, player_id, _) = attached.unwrap_or_else(|| {
            panic!("the joiner holds a seat they were never given: no SessionAttached in {kinds:?}")
        });
        assert_eq!(attached_code, game_code);
        assert_eq!(player_id, PlayerId(2));
        assert_eq!(
            seat_of_token,
            Some(PlayerId(2)),
            "the token the joiner was handed does not resolve to their seat"
        );
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
            ClientMessage::ExportAuthoritativeState,
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
                source: None,
                set_codes: Some(vec!["TST".into()]),
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
    fn mode_gate_keeps_operational_failures_with_their_game_request() {
        assert!(matches!(
            operation_failed_message(
                &ClientMessage::Action {
                    action: GameAction::PassPriority,
                },
                "disabled".to_string(),
            ),
            Some(ServerMessage::ActionFailed { message }) if message == "disabled"
        ));
        assert!(matches!(
            operation_failed_message(
                &ClientMessage::Interaction {
                    submission: InteractionSubmission {
                        interaction_id: engine::types::interaction::InteractionId("i".to_string()),
                        response: engine::types::interaction::InteractionResponse::Choose {
                            choice_id: engine::types::interaction::InteractionChoiceId("c".to_string()),
                        },
                    },
                },
                "disabled".to_string(),
            ),
            Some(ServerMessage::ActionFailed { message }) if message == "disabled"
        ));
        assert!(matches!(
            operation_failed_message(
                &ClientMessage::PreviewManaPayment {
                    request_id: 5,
                    action: GameAction::PassPriority,
                },
                "disabled".to_string(),
            ),
            Some(ServerMessage::ManaPaymentPreviewFailed { request_id: 5, message }) if message == "disabled"
        ));
        assert!(matches!(
            operation_failed_message(
                &ClientMessage::ExportAuthoritativeState,
                "disabled".to_string(),
            ),
            Some(ServerMessage::AuthoritativeStateExportFailed { message }) if message == "disabled"
        ));
        assert!(
            operation_failed_message(&ClientMessage::ConcedeMatch, "disabled".to_string(),)
                .is_none()
        );
    }

    #[test]
    fn lobby_only_allows_broker_and_lifecycle_messages() {
        let allowed: Vec<ClientMessage> = vec![
            ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
                wire_formats: Vec::new(),
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
            ClientMessage::ExportAuthoritativeState,
            ClientMessage::AbandonGame,
            ClientMessage::Concede,
            ClientMessage::ConcedeMatch,
            ClientMessage::Ping { timestamp: 0 },
            ClientMessage::CreateDraftWithSettings {
                display_name: "A".into(),
                source: None,
                set_codes: Some(vec!["TST".into()]),
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

    // -- Tournament projection ---------------------------------------------

    use server_core::protocol::{
        BracketShape, MatchArity, PairingOutcome, PairingView, PlayerSummary, PodOutcome,
        ScoringPolicy, TournamentRequestId, TournamentStatus, TournamentSummary, TournamentView,
    };

    fn tournament_client_frames() -> Vec<ClientMessage> {
        vec![
            ClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::COMMANDER_POD,
                scoring: ScoringPolicy::default_for_arity(MatchArity::COMMANDER_POD),
                bracket: BracketShape::Swiss,
                total_rounds: Some(4),
            },
            ClientMessage::JoinTournament {
                code: "TOUR01".into(),
                player_key: "key-a".into(),
                display_name: "Alice".into(),
            },
            ClientMessage::GetTournament {
                code: "TOUR01".into(),
            },
            // Deliberately CORRELATED, and the only fixture here that is.
            // `skip_serializing_if = "Option::is_none"` means a `None`
            // correlator emits no key at all, so under an all-`None` table a
            // projection that FORWARDED the field and one that DISCARDED it
            // serialize to byte-identical strings — and
            // `tournament_variants_survive_the_canonical_lobby_roundtrip`, whose
            // whole job is to catch a dropped field, would pass either way.
            // A real `Some(..)` is what makes that instrument fire.
            ClientMessage::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: "org-tok".into(),
                request_id: Some(TournamentRequestId(7)),
            },
            // The remaining three stay uncorrelated, so the same pass also
            // proves a pre-correlation frame still round-trips unchanged.
            ClientMessage::ReportMatchResult {
                code: "TOUR01".into(),
                pairing_id: 7,
                player_token: "player-tok".into(),
                outcome: PodOutcome::Decisive {
                    winner: "key-a".into(),
                    game_wins: [("key-a".to_string(), 2u8), ("key-b".to_string(), 1u8)]
                        .into_iter()
                        .collect(),
                },
                request_id: None,
            },
            ClientMessage::DropFromTournament {
                code: "TOUR01".into(),
                player_token: "player-tok".into(),
                request_id: None,
            },
            ClientMessage::EndTournament {
                code: "TOUR01".into(),
                organizer_token: "org-tok".into(),
                request_id: None,
            },
        ]
    }

    fn sample_view() -> TournamentView {
        let alice = PlayerSummary {
            player_key: "key-a".into(),
            display_name: "Alice".into(),
            dropped: false,
        };
        let bob = PlayerSummary {
            player_key: "key-b".into(),
            display_name: "Bob".into(),
            dropped: true,
        };
        TournamentView {
            summary: TournamentSummary {
                code: "TOUR01".into(),
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                bracket: BracketShape::Swiss,
                status: TournamentStatus::InProgress,
                player_count: 1,
                current_round: 1,
                total_rounds: 3,
                created_at: 1_000,
            },
            players: vec![alice.clone(), bob.clone()],
            // Every `PairingOutcome` shape, so the round-trip cannot pass by
            // collapsing a variant to a default.
            pairings: vec![
                PairingView {
                    id: 0,
                    round: 1,
                    players: vec![alice.clone()],
                    outcome: Some(PairingOutcome::Bye),
                },
                PairingView {
                    id: 1,
                    round: 1,
                    players: vec![alice.clone(), bob.clone()],
                    outcome: Some(PairingOutcome::Forfeit {
                        winner: "key-a".into(),
                    }),
                },
                PairingView {
                    id: 2,
                    round: 1,
                    players: vec![alice.clone(), bob.clone()],
                    outcome: Some(PairingOutcome::Reported(PodOutcome::Decisive {
                        winner: "key-a".into(),
                        game_wins: [("key-a".to_string(), 2u8), ("key-b".to_string(), 0u8)]
                            .into_iter()
                            .collect(),
                    })),
                },
                PairingView {
                    id: 3,
                    round: 1,
                    players: vec![alice.clone(), bob.clone()],
                    outcome: Some(PairingOutcome::Reported(PodOutcome::Draw)),
                },
                PairingView {
                    id: 4,
                    round: 2,
                    players: vec![alice, bob],
                    outcome: None,
                },
            ],
            standings: Vec::new(),
        }
    }

    /// Verification Matrix row 9. The `.is_some()` assertion is the whole
    /// point: `to_lobby_client_message` ends in `_ => return None`, so a
    /// forgotten arm is NOT a compile error — it silently drops the frame.
    /// A test that only compared fields inside an `if let Some(..)` would pass
    /// VACUOUSLY against exactly that bug, which is why every frame is
    /// unwrapped up front and the count is asserted.
    #[test]
    fn tournament_variants_survive_the_canonical_lobby_roundtrip() {
        let frames = tournament_client_frames();
        assert_eq!(frames.len(), 7, "every new client variant is covered");

        for msg in &frames {
            let projected = to_lobby_client_message(msg).unwrap_or_else(|| {
                panic!(
                    "{msg:?} projected to None — its arm is missing from to_lobby_client_message"
                )
            });

            // The two enums are wire-compatible by construction, so equality
            // of the serialized forms is the strongest available check
            // (neither enum is `PartialEq`: both carry `DeckData`).
            assert_eq!(
                serde_json::to_string(&projected).expect("projected serializes"),
                serde_json::to_string(msg).expect("canonical serializes"),
                "field dropped or renamed across the projection for {msg:?}"
            );
        }
    }

    /// The server direction. `to_server_message` is wildcard-free, so a
    /// missing arm is a compile error — this covers the remaining risk, a
    /// field mistyped or dropped inside an arm that does exist.
    #[test]
    fn tournament_server_variants_survive_the_canonical_lobby_roundtrip() {
        let view = sample_view();
        let messages = vec![
            lobby_broker::LobbyServerMessage::TournamentCreated {
                code: "TOUR01".into(),
                organizer_token: "org-tok".into(),
                view: view.clone(),
            },
            lobby_broker::LobbyServerMessage::TournamentJoined {
                code: "TOUR01".into(),
                player_token: "player-tok".into(),
                view: view.clone(),
            },
            lobby_broker::LobbyServerMessage::TournamentUpdate {
                code: "TOUR01".into(),
                view: view.clone(),
            },
            lobby_broker::LobbyServerMessage::TournamentRemoved {
                code: "TOUR01".into(),
            },
            lobby_broker::LobbyServerMessage::TournamentListUpdate {
                tournaments: vec![view.summary.clone()],
            },
            // `TournamentRequestId` is one type re-exported by both crates, so
            // the lobby literal and the canonical mirror name the same struct.
            lobby_broker::LobbyServerMessage::TournamentActionAck {
                request_id: TournamentRequestId(7),
                code: "TOUR01".into(),
                view: view.clone(),
            },
            lobby_broker::LobbyServerMessage::TournamentActionRejected {
                request_id: TournamentRequestId(7),
                message: "not the organizer".into(),
            },
        ];
        assert_eq!(messages.len(), 7, "every new server variant is covered");

        for msg in messages {
            let expected = serde_json::to_string(&msg).expect("lobby form serializes");
            let converted = to_server_message(msg);
            assert_eq!(
                serde_json::to_string(&converted).expect("canonical form serializes"),
                expected
            );
        }
    }

    /// Regression guard: the pre-existing arms of both conversions are
    /// unaffected by the tournament additions.
    #[test]
    fn pre_existing_lobby_projections_are_unaffected() {
        assert!(to_lobby_client_message(&ClientMessage::SubscribeLobby).is_some());
        assert!(to_lobby_client_message(&ClientMessage::UnsubscribeLobby).is_some());
        assert!(to_lobby_client_message(&ClientMessage::Ping { timestamp: 1 }).is_some());
        assert!(to_lobby_client_message(&ClientMessage::UnregisterLobby {
            game_code: "GAME01".into()
        })
        .is_some());
        assert!(to_lobby_client_message(&ClientMessage::Concede).is_none());
    }

    /// Tournaments are mode-agnostic: unlike `UpdateLobbyMetadata`/
    /// `UnregisterLobby` (LobbyOnly-exclusive) and `Action` (Full-only), every
    /// tournament variant is accepted in BOTH modes. Both halves are asserted
    /// so a wholesale `None` in `reject_if_disabled` cannot satisfy this.
    #[test]
    fn tournament_variants_are_accepted_in_both_server_modes() {
        for msg in tournament_client_frames() {
            assert!(
                reject_if_disabled(&msg, ServerMode::Full).is_none(),
                "{msg:?} was refused in Full mode"
            );
            assert!(
                reject_if_disabled(&msg, ServerMode::LobbyOnly).is_none(),
                "{msg:?} was refused in LobbyOnly mode"
            );
        }
        // Contrast, so the test above is not passing because the gate is a
        // wholesale `None`.
        assert!(reject_if_disabled(&ClientMessage::Concede, ServerMode::LobbyOnly).is_some());
        assert!(reject_if_disabled(
            &ClientMessage::UnregisterLobby {
                game_code: "GAME01".into()
            },
            ServerMode::Full
        )
        .is_some());
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
            lobby_organized_tournaments: Vec::new(),
            lobby_joined_tournaments: Vec::new(),
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
                wire_formats: Vec::new(),
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::Accept(ClientHelloInfo {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                wire_format: None,
            })
        );
    }

    #[test]
    fn negotiates_gzip_envelope_from_client_hello_capability() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
                wire_formats: vec![WireFormat::GzipEnvelopeV1],
            },
            hello_acceptance(ServerMode::Full),
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::Accept(ClientHelloInfo {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                wire_format: Some(WireFormat::GzipEnvelopeV1),
            })
        );
    }

    #[test]
    fn rejects_v49_before_it_can_omit_default_deck_copy_limit() {
        // v49 added `active_pack_count`, but it predates the per-format
        // `default_deck_copy_limit` required by v50. Full games must reject it
        // at hello rather than let a client use the fail-closed singleton
        // fallback instead of the format's declared copy limit.
        let previous = 49;
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.10".into(),
                build_commit: "old1234".into(),
                protocol_version: previous,
                // Legacy client: predates the lobby-owned version, so the
                // gate must fall back to the `protocol_version` window.
                lobby_protocol_version: None,
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                wire_formats: Vec::new(),
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
                card_instance_ids: vec!["x".into()],
            },
            draft_core::types::DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id: "effect".into(),
                card_instance_ids: vec!["x".into(), "y".into()],
            },
            draft_core::types::DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
                commanders: vec![],
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

    /// The `ConnState` round-trip through the shell's own identity store.
    ///
    /// The broker appends to its transient `ConnState` view; without this
    /// threading those appends would be discarded the instant the view drops,
    /// leaving both fields permanently empty in the native shell while the
    /// WASM shell (which round-trips `ConnState` through the WebSocket
    /// attachment) populated them correctly. That divergence is a silent
    /// no-op no broker-level test can catch, because at the broker layer the
    /// append demonstrably works.
    #[test]
    fn tournament_conn_state_survives_the_socket_identity_round_trip() {
        let mut identity = empty_identity();
        assert!(identity.to_conn_state().organized_tournaments.is_empty());
        assert!(identity.to_conn_state().joined_tournaments.is_empty());

        let mut conn = identity.to_conn_state();
        conn.organized_tournaments.push("TOUR01".to_string());
        conn.joined_tournaments.push("TOUR02".to_string());
        identity.absorb_conn_state(conn);

        // Absorbed into the shell...
        assert_eq!(identity.lobby_organized_tournaments, vec!["TOUR01"]);
        assert_eq!(identity.lobby_joined_tournaments, vec!["TOUR02"]);
        // ...and projected back out on the NEXT broker call, which is the half
        // that would be missing if `to_conn_state` defaulted these fields.
        let next = identity.to_conn_state();
        assert_eq!(next.organized_tournaments, vec!["TOUR01"]);
        assert_eq!(next.joined_tournaments, vec!["TOUR02"]);
    }

    /// The shell-side mirror must never come to hold a tournament
    /// `player_token`.
    ///
    /// This is the field the token would have outlived its operation in: the
    /// list is never pruned, `SocketIdentity` is per-socket state that
    /// survives every broker call, and the equivalent WASM shell copies the
    /// same `ConnState` verbatim into a durable WebSocket attachment. So the
    /// assertion is made against a REAL minted token from a REAL join, not a
    /// placeholder — a shape-only check would pass just as happily against a
    /// tuple that still carried the secret.
    #[test]
    fn socket_identity_never_retains_a_tournament_player_token() {
        use lobby_broker::{LobbyClientMessage, LobbyServerMessage};
        use server_core::protocol::{BracketShape, MatchArity, ScoringPolicy};

        // `SysEnv` is the production token source, so the needle below is a
        // genuine `generate_player_token()` value.
        let env = SysEnv;
        let mut broker = Broker::new();
        let mut identity = empty_identity();
        let mut conn = identity.to_conn_state();

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            &env,
        );
        let (code, organizer_token) = match &out[0] {
            Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                ..
            }) => (code.clone(), organizer_token.clone()),
            other => panic!("expected TournamentCreated, got {other:?}"),
        };

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::JoinTournament {
                code: code.clone(),
                player_key: "key-a".into(),
                display_name: "Alice".into(),
            },
            &env,
        );
        let player_token = match &out[0] {
            Outbound::ToSelf(LobbyServerMessage::TournamentJoined { player_token, .. }) => {
                player_token.clone()
            }
            other => panic!("expected TournamentJoined, got {other:?}"),
        };
        assert!(!player_token.is_empty() && !organizer_token.is_empty());

        identity.absorb_conn_state(conn);

        // Reach-guard: the create and the join really did reach the mirror, so
        // the negatives below are statements about a populated field rather
        // than about an empty one.
        assert_eq!(identity.lobby_organized_tournaments, vec![code.clone()]);
        assert_eq!(identity.lobby_joined_tournaments, vec![code.clone()]);

        // Structural: scan the whole projected `ConnState` — the exact value
        // the WASM shell serializes into its durable attachment — so a token
        // reintroduced at ANY nesting depth fails this, not just one in the
        // field it is expected in.
        let projected = identity.to_conn_state();
        let json = serde_json::to_string(&projected).expect("ConnState serializes");
        assert!(
            json.contains(code.as_str()),
            "reach-guard: the tournament code must be present: {json}"
        );
        assert!(
            !json.contains(player_token.as_str()),
            "player_token retained in per-socket state: {json}"
        );
        assert!(
            !json.contains(organizer_token.as_str()),
            "organizer_token retained in per-socket state: {json}"
        );
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
            let (code, _token) = mgr.create_game(PlayerDeckPayload::default(), None);
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
            let (code, _token) = mgr.create_game(PlayerDeckPayload::default(), None);
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

        let (game_code, _token, _host_player, _count, _full_key) =
            create_and_connect_multiplayer_session(
                &state,
                &connections,
                &game_db,
                MultiplayerSessionRequest {
                    resolved: PlayerDeckPayload::default(),
                    host_choice: seat_reducer::types::DeckChoice::Random,
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
                    context: ServerContext::default(),
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
mod ai_seat_setup_tests {
    use engine::database::CardDatabase;
    use engine::types::match_config::MatchType;
    use phase_ai::config::AiDifficulty;
    use seat_reducer::types::DeckChoice;
    use server_core::protocol::AiSeatRequest;

    use super::ai_seat_setups;

    fn db_with(names: &[&str]) -> CardDatabase {
        let entries: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|name| {
                (
                    name.to_lowercase(),
                    serde_json::json!({
                        "name": name,
                        "mana_cost": { "type": "NoCost" },
                        "card_type": {
                            "supertypes": [], "core_types": ["Land"], "subtypes": []
                        },
                        "power": null,
                        "toughness": null,
                        "loyalty": null,
                        "defense": null,
                        "oracle_text": null,
                        "abilities": [],
                        "triggers": [],
                        "static_abilities": [],
                        "replacements": [],
                        "keywords": []
                    }),
                )
            })
            .collect();
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
            .expect("fixture database parses")
    }

    fn list(names: &[&str]) -> DeckChoice {
        DeckChoice::DeckList(Box::new(engine::starter_decks::DeckData {
            main_deck: names.iter().map(|n| n.to_string()).collect(),
            ..Default::default()
        }))
    }

    fn request(deck: Option<DeckChoice>, deck_name: Option<&str>) -> AiSeatRequest {
        AiSeatRequest {
            seat_index: 1,
            difficulty: AiDifficulty::Easy,
            deck_name: deck_name.map(str::to_string),
            deck,
        }
    }

    #[test]
    fn a_resolvable_ai_deck_is_set_up_with_the_choice_that_describes_it() {
        // The paired positive control for the refusal below, which a function
        // that refused everything would satisfy vacuously.
        let db = db_with(&["Forest"]);

        let setups = ai_seat_setups(
            &db,
            &[request(Some(list(&["Forest"])), None)],
            2,
            None,
            MatchType::Bo1,
        )
        .expect("a resolvable deck is accepted");

        assert_eq!(setups.len(), 1);
        let DeckChoice::DeckList(recorded) = &setups[0].choice else {
            panic!("expected a recorded list, got {:?}", setups[0].choice);
        };
        assert_eq!(
            server_core::deck_resolve::resolve_deck(&db, recorded)
                .expect("the recorded choice re-resolves")
                .main_deck
                .len(),
            setups[0].resolved.main_deck.len()
        );
    }

    #[test]
    fn an_unresolvable_ai_deck_refuses_the_create() {
        let db = db_with(&["Forest"]);

        let result = ai_seat_setups(
            &db,
            &[request(Some(list(&["Nonexistent Card"])), None)],
            2,
            None,
            MatchType::Bo1,
        );

        // No setup is produced: the seat cannot be installed with a deck no
        // recorded choice describes.
        assert!(result.is_err(), "expected a refusal, got {result:?}");
    }

    #[test]
    fn a_deck_name_only_request_records_the_named_deck() {
        let starter = server_core::starter_decks::find_starter_deck("Red Deck Wins")
            .expect("the starter table names this deck");
        let names: Vec<&str> = starter.main_deck.iter().map(String::as_str).collect();
        let db = db_with(&names);

        let setups = ai_seat_setups(
            &db,
            &[request(None, Some("Red Deck Wins"))],
            2,
            None,
            MatchType::Bo1,
        )
        .expect("a named starter deck resolves");

        assert_eq!(setups.len(), 1);
        assert_eq!(
            setups[0].choice,
            DeckChoice::DeckList(Box::new(starter)),
            "the recorded choice must be the deck the seat actually got, not `Random`"
        );
    }
}

#[cfg(test)]
mod player_slots_projection_tests {
    use super::*;
    use engine::game::deck_loading::PlayerDeckPayload;
    use engine::types::format::FormatConfig;
    use phase_ai::config::AiDifficulty;
    use server_core::session::AiSeatSetup;
    use server_core::SeatKind;

    fn ai_seat_deck(msg: Option<ServerMessage>) -> DeckChoice {
        match msg {
            Some(ServerMessage::PlayerSlotsUpdate { slots }) => match &slots[1].kind {
                SeatKind::Ai { deck, .. } => deck.clone(),
                other => panic!("seat 1 is not an AI seat: {other:?}"),
            },
            other => panic!("expected PlayerSlotsUpdate, got {other:?}"),
        }
    }

    /// The host matches an AI seat's recorded list against its local catalog to
    /// decide the seat is already dealt; no other client surface reads it.
    #[tokio::test]
    async fn a_guest_receives_random_where_the_host_receives_the_ai_seats_list() {
        let installed = DeckChoice::DeckList(Box::new(engine::starter_decks::DeckData {
            main_deck: vec!["Forest".to_string()],
            ..Default::default()
        }));
        let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
        let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));

        let game_code = {
            let mut mgr = state.lock().await;
            let (code, _token) = mgr
                .create_game_n_players(
                    PlayerDeckPayload::default(),
                    None,
                    String::new(),
                    None,
                    3,
                    engine::types::match_config::MatchConfig::default(),
                    Some(FormatConfig::commander()),
                )
                .expect("commander seats three");
            mgr.sessions.get_mut(&code).unwrap().seat_ai(AiSeatSetup {
                seat_index: 1,
                difficulty: AiDifficulty::Medium,
                choice: installed.clone(),
                resolved: PlayerDeckPayload::default(),
            });
            code
        };

        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        let (guest_tx, mut guest_rx) = mpsc::unbounded_channel();
        {
            let mut conns = connections.lock().await;
            let room = conns.entry(game_code.clone()).or_default();
            room.insert(PlayerId(0), host_tx);
            room.insert(PlayerId(2), guest_tx);
        }

        broadcast_player_slots(&state, &connections, &game_code).await;

        assert_eq!(ai_seat_deck(host_rx.recv().await), installed);
        assert_eq!(ai_seat_deck(guest_rx.recv().await), DeckChoice::Random);
    }
}

#[cfg(test)]
mod delist_tests {
    use lobby_broker::lobby::{LobbyManager, RegisterGameRequest};
    use lobby_broker::BrokerEnv;

    use super::delist_destroyed_sessions;

    struct FixedEnv;
    impl BrokerEnv for FixedEnv {
        fn now_ms(&self) -> u64 {
            1_000
        }
        fn new_token(&self) -> String {
            "token".to_string()
        }
        fn new_game_code(&self) -> String {
            "CODE00".to_string()
        }
    }

    fn register(lob: &mut LobbyManager, code: &str, public: bool) {
        lob.register_game(
            code,
            RegisterGameRequest {
                host_name: "Host".to_string(),
                public,
                ..Default::default()
            },
            &FixedEnv,
        );
    }

    #[test]
    fn a_destroyed_public_room_is_delisted_and_announced() {
        let mut lob = LobbyManager::new();
        register(&mut lob, "PUB001", true);

        let announce = delist_destroyed_sessions(&mut lob, std::iter::once("PUB001"));

        assert!(!lob.has_game("PUB001"));
        assert_eq!(announce, vec!["PUB001".to_string()]);
    }

    #[test]
    fn a_private_room_is_delisted_without_an_announcement() {
        let mut lob = LobbyManager::new();
        register(&mut lob, "PRIV01", false);

        let announce = delist_destroyed_sessions(&mut lob, std::iter::once("PRIV01"));

        // Reach guard: it really was registered, so the removal is measured.
        assert!(!lob.has_game("PRIV01"));
        assert!(announce.is_empty(), "private entry announced: {announce:?}");
    }

    #[test]
    fn an_unregistered_code_is_neither_removed_nor_announced() {
        let mut lob = LobbyManager::new();
        register(&mut lob, "PUB001", true);

        let announce = delist_destroyed_sessions(&mut lob, std::iter::once("GONE01"));

        assert!(announce.is_empty(), "absent code announced: {announce:?}");
        // The bystander entry is untouched.
        assert!(lob.has_game("PUB001"));
    }

    /// The multi-code axis: one call clears every code it is handed and returns
    /// only the ones that were publicly listed.
    #[test]
    fn one_call_delists_every_code_and_announces_only_the_public_ones() {
        let mut lob = LobbyManager::new();
        register(&mut lob, "PUBA01", true);
        register(&mut lob, "PUBB01", true);
        register(&mut lob, "PRIVC1", false);

        let codes = ["PUBA01", "PUBB01", "PRIVC1"];
        let announce = delist_destroyed_sessions(&mut lob, codes.iter().copied());

        assert!(codes.iter().all(|code| !lob.has_game(code)));
        assert_eq!(
            announce,
            vec!["PUBA01".to_string(), "PUBB01".to_string()],
            "only the public entries are announced"
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
        AppState, ServerContext, ServerMode,
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
            context: ServerContext::default(),
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
mod compression_tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use axum::http::StatusCode;

    use lobby_broker::Broker;
    use server_core::draft_session::DraftSessionManager;
    use server_core::session::SessionManager;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    use tower_http::cors::CorsLayer;
    use url::Url;

    use super::{
        announcement_from_state, build_commit, build_router, draft_pools, persistence,
        spawn_announce_task, validate_announcement, AppState, RawAnnouncement, ServerContext,
        ServerInfoDocument, ServerMode, ANNOUNCE_REQUEST_TIMEOUT, DIRECTORY_VERSION, INFO_PATH,
        LOBBY_PROTOCOL_VERSION, MAX_SERVER_NAME_LEN, PROTOCOL_VERSION,
    };

    fn test_app_state(temp_dir: &tempfile::TempDir, mode: ServerMode) -> AppState {
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
            mode,
            context: ServerContext::default(),
            public_url: None,
            allowed_origin: None,
        }
    }

    async fn spawn_compressed_test_server(
        temp_dir: &tempfile::TempDir,
        mode: ServerMode,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app_state = test_app_state(temp_dir, mode);
        let app = build_router(app_state, CorsLayer::permissive(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        (format!("{addr}"), handle)
    }

    /// Issues a raw `GET` (no client library in the way of the exact bytes on
    /// the wire) and splits the response into its lowercased header block and
    /// raw body, the same way the admin-auth tests above talk to the server.
    /// A compressed response has no length known up front, so axum sends it
    /// `Transfer-Encoding: chunked`; this unwraps that framing so callers see
    /// the same payload bytes a real client's HTTP stack would hand them.
    async fn raw_http_get(
        addr: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
    ) -> (String, Vec<u8>) {
        let url = Url::parse(&format!("http://{addr}{path}")).expect("url");
        let host = url.host_str().expect("host");
        let port = url.port().expect("port");
        let mut stream = tokio::net::TcpStream::connect((host, port))
            .await
            .expect("connect");
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n");
        for (name, value) in extra_headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read");
        let split_at = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header/body separator");
        let headers = String::from_utf8_lossy(&buf[..split_at]).to_ascii_lowercase();
        let raw_body = &buf[split_at + 4..];
        let body = if headers.contains("transfer-encoding: chunked") {
            dechunk(raw_body)
        } else {
            raw_body.to_vec()
        };
        (headers, body)
    }

    /// Minimal HTTP/1.1 chunked-transfer-coding decoder (RFC 9112 §7.1) —
    /// just enough to reassemble a `raw_http_get` body for assertions, not a
    /// general-purpose client.
    fn dechunk(mut body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(line_end) = body.windows(2).position(|w| w == b"\r\n") {
            let size_line = std::str::from_utf8(&body[..line_end]).unwrap_or("0");
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            body = &body[line_end + 2..];
            if size == 0 || body.len() < size {
                break;
            }
            out.extend_from_slice(&body[..size]);
            body = &body[size..];
            body = body.strip_prefix(b"\r\n".as_slice()).unwrap_or(body);
        }
        out
    }

    /// `CompressionLayer`'s default predicate skips bodies below a minimum
    /// size (compressing a couple of bytes would grow, not shrink, the
    /// response once gzip's own framing overhead is counted) — so `/health`'s
    /// two-byte `"ok"` body is *correctly* left alone even with the layer
    /// present and `Accept-Encoding: gzip` on the request. This test pins
    /// that expectation; `large_json_response_is_gzip_compressed_when_accepted`
    /// below exercises the case the layer exists for.
    #[tokio::test]
    async fn health_response_is_uncompressed_regardless_of_accept_encoding() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let (addr, server) = spawn_compressed_test_server(&temp_dir, ServerMode::Full).await;

        let (headers, body) = raw_http_get(&addr, "/health", &[("Accept-Encoding", "gzip")]).await;

        assert!(
            !headers.contains("content-encoding"),
            "did not expect a Content-Encoding header on a response this small, got:\n{headers}"
        );
        assert_eq!(body, b"ok");

        server.abort();
    }

    /// Exercises `CompressionLayer` against a response large enough to clear
    /// the default minimum-size predicate: a P2P draft backup snapshot, the
    /// same JSON API surface the issue's audit called out as uncompressed at
    /// origin.
    #[tokio::test]
    async fn large_json_response_is_gzip_compressed_when_accepted() {
        const DRAFT_CODE: &str = "GZIP01";
        const HOST_PEER: &str = "peer-compression-test";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir, ServerMode::Full);
        // A highly compressible payload comfortably over the predicate's
        // minimum size, matching the redactor's "JSON object" requirement.
        let padding = "a".repeat(4096);
        let snapshot_json = format!(r#"{{"status":"Drafting","padding":"{padding}"}}"#);
        app_state
            .game_db
            .save_p2p_backup(DRAFT_CODE, HOST_PEER, &snapshot_json)
            .expect("seed backup");
        let uncompressed_len = snapshot_json.len();

        let app = build_router(app_state, CorsLayer::permissive(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        let addr = format!("{addr}");

        let (headers, body) = raw_http_get(
            &addr,
            &format!("/p2p-draft-backup/{DRAFT_CODE}?host_peer_id={HOST_PEER}"),
            &[("Accept-Encoding", "gzip")],
        )
        .await;

        assert!(
            headers.contains("content-encoding: gzip"),
            "expected a gzip Content-Encoding header, got:\n{headers}"
        );
        assert_eq!(
            body.get(..2),
            Some([0x1f, 0x8b].as_slice()),
            "response body does not start with the gzip magic bytes"
        );
        assert!(
            body.len() < uncompressed_len,
            "gzip body ({} bytes) should be smaller than the uncompressed JSON it wraps ({} bytes)",
            body.len(),
            uncompressed_len
        );

        server.abort();
    }

    /// Guards the exact regression this fix must not introduce — a
    /// `CompressionLayer` applied broadly enough to wrap the `/ws` upgrade
    /// response. `build_router` keeps `/ws` on its own uncompressed
    /// sub-router; this asserts the handshake still completes when served
    /// from the very `Router` that also carries compression.
    #[tokio::test]
    async fn ws_upgrade_still_succeeds_through_the_compressed_router() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let (addr, server) = spawn_compressed_test_server(&temp_dir, ServerMode::Full).await;

        let connected = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
        assert!(
            connected.is_ok(),
            "WebSocket upgrade failed through the compressed router: {:?}",
            connected.err()
        );

        server.abort();
    }

    /// Recorded announce requests: the header map and the raw body bytes of
    /// every POST the directory stub received.
    type AnnounceLog = Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, Vec<u8>)>>>;

    /// A directory stub on an ephemeral port that records every announce POST
    /// and answers `status`. Inlines the listener+spawn shape
    /// `large_json_response_is_gzip_compressed_when_accepted` uses.
    async fn spawn_recording_directory(
        status: axum::http::StatusCode,
    ) -> (Url, AnnounceLog, tokio::task::JoinHandle<()>) {
        let log: AnnounceLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_log = log.clone();
        let app = axum::Router::new().route(
            "/servers/announce",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let log = handler_log.clone();
                    async move {
                        log.lock()
                            .expect("announce log")
                            .push((headers, body.to_vec()));
                        status
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind directory stub");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("directory stub");
        });
        let endpoint =
            Url::parse(&format!("http://{addr}/servers/announce")).expect("directory endpoint");
        (endpoint, log, server)
    }

    /// Polls to a deadline rather than sleeping a fixed interval, so a slow
    /// machine does not turn a passing case into a flake.
    async fn wait_for_requests(log: &AnnounceLog, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let count = log.lock().expect("announce log").len();
            if count >= want {
                return count;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {count} of {want} announce requests arrived within 2s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Every recorded request must carry the JSON content type and a body the
    /// directory's OWN validator accepts — not merely well-shaped bytes.
    /// Returns each request's announced player count, in arrival order.
    fn recorded_player_counts(log: &AnnounceLog) -> Vec<u32> {
        let records = log.lock().expect("announce log");
        assert!(!records.is_empty(), "no announce request was recorded");
        records
            .iter()
            .map(|(headers, body)| {
                assert_eq!(
                    headers
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    Some("application/json")
                );
                let raw: RawAnnouncement =
                    serde_json::from_slice(body).expect("wire body is a RawAnnouncement");
                validate_announcement(&raw)
                    .expect("the directory's own validator accepts the wire body")
                    .current_players()
            })
            .collect()
    }

    /// V11. The info route serves exactly the identity `ServerHello`
    /// advertises, on the shared constant path.
    #[tokio::test]
    async fn info_route_serves_the_server_hello_identity() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let (addr, server) = spawn_compressed_test_server(&temp_dir, ServerMode::Full).await;

        let (headers, body) = raw_http_get(&addr, INFO_PATH, &[]).await;
        // Reach-guard: without this a 404 body would be "tested" as a serde
        // failure instead of failing here.
        assert!(
            headers.starts_with("http/1.1 200"),
            "expected 200 on {INFO_PATH}, got:\n{headers}"
        );
        let document: ServerInfoDocument =
            serde_json::from_slice(&body).expect("info document parses");
        assert_eq!(document.mode, lobby_broker::ServerMode::Full);
        assert_eq!(document.server_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(document.protocol_version, PROTOCOL_VERSION);
        assert_eq!(document.lobby_protocol_version, LOBBY_PROTOCOL_VERSION);
        assert_eq!(document.build_commit.as_deref(), Some(build_commit()));
        server.abort();

        // The sibling: `mode` tracks this server's actual role, so a hard-coded
        // "Full" fails here.
        let (addr, server) = spawn_compressed_test_server(&temp_dir, ServerMode::LobbyOnly).await;
        let (headers, body) = raw_http_get(&addr, INFO_PATH, &[]).await;
        assert!(
            headers.starts_with("http/1.1 200"),
            "expected 200 on {INFO_PATH}, got:\n{headers}"
        );
        let document: ServerInfoDocument =
            serde_json::from_slice(&body).expect("info document parses");
        assert_eq!(document.mode, lobby_broker::ServerMode::LobbyOnly);
        server.abort();
    }

    /// V14. The announcement is assembled from live `AppState`, and only from
    /// an `https://` public URL.
    #[test]
    fn announcement_from_state_carries_url_mode_and_live_player_count() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        // LobbyOnly rather than Full, so a hard-coded mode fails here.
        let mut state = test_app_state(&temp_dir, ServerMode::LobbyOnly);
        state.public_url = Some("https://play.example.com".to_string());
        // Non-zero, so a stubbed-zero implementation fails.
        state.player_count.store(7, Ordering::Relaxed);

        let announcement = announcement_from_state(&state).expect("state is announceable");
        assert_eq!(announcement.url().as_str(), "wss://play.example.com/ws");
        assert_eq!(announcement.name(), "play.example.com");
        assert_eq!(announcement.mode(), lobby_broker::ServerMode::LobbyOnly);
        assert_eq!(announcement.current_players(), 7);
        assert_eq!(announcement.directory_version(), DIRECTORY_VERSION);

        // A non-default port survives; the scheme default does not.
        state.public_url = Some("https://play.example.com:8443".to_string());
        assert_eq!(
            announcement_from_state(&state)
                .expect("state is announceable")
                .url()
                .as_str(),
            "wss://play.example.com:8443/ws"
        );
        state.public_url = Some("https://play.example.com:443".to_string());
        assert_eq!(
            announcement_from_state(&state)
                .expect("state is announceable")
                .url()
                .as_str(),
            "wss://play.example.com/ws"
        );

        // (a) Reachable whenever a malformed `--public-url` was dropped by
        // `validate_public_url`, not only via the ngrok fallback.
        state.public_url = None;
        assert!(announcement_from_state(&state).is_err());
        // (b) Without this the server would announce an address it does not
        // serve, and warn every 60 s forever.
        state.public_url = Some("http://myserver.example:9374".to_string());
        let error =
            announcement_from_state(&state).expect_err("an http:// public URL is not announceable");
        assert!(
            error.contains("http"),
            "the scheme found must be named: {error}"
        );
        // (c) The silent-default-port case: `Url::port()` is `None` at :80, so
        // this would otherwise become wss://host.example/ws, claiming 443.
        state.public_url = Some("http://host.example:80".to_string());
        assert!(announcement_from_state(&state).is_err());

        // A host longer than the NAME cap but well inside the URL cap must
        // still announce: the name is truncated, the ADDRESS is not. Without
        // the truncation this returns `Err` and the server never announces at
        // all. Every label is 60 bytes (inside rule 8's 63) and the whole host
        // is 121 characters — a valid DNS name that exceeds
        // MAX_SERVER_NAME_LEN.
        let long_label = "a".repeat(60);
        let long_host = format!("{long_label}.{long_label}");
        assert!(long_host.chars().count() > MAX_SERVER_NAME_LEN);
        state.public_url = Some(format!("https://{long_host}"));
        let announcement =
            announcement_from_state(&state).expect("a long but legal host is still announceable");
        assert_eq!(announcement.name().len(), MAX_SERVER_NAME_LEN);
        assert!(long_host.starts_with(announcement.name()));
        assert_eq!(
            announcement.url().as_str(),
            format!("wss://{long_host}/ws"),
            "the address must keep the FULL host; only the display name is truncated"
        );
    }

    /// V16. The heartbeat actually POSTs: right shape, right cadence, and a
    /// directory that rejects forever does not kill it.
    #[tokio::test]
    async fn announce_task_posts_json_repeatedly_and_survives_failures() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut state = test_app_state(&temp_dir, ServerMode::Full);
        // `test_app_state` leaves `public_url: None`, which would make the base
        // unbuildable and leave this test recording ZERO requests while every
        // ">= 2"-shaped assertion below passed unreached.
        state.public_url = Some("https://play.example.com".to_string());
        state.player_count.store(3, Ordering::Relaxed);
        let base = announcement_from_state(&state).expect("fixture must be announceable");
        // Built the way `serve()` builds it, so the test drives the real
        // parameterised loop rather than a differently-configured client.
        // `reqwest::Client` is a cheap `Arc` handle, so cloning it per case is
        // what sharing one configured client looks like.
        let client = reqwest::Client::builder()
            .timeout(ANNOUNCE_REQUEST_TIMEOUT)
            .build()
            .expect("announce client builds");

        // (A) `interval`'s first tick is immediate, so a 30 s period still
        // announces at startup — pinned without a 30 s wait.
        let (endpoint, log, recorder) = spawn_recording_directory(StatusCode::OK).await;
        let handle = spawn_announce_task(
            endpoint,
            client.clone(),
            base.clone(),
            state.player_count.clone(),
            std::time::Duration::from_secs(30),
        );
        wait_for_requests(&log, 1).await;
        assert_eq!(recorded_player_counts(&log), vec![3]);
        handle.abort();
        recorder.abort();

        // (B) A directory returning 500 forever: the loop keeps announcing and
        // the task stays alive. A `?` or an early return on the error path
        // fails this.
        let (endpoint, log, recorder) =
            spawn_recording_directory(StatusCode::INTERNAL_SERVER_ERROR).await;
        let handle = spawn_announce_task(
            endpoint,
            client.clone(),
            base.clone(),
            state.player_count.clone(),
            std::time::Duration::from_millis(25),
        );
        let seen = wait_for_requests(&log, 2).await;
        assert!(seen >= 2, "expected a repeating heartbeat, saw {seen}");
        assert!(
            !handle.is_finished(),
            "a directory rejecting every announcement must not end the heartbeat"
        );
        // The live count is re-read per tick rather than replayed from the
        // frozen base.
        state.player_count.store(9, Ordering::Relaxed);
        wait_for_requests(&log, seen + 2).await;
        assert_eq!(
            recorded_player_counts(&log).last().copied(),
            Some(9),
            "the loop must refresh the player count, not replay the base"
        );
        handle.abort();
        recorder.abort();

        // (C) Paired positive: (B) is not an artifact of the error path — the
        // success path loops too.
        state.player_count.store(3, Ordering::Relaxed);
        let (endpoint, log, recorder) = spawn_recording_directory(StatusCode::OK).await;
        let handle = spawn_announce_task(
            endpoint,
            client.clone(),
            base,
            state.player_count.clone(),
            std::time::Duration::from_millis(25),
        );
        let seen = wait_for_requests(&log, 2).await;
        assert!(seen >= 2, "expected a repeating heartbeat, saw {seen}");
        assert!(!handle.is_finished());
        assert!(recorded_player_counts(&log).iter().all(|count| *count == 3));
        handle.abort();
        recorder.abort();
    }
}

#[cfg(test)]
mod p2p_backup_delete_tests {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::{Path, Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use lobby_broker::Broker;
    use server_core::draft_session::DraftSessionManager;
    use server_core::session::SessionManager;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    use url::Url;

    use super::{admin, draft_pools, persistence, AppState, ServerContext, ServerMode};

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
            context: ServerContext::default(),
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
    async fn public_backup_store_and_get_redact_chaos_assignments() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let app_state = test_app_state(&temp_dir);
        let nested = serde_json::json!({
            "config": {
                "source": {
                    "type": "Set",
                    "data": {
                        "candidate_codes": ["TST", "ALT"],
                        "assignments": [["TST", "ALT"], ["ALT", "TST"]]
                    }
                }
            }
        });
        let snapshot = serde_json::json!({ "draftSessionJson": nested.to_string() }).to_string();

        let stored = admin::p2p_backup_store(
            State(app_state.clone()),
            Json(admin::P2pBackupRequest {
                draft_code: DRAFT_CODE.to_string(),
                host_peer_id: HOST_PEER.to_string(),
                snapshot_json: snapshot,
            }),
        )
        .await
        .into_response();
        assert_eq!(stored.status(), StatusCode::OK);

        let (_, stored_snapshot, _) = app_state
            .game_db
            .load_p2p_backup(DRAFT_CODE)
            .expect("load")
            .expect("stored backup");
        let stored_value: serde_json::Value =
            serde_json::from_str(&stored_snapshot).expect("snapshot JSON");
        let stored_session: serde_json::Value = serde_json::from_str(
            stored_value["draftSessionJson"]
                .as_str()
                .expect("session JSON string"),
        )
        .expect("session JSON");
        let stored_source = &stored_session["config"]["source"];
        assert_eq!(stored_source["type"], "Set");
        let stored_layout = &stored_source["data"];
        assert!(stored_layout.get("assignments").is_none());
        assert_eq!(
            stored_layout["candidate_codes"],
            serde_json::json!(["TST", "ALT"])
        );

        let response = admin::p2p_backup_get(
            State(app_state),
            Path(DRAFT_CODE.to_string()),
            Query(admin::P2pBackupGetQuery {
                host_peer_id: HOST_PEER.to_string(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let public_response: serde_json::Value =
            serde_json::from_slice(&body).expect("response JSON");
        let public_snapshot: serde_json::Value = serde_json::from_str(
            public_response["snapshot_json"]
                .as_str()
                .expect("snapshot JSON string"),
        )
        .expect("snapshot JSON");
        let public_session: serde_json::Value = serde_json::from_str(
            public_snapshot["draftSessionJson"]
                .as_str()
                .expect("session JSON string"),
        )
        .expect("session JSON");
        assert_eq!(public_session["config"]["source"]["type"], "Set");
        assert!(public_session["config"]["source"]["data"]
            .get("assignments")
            .is_none());
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

#[cfg(test)]
mod metrics_tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::routing::get;
    use axum::Router;
    use engine::database::CardDatabase;
    use engine::game::deck_loading::PlayerDeckPayload;
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    use lobby_broker::Broker;
    use phase_ai::config::AiDifficulty;
    use server_core::draft_session::DraftSessionManager;
    use server_core::protocol::{AiSeatRequest, ClientMessage, DeckData, ServerMessage};
    use server_core::session::{GameSession, SessionManager};
    use tokio::sync::{mpsc, Mutex};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::metrics::{self, RejectReason};
    use super::{
        build_commit, draft_pools, persistence, AppState, ConnectionSlot, Limits, ServerContext,
        ServerMode, SharedPlayerCount, LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION,
    };

    fn app_state(temp_dir: &tempfile::TempDir, context: ServerContext) -> AppState {
        let game_db = Arc::new(
            persistence::GameDb::open(
                &temp_dir.path().join("games.db"),
                persistence::SessionRetention::Multiplayer,
            )
            .expect("game db"),
        );
        AppState {
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
            context,
            public_url: None,
            allowed_origin: None,
        }
    }

    /// A sender whose receiver has been dropped — exactly what a departed
    /// socket leaves behind in `connections`, since the disconnect path does
    /// not remove the entry.
    fn dead_sender() -> mpsc::UnboundedSender<ServerMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        tx
    }

    async fn spawn(app_state: AppState) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/ws", get(super::ws_handler))
            .route("/metrics", get(metrics::handler))
            .with_state(app_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        (addr.to_string(), handle)
    }

    /// Occupancy is the metric a scale-in decision reads, so it must track live
    /// sockets rather than map membership. Both wrong implementations are
    /// represented here: `sessions.len()` would answer 3 and `connections.len()`
    /// would answer 2, while only one game actually has a human on it.
    #[tokio::test]
    async fn occupancy_counts_only_games_holding_a_live_socket() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = app_state(&temp, ServerContext::default());

        let (occupied, abandoned, empty) = {
            let mut mgr = state.sessions.lock().await;
            let (a, _) = mgr.create_game(PlayerDeckPayload::default(), None);
            let (b, _) = mgr.create_game(PlayerDeckPayload::default(), None);
            let (c, _) = mgr.create_game(PlayerDeckPayload::default(), None);
            (a, b, c)
        };

        let (live_tx, _live_rx) = mpsc::unbounded_channel();
        let (retired_tx, _retired_rx) = mpsc::unbounded_channel();
        {
            let mut conns = state.connections.lock().await;
            conns.insert(
                occupied.clone(),
                HashMap::from([(engine::types::player::PlayerId(0), live_tx)]),
            );
            // A player who left: the key survives, the channel does not.
            conns.insert(
                abandoned.clone(),
                HashMap::from([(engine::types::player::PlayerId(0), dead_sender())]),
            );
            // A game that was retired while its connection entry lingered:
            // must not be counted, and must not be invented as an active game.
            conns.insert(
                "RETIRED".to_string(),
                HashMap::from([(engine::types::player::PlayerId(0), retired_tx)]),
            );
        }
        assert!(!empty.is_empty(), "third game code was created");

        let snapshot = metrics::collect(&state).await;
        assert_eq!(snapshot.games_active, 3);
        assert_eq!(snapshot.games_with_connected_humans, 1);
    }

    /// A game watched only by a spectator still pins this replica: the
    /// spectator socket lives in a different map from player connections.
    #[tokio::test]
    async fn a_spectator_only_game_counts_as_occupied() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = app_state(&temp, ServerContext::default());

        let code = {
            let mut mgr = state.sessions.lock().await;
            let (code, _) = mgr.create_game(PlayerDeckPayload::default(), None);
            code
        };

        // Baseline: no sockets at all, so the game is not occupied. Without
        // this the assertion below could pass on an implementation that counts
        // every session.
        assert_eq!(
            metrics::collect(&state).await.games_with_connected_humans,
            0
        );

        let (spectator_tx, _spectator_rx) = mpsc::unbounded_channel();
        state
            .game_spectators
            .lock()
            .await
            .insert(code, vec![spectator_tx]);

        let snapshot = metrics::collect(&state).await;
        assert_eq!(snapshot.games_active, 1);
        assert_eq!(snapshot.games_with_connected_humans, 1);
    }

    /// Refusing an upgrade at the connection cap must be visible to a scraper,
    /// and an ordinary connect must not look like a refusal.
    #[tokio::test]
    async fn connection_cap_refusal_is_counted_but_an_accepted_socket_is_not() {
        let temp = tempfile::tempdir().expect("temp dir");
        let context = ServerContext {
            limits: Limits {
                max_connections: 1,
                ..Limits::default()
            },
            ..ServerContext::default()
        };
        let counters = context.metrics.clone();
        let state = app_state(&temp, context);
        let player_count = state.player_count.clone();
        let (addr, server) = spawn(state).await;

        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            // Control: the first socket is admitted. This proves the endpoint
            // works, so the refusal below is the cap and not a broken server.
            let (mut socket, response) =
                tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
                    .await
                    .expect("first connect is admitted");
            assert_eq!(response.status().as_u16(), 101);
            let hello = recv(&mut socket).await;
            assert!(
                matches!(hello, ServerMessage::ServerHello { .. }),
                "admitted socket got {hello:?}"
            );
            assert_eq!(counters.reject_count(RejectReason::ConnectionLimit), 0);

            // The gate reads `player_count`, which the accepted socket
            // increments from its own task; wait for that rather than racing it.
            while player_count.load(std::sync::atomic::Ordering::Relaxed) < 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            let refused = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
                .await
                .expect_err("second connect is over the cap");
            match refused {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    assert_eq!(response.status().as_u16(), 503);
                }
                other => panic!("expected an HTTP 503, got {other:?}"),
            }
            assert_eq!(counters.reject_count(RejectReason::ConnectionLimit), 1);
            // Only the reason that fired moves.
            assert_eq!(counters.reject_count(RejectReason::GameLimit), 0);
            assert_eq!(counters.reject_count(RejectReason::OriginNotAllowed), 0);
        })
        .await;
        server.abort();
        outcome.expect("connection cap test timed out");
    }

    /// `--max-games` has to bind at the real `CreateGame` path, not just exist
    /// as a field. Driven over a websocket so the whole production route runs.
    #[tokio::test]
    async fn create_game_past_the_cap_is_refused_and_counted() {
        let temp = tempfile::tempdir().expect("temp dir");
        let context = ServerContext {
            limits: Limits {
                max_games: 1,
                ..Limits::default()
            },
            ..ServerContext::default()
        };
        let counters = context.metrics.clone();
        let (addr, server) = spawn(app_state(&temp, context)).await;

        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let first = create_game(&addr, "Alice").await;
            assert!(
                matches!(first, ServerMessage::GameCreated { .. }),
                "the first create is under the cap, got {first:?}"
            );
            assert_eq!(counters.reject_count(RejectReason::GameLimit), 0);

            let second = create_game(&addr, "Bob").await;
            match second {
                ServerMessage::Error { ref message, .. } => {
                    assert!(
                        message.contains("game capacity"),
                        "unexpected error text: {message}"
                    );
                }
                other => panic!("expected a capacity error, got {other:?}"),
            }
            assert_eq!(counters.reject_count(RejectReason::GameLimit), 1);
        })
        .await;
        server.abort();
        outcome.expect("max-games test timed out");
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_prometheus_text() {
        let temp = tempfile::tempdir().expect("temp dir");
        let context = ServerContext {
            replica_ordinal: Some(2),
            ..ServerContext::default()
        };
        let (addr, server) = spawn(app_state(&temp, context)).await;

        let response = reqwest::get(format!("http://{addr}/metrics"))
            .await
            .expect("scrape /metrics");
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        let body = response.text().await.expect("body");
        server.abort();

        for family in [
            "phase_connections",
            "phase_connections_capacity",
            "phase_games_active",
            "phase_games_with_connected_humans",
            "phase_games_capacity",
            "phase_drafts_active",
            "phase_drafts_with_connected_humans",
            "phase_replica_ordinal",
            "phase_admission_rejects_total",
            "phase_build_info",
        ] {
            assert!(
                body.contains(&format!("# TYPE {family} ")),
                "{family} missing from scrape:\n{body}"
            );
        }
        assert!(body.contains("phase_replica_ordinal 2\n"));
        assert!(body.contains(&format!(
            "phase_connections_capacity {}\n",
            super::DEFAULT_MAX_CONNECTIONS
        )));
    }

    async fn recv<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        match socket.next().await.expect("frame").expect("ok frame") {
            WsMessage::Text(text) => serde_json::from_str(&text).expect("server message"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    type TestSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// Connect and complete the handshake, leaving the socket ready to create.
    async fn connect_and_hello(addr: &str) -> TestSocket {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("connect");
        assert!(matches!(
            recv(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ));

        let hello = ClientMessage::ClientHello {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            build_commit: build_commit().to_string(),
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            wire_formats: Vec::new(),
        };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&hello).expect("hello json").into(),
            ))
            .await
            .expect("send hello");
        socket
    }

    /// Send one create request; returns the first message that is not a slot or
    /// count broadcast.
    async fn send_create(socket: &mut TestSocket, request: ClientMessage) -> ServerMessage {
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&request).expect("create json").into(),
            ))
            .await
            .expect("send create");

        loop {
            match recv(socket).await {
                ServerMessage::PlayerSlotsUpdate { .. } | ServerMessage::PlayerCount { .. } => {}
                other => return other,
            }
        }
    }

    fn settings_request(
        display_name: &str,
        deck: DeckData,
        ai_seats: Vec<AiSeatRequest>,
    ) -> ClientMessage {
        ClientMessage::CreateGameWithSettings {
            deck,
            display_name: display_name.to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: Default::default(),
            ai_seats,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            start_when_full: true,
            ranked: false,
        }
    }

    async fn create_game(addr: &str, display_name: &str) -> ServerMessage {
        let mut socket = connect_and_hello(addr).await;
        send_create(
            &mut socket,
            settings_request(display_name, DeckData::default(), Vec::new()),
        )
        .await
    }

    /// Connects every client and gets it through the handshake first, then
    /// releases them all into one simultaneous create.
    ///
    /// The barrier is what makes the caller discriminating: creates that arrive
    /// spread out are serialized by the sessions lock, so a path that checks
    /// capacity, releases the lock, and inserts later would look correct by
    /// luck. Releasing them together puts every racer inside that window.
    async fn race_creates(addr: &str, requests: Vec<ClientMessage>) -> Vec<ServerMessage> {
        let barrier = Arc::new(tokio::sync::Barrier::new(requests.len()));
        let mut racers = Vec::new();
        for request in requests {
            let mut socket = connect_and_hello(addr).await;
            let barrier = barrier.clone();
            racers.push(tokio::spawn(async move {
                barrier.wait().await;
                send_create(&mut socket, request).await
            }));
        }

        let mut replies = Vec::new();
        for racer in racers {
            replies.push(racer.await.expect("create task"));
        }
        replies
    }

    fn tally(replies: &[ServerMessage], path: &str) -> (u64, u64) {
        let mut created = 0;
        let mut refused = 0;
        for reply in replies {
            match reply {
                ServerMessage::GameCreated { .. } | ServerMessage::GameStarted { .. } => {
                    created += 1
                }
                ServerMessage::Error { message, .. } if message.contains("game capacity") => {
                    refused += 1
                }
                other => panic!("{path}: unexpected reply {other:?}"),
            }
        }
        (created, refused)
    }

    /// The cap has to be enforced by the same atomic step that takes the slot.
    /// Loading `player_count`, comparing it, and incrementing after the upgrade
    /// admits every handshake that raced into the gap, so a cap of one is
    /// overshot by however many arrive together.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_upgrades_cannot_exceed_the_connection_cap() {
        const RACERS: u64 = 8;
        let temp = tempfile::tempdir().expect("temp dir");
        let context = ServerContext {
            limits: Limits {
                max_connections: 1,
                ..Limits::default()
            },
            ..ServerContext::default()
        };
        let counters = context.metrics.clone();
        let state = app_state(&temp, context);
        let player_count = state.player_count.clone();
        let (addr, server) = spawn(state).await;

        let barrier = Arc::new(tokio::sync::Barrier::new(RACERS as usize));
        let mut racers = Vec::new();
        for _ in 0..RACERS {
            let addr = addr.clone();
            let barrier = barrier.clone();
            racers.push(tokio::spawn(async move {
                barrier.wait().await;
                tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await
            }));
        }

        let outcome = tokio::time::timeout(Duration::from_secs(30), async {
            let mut admitted = Vec::new();
            let mut refused = 0u64;
            for racer in racers {
                match racer.await.expect("connect task") {
                    Ok((socket, response)) => {
                        assert_eq!(response.status().as_u16(), 101);
                        // Held open: a socket that closed would give its slot
                        // back and hide an over-admission from the count below.
                        admitted.push(socket);
                    }
                    Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                        assert_eq!(response.status().as_u16(), 503);
                        refused += 1;
                    }
                    Err(other) => panic!("expected an HTTP 503, got {other:?}"),
                }
            }
            (admitted, refused)
        })
        .await;

        let (admitted, refused) = outcome.expect("connection race timed out");
        assert_eq!(
            admitted.len(),
            1,
            "more than one racer took the single slot"
        );
        assert_eq!(refused, RACERS - 1);
        assert_eq!(
            player_count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "reservations outnumber the cap"
        );
        assert_eq!(
            counters.reject_count(RejectReason::ConnectionLimit),
            RACERS - 1
        );
        // Torn down only after `player_count` is read. `on_upgrade` spawns its
        // callback as an independent task, and that callback owns the armed
        // `ConnectionSlot` until `handle_socket` disarms it. The callback only
        // runs once `hyper::upgrade::OnUpgrade` resolves, which is driven by the
        // connection task inside `server` — so aborting first makes the upgrade
        // fail, and axum drops the closure, and with it the still-armed guard,
        // without ever calling `handle_socket`. `Drop` then releases the
        // reservation and the assertion above reads 0 instead of 1. The racer
        // has already been handed its 101 by that point, so the client sees a
        // live connection either way; the held-open sockets keep the
        // reservation alive on their own and nothing here needs the server
        // stopped first. The `reject_count` reads are safe in either order,
        // since rejection counters are monotonic and no release path touches
        // them — `player_count` is the only counter teardown mutates.
        server.abort();
    }

    /// Every full-mode creation path must check capacity under the same lock
    /// acquisition that inserts the session. All three are driven here over the
    /// real websocket route, because each reaches a different check: the one in
    /// the `CreateGame` arm, the one in `create_and_connect_multiplayer_session`,
    /// and the one in the AI branch of `CreateGameWithSettings`. Two of them
    /// share an insert (`create_game_n_players`), so the insert does not
    /// identify the path — the check does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_creates_cannot_exceed_the_game_cap() {
        const RACERS: u64 = 8;
        for path in [
            "create_game_arm",
            "create_and_connect_multiplayer_session",
            "create_game_with_settings_ai_branch",
        ] {
            let temp = tempfile::tempdir().expect("temp dir");
            let context = ServerContext {
                limits: Limits {
                    max_games: 1,
                    ..Limits::default()
                },
                ..ServerContext::default()
            };
            let counters = context.metrics.clone();
            let state = app_state(&temp, context);
            let sessions = state.sessions.clone();
            let (addr, server) = spawn(state).await;

            let deck = DeckData::default();
            let requests = (0..RACERS)
                .map(|racer| match path {
                    "create_game_arm" => ClientMessage::CreateGame { deck: deck.clone() },
                    "create_game_with_settings_ai_branch" => settings_request(
                        &format!("racer{racer}"),
                        deck.clone(),
                        // Empty list for the same reason as the AI-game
                        // harness above: this server has an empty card
                        // database, so only an empty deck resolves.
                        vec![AiSeatRequest {
                            seat_index: 1,
                            difficulty: AiDifficulty::Medium,
                            deck_name: None,
                            deck: Some(seat_reducer::types::DeckChoice::DeckList(Box::default())),
                        }],
                    ),
                    _ => settings_request(&format!("racer{racer}"), deck.clone(), Vec::new()),
                })
                .collect();

            let outcome =
                tokio::time::timeout(Duration::from_secs(30), race_creates(&addr, requests)).await;
            server.abort();
            let replies = outcome.unwrap_or_else(|_| panic!("{path}: create race timed out"));

            let (created, refused) = tally(&replies, path);
            assert_eq!(
                created, 1,
                "{path}: more than one create took the last slot"
            );
            assert_eq!(refused, RACERS - 1, "{path}");
            let survivor = {
                let mgr = sessions.lock().await;
                assert_eq!(mgr.sessions.len(), 1, "{path}: sessions past the cap");
                mgr.sessions
                    .values()
                    .next()
                    .expect("one session")
                    .ai_seats
                    .len()
            };
            // Which insert actually ran: only `create_game_with_ai` seats an AI.
            // Without this the AI case would silently exercise the multiplayer
            // path if seat validation ever rejected the request.
            assert_eq!(
                survivor,
                usize::from(path == "create_game_with_settings_ai_branch"),
                "{path}: wrong creation path ran"
            );
            assert_eq!(
                counters.reject_count(RejectReason::GameLimit),
                RACERS - 1,
                "{path}"
            );
        }
    }

    /// Snapshot a live room and rebuild it the way a process restart does.
    async fn restart(sessions: &super::SharedState, game_code: &str) -> GameSession {
        let persisted = {
            let mgr = sessions.lock().await;
            mgr.sessions
                .get(game_code)
                .expect("the room is live")
                .to_persisted()
        };
        GameSession::from_persisted(persisted, &Arc::new(CardDatabase::default()))
            .expect("the snapshot restores")
    }

    /// Both legacy seat installs must record the unresolved deck they were
    /// filled from: a restart rebuilds `decks` from `deck_choices` alone, and a
    /// seat with no provenance restores empty and `start_game` refuses the
    /// table. Driven over the socket rather than against the setters, because
    /// the setters were already correct and these two entry points were not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn legacy_create_and_join_record_their_seats_deck_provenance() {
        let temp = tempfile::tempdir().expect("temp dir");
        let state = app_state(&temp, ServerContext::default());
        let sessions = state.sessions.clone();
        let (addr, server) = spawn(state).await;

        let mut host = connect_and_hello(&addr).await;
        let created = send_create(
            &mut host,
            ClientMessage::CreateGame {
                deck: DeckData::default(),
            },
        )
        .await;
        let ServerMessage::GameCreated { game_code, .. } = created else {
            panic!("CreateGame did not create a room: {created:?}");
        };

        // Seat 1 is the one nobody filled. A host recorded without its list
        // names seat 0 here instead, and seat 0 is `SeatImmutable` — no reseat
        // can clear it, so that room never starts again.
        assert_eq!(
            restart(&sessions, &game_code)
                .await
                .start_game(&Arc::new(CardDatabase::default())),
            Err(server_core::session::StartGameError::SeatDeckMissing { seat_index: 1 }),
            "the host seat lost its deck across a restart"
        );

        let mut guest = connect_and_hello(&addr).await;
        let joined = send_create(
            &mut guest,
            ClientMessage::JoinGame {
                game_code: game_code.clone(),
                deck: DeckData::default(),
            },
        )
        .await;
        assert!(
            !matches!(joined, ServerMessage::Error { .. }),
            "JoinGame was refused: {joined:?}"
        );

        // With both seats recorded the restored room is startable — the row
        // above is the paired control proving this one is not vacuous.
        assert_eq!(
            restart(&sessions, &game_code)
                .await
                .start_game(&Arc::new(CardDatabase::default())),
            Ok(())
        );

        server.abort();
    }

    /// A reservation whose upgrade never reaches `handle_socket` has to be
    /// given back, or the server sits one slot below capacity for good.
    /// Disarming is what hands that release to `handle_socket` instead.
    #[test]
    fn a_dropped_connection_slot_releases_its_reservation() {
        let counter: SharedPlayerCount = Arc::new(AtomicU32::new(1));

        drop(ConnectionSlot::new(counter.clone()));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);

        counter.store(1, std::sync::atomic::Ordering::Relaxed);
        ConnectionSlot::new(counter.clone()).disarm();
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
