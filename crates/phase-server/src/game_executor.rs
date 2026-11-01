//! CPU admission for game work, separate from socket and HTTP liveness.
//!
//! A session's owned mutex guard is its ordering/publication lease. Move that
//! guard through blocking work, then retain it until the resulting messages and
//! persistence work have been queued. No worker waits for socket delivery.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::GameLogEntry;
use server_core::game_state_snapshot_wire_guard::{
    guard_state_snapshot_broadcast, StateSnapshotParts, MAX_SNAPSHOT_EVENTS,
    MAX_SNAPSHOT_LOG_ENTRIES,
};
use server_core::protocol::ServerMessage;
use server_core::session::{
    ActionResult, AiRunOutcome, AiTransition, DecisionSnapshot, GameSession, SessionManager,
};
use server_core::takeback::RewindOption;
use server_core::FullSessionKey;
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tracing::warn;

#[cfg(test)]
use tokio::sync::mpsc;

use crate::{
    build_spectator_state_update_message, build_state_update_message, remove_game_spectator_sender,
    SharedConnections, SharedGameSpectators, SharedState,
};

#[cfg(test)]
type AiStepHook = Arc<dyn Fn(&GameSession) + Send + Sync>;

#[cfg(test)]
type PublicationGate =
    Arc<parking_lot::Mutex<Option<Arc<crate::socket_test_hooks::BootstrapGate>>>>;

#[cfg(test)]
type PersistenceNotice = Arc<parking_lot::Mutex<Option<Arc<tokio::sync::Notify>>>>;

#[derive(Clone)]
pub(crate) struct GameExecutor {
    permits: Arc<Semaphore>,
    persistence_io: Arc<Semaphore>,
    #[cfg(test)]
    ai_step_hook: Option<AiStepHook>,
    #[cfg(test)]
    historical_publication: PublicationGate,
    #[cfg(test)]
    persistence_admission: PersistenceNotice,
    #[cfg(test)]
    persistence_lease: PersistenceNotice,
}

/// A CPU job retains its game's ordering lease. Only persistence scheduling
/// needs the weak handle; individual AI steps incur no handle-cloning cost.
pub(crate) struct SessionJob<'a> {
    session: &'a mut OwnedMutexGuard<GameSession>,
}

impl SessionJob<'_> {
    pub(crate) fn weak_session(&self) -> std::sync::Weak<tokio::sync::Mutex<GameSession>> {
        Arc::downgrade(OwnedMutexGuard::mutex(self.session))
    }
}

impl std::ops::Deref for SessionJob<'_> {
    type Target = GameSession;

    fn deref(&self) -> &Self::Target {
        self.session
    }
}

impl std::ops::DerefMut for SessionJob<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.session
    }
}

impl Default for GameExecutor {
    fn default() -> Self {
        Self::new(Self::default_parallelism())
    }
}

impl GameExecutor {
    pub(crate) fn default_parallelism() -> NonZeroUsize {
        // available_parallelism honors CPU affinity and cgroup quotas.
        let processors = std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1);
        NonZeroUsize::new(processors.saturating_sub(1).max(1)).unwrap()
    }

    pub(crate) fn new(parallelism: NonZeroUsize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(parallelism.get())),
            persistence_io: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            ai_step_hook: None,
            #[cfg(test)]
            historical_publication: Arc::default(),
            #[cfg(test)]
            persistence_admission: Arc::default(),
            #[cfg(test)]
            persistence_lease: Arc::default(),
        }
    }

    pub(crate) async fn run<T, F>(&self, work: F) -> T
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        // Acquire before spawning: waiting games must not occupy blocking-pool
        // threads. Reads, heartbeat controls and metadata encoding never acquire it.
        self.try_run(work).await.expect("game CPU worker panicked")
    }

    /// Preserve worker failures for transport callers that must close explicitly.
    pub(crate) async fn try_run<T, F>(&self, work: F) -> Result<T, tokio::task::JoinError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        Self::run_admitted(self.admit().await, work).await
    }

    async fn admit(&self) -> OwnedSemaphorePermit {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .expect("game CPU admission is never closed")
    }

    async fn run_admitted<T, F>(
        permit: OwnedSemaphorePermit,
        work: F,
    ) -> Result<T, tokio::task::JoinError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            // The job, not its asynchronous waiter, owns admission. Cancelling
            // a connection cannot release capacity while its CPU work continues.
            let _permit = permit;
            span.in_scope(work)
        })
        .await
    }

    pub(crate) async fn with_session<T, F>(
        &self,
        mut session: OwnedMutexGuard<GameSession>,
        work: F,
    ) -> (OwnedMutexGuard<GameSession>, T)
    where
        T: Send + 'static,
        F: FnOnce(&mut SessionJob<'_>) -> T + Send + 'static,
    {
        self.run(move || {
            let result = work(&mut SessionJob {
                session: &mut session,
            });
            (session, result)
        })
        .await
    }

    /// Keep the game lease, but yield FIFO CPU admission after each AI action.
    /// Neither an idle permit waiter nor a complete AI batch owns a blocking
    /// thread while another game has the CPU.
    pub(crate) async fn run_ai(
        &self,
        session: OwnedMutexGuard<GameSession>,
    ) -> (OwnedMutexGuard<GameSession>, AiRunOutcome) {
        match session.begin_ai_run() {
            std::ops::ControlFlow::Break(outcome) => (session, outcome),
            std::ops::ControlFlow::Continue(cursor) => self.continue_ai(session, cursor).await,
        }
    }

    /// Resume a cursor already obtained atomically with a human submission.
    pub(crate) async fn continue_ai(
        &self,
        mut session: OwnedMutexGuard<GameSession>,
        mut cursor: server_core::session::AiRunCursor,
    ) -> (OwnedMutexGuard<GameSession>, AiRunOutcome) {
        loop {
            #[cfg(test)]
            let hook = self.ai_step_hook.clone();
            let (lease, step) = self
                .with_session(session, move |session| {
                    #[cfg(test)]
                    if let Some(hook) = hook {
                        hook(session);
                    }
                    session.advance_ai_run(cursor)
                })
                .await;
            session = lease;
            match step {
                std::ops::ControlFlow::Break(outcome) => return (session, outcome),
                std::ops::ControlFlow::Continue(next) => cursor = next,
            }
        }
    }

    /// Drain host transitions while yielding CPU admission between follow-up AI
    /// decisions. The same session lease spans capture, AI and publication.
    pub(crate) async fn drain_contest(
        &self,
        session: OwnedMutexGuard<GameSession>,
    ) -> (
        OwnedMutexGuard<GameSession>,
        server_core::contest::ContestDrain,
    ) {
        let (session, step) = self
            .with_session(session, |session| session.begin_contest_drain())
            .await;
        match step {
            std::ops::ControlFlow::Break(drain) => (session, drain),
            std::ops::ControlFlow::Continue((transitions, cursor)) => {
                let (session, outcome) = self.continue_ai(session, cursor).await;
                self.with_session(session, move |session| {
                    session.finish_contest_drain(transitions, outcome)
                })
                .await
            }
        }
    }

    #[cfg(test)]
    fn with_ai_step_hook(mut self, hook: impl Fn(&GameSession) + Send + Sync + 'static) -> Self {
        self.ai_step_hook = Some(Arc::new(hook));
        self
    }

    #[cfg(test)]
    fn arm_historical_publication(&self) -> Arc<crate::socket_test_hooks::BootstrapGate> {
        let gate = Arc::new(crate::socket_test_hooks::BootstrapGate::default());
        *self.historical_publication.lock() = Some(gate.clone());
        gate
    }

    #[cfg(test)]
    pub(crate) async fn before_historical_publication(&self) {
        let gate = self.historical_publication.lock().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }

    /// One prepared buffer/writer at a time, independently of game CPU work.
    pub(crate) async fn admit_persistence_io(&self) -> OwnedSemaphorePermit {
        let admission = self.persistence_io.clone().acquire_owned();
        tokio::pin!(admission);
        #[cfg(test)]
        let mut entered = self.persistence_admission.lock().take();
        std::future::poll_fn(|cx| {
            let result = std::future::Future::poll(admission.as_mut(), cx);
            #[cfg(test)]
            if let Some(entered) = entered.take() {
                // Notify only after the real semaphore future has been polled,
                // so tests observe its actual FIFO queue position.
                entered.notify_one();
            }
            result
        })
        .await
        .expect("Full persistence admission is never closed")
    }

    #[cfg(test)]
    fn arm_persistence_admission(&self) -> Arc<tokio::sync::Notify> {
        let entered = Arc::new(tokio::sync::Notify::new());
        *self.persistence_admission.lock() = Some(entered.clone());
        entered
    }

    #[cfg(test)]
    fn arm_persistence_lease(&self) -> Arc<tokio::sync::Notify> {
        let entered = Arc::new(tokio::sync::Notify::new());
        *self.persistence_lease.lock() = Some(entered.clone());
        entered
    }

    #[cfg(test)]
    pub(crate) fn notify_persistence_lease_wait(&self) {
        if let Some(entered) = self.persistence_lease.lock().take() {
            entered.notify_one();
        }
    }

    /// Creation owns unpublished sessions through the registry. CPU admission
    /// must precede that global lock, unlike an existing game's ordering lease.
    pub(crate) async fn with_registry<T, F>(
        &self,
        registry: &SharedState,
        work: F,
    ) -> (OwnedMutexGuard<SessionManager>, T)
    where
        T: Send + 'static,
        F: FnOnce(&mut SessionManager) -> T + Send + 'static,
    {
        let permit = self.admit().await;
        let mut registry = registry.clone().lock_owned().await;
        Self::run_admitted(permit, move || {
            let result = work(&mut registry);
            (registry, result)
        })
        .await
        .expect("game creation CPU worker panicked")
    }
}

/// Borrow a complete decision without cloning its action and cost domains.
#[derive(Clone, Copy)]
pub(crate) struct DecisionInputs<'a> {
    pub(crate) legal_actions: &'a [GameAction],
    pub(crate) spell_costs: &'a HashMap<ObjectId, ManaCost>,
    pub(crate) by_object: &'a HashMap<ObjectId, Vec<GameAction>>,
}

impl<'a> From<&'a DecisionSnapshot> for DecisionInputs<'a> {
    fn from(decision: &'a DecisionSnapshot) -> Self {
        Self {
            legal_actions: &decision.legal_actions,
            spell_costs: &decision.spell_costs,
            by_object: &decision.legal_actions_by_object,
        }
    }
}

/// Immutable inputs for one revision, with no invented decision for an
/// intermediate AI transition. Event filtering always uses this revision's
/// original state, not the final state of the surrounding AI batch.
pub(crate) struct StateUpdateSource<'a> {
    pub(crate) revision: u64,
    pub(crate) state: &'a GameState,
    pub(crate) events: &'a [GameEvent],
    pub(crate) log_entries: &'a [GameLogEntry],
    pub(crate) decision: Option<DecisionInputs<'a>>,
}

impl<'a> StateUpdateSource<'a> {
    pub(crate) fn from_result(result: &'a ActionResult, revision: u64) -> Self {
        Self {
            revision,
            state: &result.0,
            events: &result.1,
            log_entries: &result.3,
            decision: Some(DecisionInputs {
                legal_actions: &result.2,
                spell_costs: &result.5,
                by_object: &result.6,
            }),
        }
    }

    pub(crate) fn from_transition(transition: &'a AiTransition) -> Self {
        Self {
            revision: transition.state_revision,
            state: &transition.state,
            events: &transition.events,
            log_entries: &transition.log_entries,
            decision: transition.decision.as_ref().map(DecisionInputs::from),
        }
    }

    /// Checks every bound except events and log, which `guarded` trims.
    pub(crate) fn admissible(&self) -> Result<(), String> {
        Self {
            events: &[],
            log_entries: &[],
            ..*self
        }
        .check()
    }

    /// Trims events and log to their broadcast bounds, warning when it drops
    /// any, then checks the bounds it cannot trim.
    pub(crate) fn guarded(mut self, game_code: &str) -> Result<Self, String> {
        // Newest entries are kept: the state is authoritative, events and log are presentation.
        let events_dropped = self.events.len().saturating_sub(MAX_SNAPSHOT_EVENTS);
        let log_dropped = self
            .log_entries
            .len()
            .saturating_sub(MAX_SNAPSHOT_LOG_ENTRIES);
        self.events = &self.events[events_dropped..];
        self.log_entries = &self.log_entries[log_dropped..];
        if events_dropped > 0 || log_dropped > 0 {
            warn!(
                game = %game_code,
                revision = self.revision,
                events_dropped,
                log_dropped,
                "state update events/log trimmed to broadcast bounds"
            );
        }
        self.check()?;
        Ok(self)
    }

    fn check(&self) -> Result<(), String> {
        let empty_costs = HashMap::new();
        let empty_by_object = HashMap::new();
        guard_state_snapshot_broadcast(StateSnapshotParts {
            state: self.state,
            events: self.events,
            log_entries: self.log_entries,
            legal_actions: self
                .decision
                .as_ref()
                .map(|decision| decision.legal_actions)
                .unwrap_or(&[]),
            spell_costs: self
                .decision
                .as_ref()
                .map(|decision| decision.spell_costs)
                .unwrap_or(&empty_costs),
            legal_actions_by_object: self
                .decision
                .as_ref()
                .map(|decision| decision.by_object)
                .unwrap_or(&empty_by_object),
        })
    }

    pub(crate) fn without_decision(mut self) -> Self {
        self.decision = None;
        self
    }
}

/// Append owned frame copies only on a blocking CPU worker, without replacing
/// an earlier event batch. Registry locks only fence the final ownership transfer.
pub(crate) fn prepare_spectator_frames(
    frames: &mut Vec<(crate::socket_mailbox::Sender, ServerMessage)>,
    captured: &[crate::socket_mailbox::Sender],
    message: ServerMessage,
) {
    frames.reserve(captured.len());
    let Some((last, others)) = captured.split_last() else {
        return;
    };
    frames.extend(
        others
            .iter()
            .map(|sender| (sender.clone(), message.clone())),
    );
    frames.push((last.clone(), message));
}

/// A spectator may switch games while projection runs. Recheck captured
/// channels against the live registry under the same fence as that switch;
/// never enqueue an old-room frame after the destination bootstrap.
pub(crate) async fn publish_spectator_frames(
    executor: &GameExecutor,
    routes: &SharedGameSpectators,
    game_code: &str,
    frames: Vec<(crate::socket_mailbox::Sender, ServerMessage)>,
) -> Vec<crate::socket_mailbox::Sender> {
    let mut failed = Vec::new();
    let mut discarded = Vec::new();
    {
        let registry = routes.lock().await;
        for (sender, message) in frames {
            let current = registry.get(game_code).is_some_and(|recipients| {
                recipients.iter().any(|active| active.same_channel(&sender))
            });
            if !current {
                discarded.push(message);
            } else if sender
                .send_recovering(message, |message| discarded.push(message))
                .is_err()
            {
                failed.push(sender);
            }
        }
    }
    if !discarded.is_empty() {
        // Dropping large snapshots is CPU work too, and must not run under
        // the registry lock or on an I/O worker.
        executor.run(move || drop(discarded)).await;
    }
    failed
}

/// Captured sender identities and batch metadata, moved between CPU jobs.
/// The caller retains the session's publication lease throughout the batch.
pub(crate) struct StateUpdatePublisher {
    full_key: FullSessionKey,
    player_count: u8,
    players: HashMap<PlayerId, crate::socket_mailbox::Sender>,
    spectators: Vec<crate::socket_mailbox::Sender>,
    spectator_routes: Option<SharedGameSpectators>,
    spectator_frames: Vec<(crate::socket_mailbox::Sender, ServerMessage)>,
    failed_spectators: Vec<crate::socket_mailbox::Sender>,
    eliminated: Vec<PlayerId>,
    rewind_targets: Vec<RewindOption>,
    completion: Option<(PlayerId, server_core::protocol::CommandId)>,
}

impl StateUpdatePublisher {
    pub(crate) async fn capture(
        connections: &SharedConnections,
        game_spectators: Option<&SharedGameSpectators>,
        full_key: FullSessionKey,
        player_count: u8,
        eliminated: Vec<PlayerId>,
        rewind_targets: Vec<RewindOption>,
    ) -> Self {
        // No projection, serialization or worker admission under either lock.
        let players = connections
            .lock()
            .await
            .get(&full_key.game_code)
            .cloned()
            .unwrap_or_default();
        let spectators = match game_spectators {
            Some(routes) => routes
                .lock()
                .await
                .get(&full_key.game_code)
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Self {
            full_key,
            player_count,
            players,
            spectators,
            spectator_routes: game_spectators.cloned(),
            spectator_frames: Vec::new(),
            failed_spectators: Vec::new(),
            eliminated,
            rewind_targets,
            completion: None,
        }
    }

    pub(crate) fn with_completion(
        mut self,
        player: PlayerId,
        command_id: Option<server_core::protocol::CommandId>,
    ) -> Self {
        self.completion = command_id.map(|id| (player, id));
        self
    }

    pub(crate) fn set_batch_metadata(
        &mut self,
        eliminated: Vec<PlayerId>,
        rewind_targets: Vec<RewindOption>,
    ) {
        self.eliminated = eliminated;
        self.rewind_targets = rewind_targets;
    }

    pub(crate) async fn publish_ai(
        mut self,
        executor: &GameExecutor,
        transitions: Vec<AiTransition>,
    ) -> Self {
        self.flush_spectators(executor).await;
        let transition_count = transitions.len();
        for (index, transition) in transitions.into_iter().enumerate() {
            debug_assert!(
                transition.decision.is_none() || index + 1 == transition_count,
                "only the final AI transition may carry a decision",
            );
            self = executor
                .run(move || {
                    let source = StateUpdateSource::from_transition(&transition);
                    let revision = source.revision;
                    match source.guarded(&self.full_key.game_code) {
                        Ok(source) => self.publish(&source),
                        Err(reason) => warn!(
                            game = %self.full_key.game_code,
                            revision,
                            %reason,
                            "AI transition exceeds broadcast bounds; not published"
                        ),
                    }
                    self
                })
                .await;
            self.flush_spectators(executor).await;
        }
        self
    }

    pub(crate) fn notify_players(&self, message: ServerMessage) {
        for sender in self.players.values() {
            let _ = sender.send(message.clone());
        }
    }

    pub(crate) fn deliver_terminal(
        &self,
        deliveries: Vec<(PlayerId, server_core::CurrentTerminalDelivery)>,
    ) {
        for (player, delivery) in deliveries {
            if let Some(sender) = self.players.get(&player) {
                let _ = sender.send(ServerMessage::TerminalResult {
                    delivery: Some(delivery),
                });
            }
        }
    }

    /// Runs only inside a CPU job, after the caller's existing snapshot guard.
    pub(crate) fn publish(&mut self, source: &StateUpdateSource<'_>) {
        for (&player, sender) in &self.players {
            // Preserve the old seat-range boundary without projecting seats
            // which have no connection to receive their snapshot.
            if player.0 >= self.player_count || sender.is_closed() {
                continue;
            }
            let message = build_state_update_message(
                source,
                &self.full_key,
                player,
                self.rewind_targets.clone(),
                &self.eliminated,
            );
            let command_id = if self.completion.is_some_and(|(owner, _)| owner == player) {
                self.completion.take().map(|(_, id)| id)
            } else {
                None
            };
            let _ = sender.send_completion(message, command_id);
        }
        self.collect_closed_spectators();
        if self.spectators.is_empty() {
            return;
        }
        let Ok(message) = build_spectator_state_update_message(
            &self.full_key,
            source.state,
            source.events,
            source.log_entries,
            source.revision,
        ) else {
            return;
        };
        prepare_spectator_frames(&mut self.spectator_frames, &self.spectators, message);
    }

    async fn flush_spectators(&mut self, executor: &GameExecutor) {
        if self.spectator_frames.is_empty() {
            return;
        }
        let routes = self
            .spectator_routes
            .as_ref()
            .expect("prepared spectator frames have a captured registry");
        let failed = publish_spectator_frames(
            executor,
            routes,
            &self.full_key.game_code,
            std::mem::take(&mut self.spectator_frames),
        )
        .await;
        self.failed_spectators.extend(failed);
    }

    fn collect_closed_spectators(&mut self) {
        let failed = &mut self.failed_spectators;
        self.spectators.retain(|sender| {
            if sender.is_closed() {
                failed.push(sender.clone());
                false
            } else {
                true
            }
        });
    }

    pub(crate) async fn finish(mut self, executor: &GameExecutor) {
        self.flush_spectators(executor).await;
        for sender in &self.failed_spectators {
            let routes = self
                .spectator_routes
                .as_ref()
                .expect("failed spectator identities have a captured registry");
            // Remove only captured failed identities, never a replacement
            // spectator that has since attached under a reused game code.
            remove_game_spectator_sender(routes, &self.full_key.game_code, sender).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn cancelling_a_waiter_does_not_release_a_running_workers_cpu_permit() {
        let executor = GameExecutor::new(NonZeroUsize::MIN);
        let (release, wait) = std::sync::mpsc::channel();
        let (started, running) = oneshot::channel();
        let first = tokio::spawn({
            let executor = executor.clone();
            async move {
                executor
                    .run(move || {
                        let _ = started.send(());
                        let _ = wait.recv();
                    })
                    .await;
            }
        });
        running.await.unwrap();
        first.abort();
        let _ = first.await;

        let (started, mut running) = oneshot::channel();
        let second = tokio::spawn(async move {
            executor
                .run(move || {
                    let _ = started.send(());
                })
                .await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut running)
                .await
                .is_err(),
            "the cancelled waiter's blocking job still owns the sole CPU permit"
        );
        release.send(()).unwrap();
        running.await.unwrap();
        second.await.unwrap();
    }

    #[test]
    fn health_hello_and_pong_remain_responsive_with_all_cpu_permits_busy() {
        crate::socket_test_hooks::run_async_test(|| async {
            use crate::game_submission_tests::basic_land_test_database;
            use crate::issue_4548_full_create_tests::{
                recv_enveloped_server_message, recv_server_message, send_test_message,
                spawn_server, test_client_hello,
            };
            use crate::ServerContext;
            use engine::types::format::FormatConfig;
            use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
            use server_core::protocol::{ClientMessage, DeckChoice, DeckData, WireFormat};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::time::Instant;

            struct ReleaseOnDrop(Arc<AtomicBool>);
            impl Drop for ReleaseOnDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }

            let release = Arc::new(AtomicBool::new(false));
            let _release_on_drop = ReleaseOnDrop(release.clone());
            let busy = Arc::new(AtomicBool::new(false));
            let (started, running) = oneshot::channel();
            let started = parking_lot::Mutex::new(Some(started));
            let executor = GameExecutor::new(NonZeroUsize::MIN).with_ai_step_hook({
                let release = release.clone();
                let busy = busy.clone();
                move |_| {
                    let first = started.lock().take();
                    if let Some(started) = first {
                        busy.store(true, Ordering::Release);
                        let _ = started.send(());
                        let stop = Instant::now() + Duration::from_secs(5);
                        while !release.load(Ordering::Acquire) && Instant::now() < stop {
                            std::hint::spin_loop();
                        }
                        busy.store(false, Ordering::Release);
                    }
                }
            });
            let context = ServerContext {
                game_executor: executor.clone(),
                ..Default::default()
            };
            let (url, server, _temp_dir, app) = spawn_server(
                crate::ServerMode::Full,
                crate::draft_pools::DraftPools::default(),
                Arc::new(basic_land_test_database()),
                context,
            )
            .await;
            let code = {
                let names = DeckData {
                    main_deck: vec!["Network Test Land".to_string(); 60],
                    ..Default::default()
                };
                let deck = crate::resolve_deck(&app.db, &names).unwrap();
                let choice = Some(DeckChoice::DeckList(Box::new(names)));
                let mut manager = app.sessions.lock().await;
                let (code, _) = manager
                    .create_game_n_players(
                        deck.clone(),
                        choice.clone(),
                        "AI".to_string(),
                        None,
                        2,
                        Default::default(),
                        Some(FormatConfig::freeform()),
                    )
                    .unwrap();
                let session = manager.session_exclusive(&code).unwrap();
                session
                    .join_with_reservation(deck, choice, "human".to_string(), None)
                    .unwrap();
                session.start_game(&app.db).unwrap();
                session.ai_seats.insert(PlayerId(0));
                session.ai_configs.insert(
                    PlayerId(0),
                    create_config_for_players(AiDifficulty::Easy, Platform::Native, 2),
                );
                let key = crate::bind_full_session_key(&app.game_db, &code).unwrap();
                crate::initialize_full_runtime(&app.game_db, session, key).unwrap();
                code
            };
            let session = crate::lock_session(&app.sessions, &code).await.unwrap();
            let job = tokio::spawn(async move { executor.run_ai(session).await });

            let result = tokio::time::timeout(Duration::from_secs(2), async {
                running.await.unwrap();
                assert!(
                    busy.load(Ordering::Acquire),
                    "CPU work ran on the sole Tokio worker"
                );
                let authority = url
                    .strip_prefix("ws://")
                    .unwrap()
                    .strip_suffix("/ws")
                    .unwrap();
                let client = reqwest::Client::builder().no_proxy().build().unwrap();
                let health = client
                    .get(format!("http://{authority}/health"))
                    .send()
                    .await
                    .unwrap();
                assert!(health.status().is_success());
                assert_eq!(health.text().await.unwrap(), "ok");
                for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
                    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
                        .await
                        .unwrap();
                    let ServerMessage::ServerHello {
                        heartbeat: Some(contract),
                        ..
                    } = recv_server_message(&mut socket).await
                    else {
                        panic!("the native server did not advertise its heartbeat contract");
                    };
                    assert_eq!(contract.ping_interval_ms, 5000);
                    assert_eq!(contract.silence_budget_ms, 45000);
                    send_test_message(
                        &mut socket,
                        &test_client_hello(format.into_iter().collect()),
                        false,
                    )
                    .await;
                    send_test_message(
                        &mut socket,
                        &ClientMessage::Ping { timestamp: 73 },
                        format.is_some(),
                    )
                    .await;
                    let reply = if format.is_some() {
                        recv_enveloped_server_message(&mut socket).await
                    } else {
                        recv_server_message(&mut socket).await
                    };
                    assert!(matches!(reply, ServerMessage::Pong { timestamp: 73 }));
                    send_test_message(
                        &mut socket,
                        &ClientMessage::SubscribeLobby,
                        format.is_some(),
                    )
                    .await;
                    loop {
                        let message = if format.is_some() {
                            recv_enveloped_server_message(&mut socket).await
                        } else {
                            recv_server_message(&mut socket).await
                        };
                        if matches!(message, ServerMessage::LobbyUpdate { .. }) {
                            break;
                        }
                    }
                    assert!(
                        busy.load(Ordering::Acquire),
                        "probe ran after CPU admission became free"
                    );
                    socket.close(None).await.unwrap();
                }
            })
            .await;
            release.store(true, Ordering::Release);
            let (session, outcome) = job.await.unwrap();
            assert!(outcome.failure.is_none(), "the real AI driver failed");
            assert_eq!(
                server_core::acting_players(&session.state).as_slice(),
                &[PlayerId(1)],
                "the admitted AI decision must actually hand the game back to the human",
            );
            drop(session);
            server.abort();
            result.expect("HTTP or WebSocket liveness waited for game CPU admission");
        });
    }

    struct GamePair {
        _directory: tempfile::TempDir,
        db: crate::SharedDb,
        game_db: crate::SharedGameDb,
        state: SharedState,
        games: [(String, String, u64); 2],
    }

    fn game_pair(first_ai_players: &[PlayerId]) -> GamePair {
        use engine::types::format::FormatConfig;
        use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
        use server_core::protocol::{DeckChoice, DeckData};

        let db = Arc::new(crate::game_submission_tests::basic_land_test_database());
        let names = DeckData {
            main_deck: vec!["Network Test Land".to_string(); 60],
            ..Default::default()
        };
        let deck = crate::resolve_deck(&db, &names).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let game_db = Arc::new(
            crate::persistence::GameDb::open(
                &directory.path().join("games.db"),
                crate::persistence::SessionRetention::Multiplayer,
            )
            .unwrap(),
        );
        let mut manager = SessionManager::new();
        let games = [first_ai_players, &[]].map(|roster| {
            let (code, token) = manager
                .create_game_n_players_with_code(
                    deck.clone(),
                    Some(DeckChoice::DeckList(Box::new(names.clone()))),
                    "host".to_string(),
                    None,
                    2,
                    Default::default(),
                    Some(FormatConfig::freeform()),
                    None,
                )
                .unwrap();
            let session = manager.session_exclusive(&code).unwrap();
            session
                .join_with_reservation(
                    deck.clone(),
                    Some(DeckChoice::DeckList(Box::new(names.clone()))),
                    "guest".to_string(),
                    None,
                )
                .unwrap();
            session.start_game(&db).unwrap();
            for &player in roster {
                session.ai_seats.insert(player);
                session.ai_configs.insert(
                    player,
                    create_config_for_players(AiDifficulty::Easy, Platform::Native, 2),
                );
            }
            let key = crate::bind_full_session_key(&game_db, &code).unwrap();
            crate::initialize_full_runtime(&game_db, session, key).unwrap();
            (code, token, session.state_revision)
        });
        GamePair {
            _directory: directory,
            db,
            game_db,
            state: Arc::new(tokio::sync::Mutex::new(manager)),
            games,
        }
    }

    #[test]
    fn captured_spectator_projection_cannot_follow_a_destination_bootstrap() {
        crate::socket_test_hooks::run_async_test(|| async {
            let GamePair {
                _directory,
                state,
                games,
                ..
            } = game_pair(&[]);
            let [(prior, _, _), (next, _, _)] = games;
            let (key, revision, snapshot) = {
                let session = crate::lock_session(&state, &prior).await.unwrap();
                (
                    session.full_runtime.as_ref().unwrap().key.clone(),
                    session.state_revision,
                    session.current_broadcast_snapshot(),
                )
            };
            let bootstrap = {
                let session = crate::lock_session(&state, &next).await.unwrap();
                crate::build_spectator_game_started_message(&session).unwrap()
            };
            let (tx, mut messages) = crate::socket_mailbox::channel();
            let routes: SharedGameSpectators =
                Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                    prior.clone(),
                    vec![tx.clone()],
                )])));
            let executor = GameExecutor::new(NonZeroUsize::MIN);
            let hold = executor.admit().await;
            let (capturing, captured) = oneshot::channel();
            let projection = tokio::spawn({
                let executor = executor.clone();
                let routes = routes.clone();
                async move {
                    capturing.send(()).unwrap();
                    crate::broadcast_takeback_approved(
                        &executor,
                        &Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                        &routes,
                        key,
                        2,
                        revision,
                        snapshot,
                        None,
                        Vec::new(),
                    )
                    .await;
                }
            });
            // The child captures the old route, then parks on CPU admission,
            // before this current-thread runtime resumes the signalled parent.
            captured.await.unwrap();
            crate::switch_game_spectator_slot(&routes, Some(&prior), &next, &tx, Some(bootstrap))
                .await
                .unwrap();
            drop(hold);
            projection.await.unwrap();
            assert!(matches!(
                messages.recv().await.unwrap(),
                ServerMessage::GameStarted { full_key: Some(key), .. } if key.game_code == next
            ));
            assert!(
                matches!(messages.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "a captured old-room projection followed the new-room bootstrap"
            );
        });
    }

    #[test]
    fn persistence_serialization_is_admitted_but_sqlite_wait_does_not_block_other_games() {
        crate::socket_test_hooks::run_async_test(|| async {
            use crate::issue_4548_deadlock_tests::empty_identity;
            use engine::types::actions::MulliganChoice;
            use engine::types::game_state::WaitingFor;

            let GamePair {
                _directory: directory,
                db,
                game_db,
                state,
                games,
            } = game_pair(&[]);
            let executor = GameExecutor::new(NonZeroUsize::MIN);
            let blocker = rusqlite::Connection::open(directory.path().join("games.db")).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
            let (writing, mut writes) = mpsc::unbounded_channel();
            game_db.set_full_save_io_hook(Arc::new(move |key, revision| {
                writing.send((key.game_code.clone(), revision)).unwrap();
            }));
            let (entered, mut saves) = mpsc::unbounded_channel();
            let (release, wait) = std::sync::mpsc::channel();
            let wait = parking_lot::Mutex::new(Some(wait));
            game_db.set_full_save_hook(Arc::new(move |snapshot| {
                entered
                    .send((snapshot.key.game_code.clone(), snapshot.mutation_revision))
                    .unwrap();
                if let Some(wait) = wait.lock().take() {
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            }));
            let connections: SharedConnections = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let spectators: SharedGameSpectators =
                Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let drafts = Arc::new(tokio::sync::Mutex::new(crate::DraftSessionManager::new()));
            let submit = |code: String, token: String, started: oneshot::Sender<()>| {
                let executor = executor.clone();
                let state = state.clone();
                let db = db.clone();
                let game_db = game_db.clone();
                let connections = connections.clone();
                let spectators = spectators.clone();
                let drafts = drafts.clone();
                async move {
                    let (tx, mut messages) = crate::socket_mailbox::channel();
                    connections
                        .lock()
                        .await
                        .insert(code.clone(), HashMap::from([(PlayerId(0), tx.clone())]));
                    let mut identity = empty_identity();
                    identity.set_session(code, PlayerId(0), token);
                    started.send(()).unwrap();
                    crate::handle_full_game_submission(
                        None,
                        crate::GameSubmission::Action(GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        }),
                        &executor,
                        &state,
                        &db,
                        &drafts,
                        &connections,
                        &tx,
                        &game_db,
                        &spectators,
                        &identity,
                    )
                    .await;
                    messages.recv().await.unwrap()
                }
            };
            let [(code_a, token_a, revision_a), (code_b, token_b, revision_b)] = games;
            let (started, waiting) = oneshot::channel();
            let first = tokio::spawn(submit(code_a.clone(), token_a, started));
            waiting.await.unwrap();
            assert!(
                matches!(first.await.unwrap(), ServerMessage::StateUpdate { state_revision, .. }
                if state_revision == revision_a + 1)
            );
            let first_save = saves.recv().await.unwrap();
            assert_eq!(first_save.0, code_a);
            assert_eq!(first_save.1, revision_a + 1);

            let (started, waiting) = oneshot::channel();
            let mut second = tokio::spawn(submit(code_b.clone(), token_b, started));
            waiting.await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut second)
                    .await
                    .is_err(),
                "snapshot serialization escaped the shared CPU budget",
            );
            release.send(()).unwrap();
            let first_write = writes.recv().await.unwrap();
            assert_eq!(first_write.0, code_a);
            assert_eq!(first_write.1, revision_a + 1);
            assert!(
                matches!(tokio::time::timeout(Duration::from_secs(2), &mut second)
                    .await.expect("a SQLite wait retained the game CPU permit").unwrap(),
                    ServerMessage::StateUpdate { state_revision, .. }
                if state_revision == revision_b + 1)
            );
            assert!(
                matches!(saves.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "a second prepared JSON buffer escaped the separate I/O admission"
            );
            let durable_before: u64 = blocker
                .query_row(
                    "SELECT mutation_revision FROM game_sessions WHERE game_code = ?1",
                    [&code_a],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                durable_before, revision_a,
                "the first save must still be blocked in SQLite"
            );
            blocker.execute_batch("COMMIT;").unwrap();
            let second_save = saves.recv().await.unwrap();
            assert_eq!(second_save.0, code_b);
            assert_eq!(second_save.1, revision_b + 1);
            let second_write = writes.recv().await.unwrap();
            assert_eq!(second_write.0, code_b);
            assert_eq!(second_write.1, revision_b + 1);
            let _flushed = executor
                .persistence_io
                .clone()
                .acquire_owned()
                .await
                .unwrap();
            let mut snapshots = game_db.load_active_full_sessions().unwrap();
            for (code, revision) in [(code_a, revision_a + 1), (code_b, revision_b + 1)] {
                let index = snapshots
                    .iter()
                    .position(|snapshot| snapshot.key.game_code == code)
                    .unwrap();
                let snapshot = snapshots.swap_remove(index);
                assert_eq!(snapshot.mutation_revision, revision);
                let restored = GameSession::from_persisted(snapshot.persisted, &db).unwrap();
                let WaitingFor::MulliganDecision { pending, .. } = restored.state.waiting_for
                else {
                    panic!("the durable game did not retain the other human's pending decision");
                };
                assert!(pending.iter().all(|entry| entry.player != PlayerId(0)));
            }
        });
    }

    #[test]
    fn ai_batch_publishes_oversized_transitions_trimmed_and_skips_only_untrimmable_ones() {
        crate::socket_test_hooks::run_async_test(|| async {
            use engine::game::zones::create_object;
            use engine::types::game_state::WaitingFor;
            use engine::types::identifiers::CardId;
            use engine::types::log::{LogCategory, LogSegment};
            use engine::types::phase::Phase;
            use engine::types::zones::Zone;
            use server_core::game_state_snapshot_wire_guard::MAX_SNAPSHOT_OBJECTS;

            let events = |count: i32| -> Vec<GameEvent> {
                (1..=count)
                    .map(|amount| GameEvent::LifeChanged {
                        player_id: PlayerId(0),
                        amount,
                        new_total: Default::default(),
                    })
                    .collect()
            };
            let log = |count: u32| -> Vec<GameLogEntry> {
                (1..=count)
                    .map(|seq| GameLogEntry {
                        seq,
                        turn: 1,
                        phase: Phase::PreCombatMain,
                        category: LogCategory::Game,
                        segments: vec![LogSegment::Text("entry".to_string())],
                        presentation: Default::default(),
                    })
                    .collect()
            };
            let mut state = GameState::new_two_player(42);
            state.phase = Phase::PreCombatMain;
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            let mut crowded = state.clone();
            for index in 0..=MAX_SNAPSHOT_OBJECTS as u64 {
                create_object(
                    &mut crowded,
                    CardId(index),
                    PlayerId(0),
                    "Token".to_string(),
                    Zone::Battlefield,
                );
            }
            let small = AiTransition {
                state_revision: 1,
                state: state.clone(),
                events: events(3),
                log_entries: log(3),
                decision: None,
            };
            let untrimmable = AiTransition {
                state_revision: 2,
                state: crowded,
                events: Vec::new(),
                log_entries: Vec::new(),
                decision: None,
            };
            let oversized = AiTransition {
                state_revision: 3,
                decision: Some(DecisionSnapshot::from_state(&state)),
                state,
                events: events(MAX_SNAPSHOT_EVENTS as i32 + 500),
                log_entries: log(MAX_SNAPSHOT_LOG_ENTRIES as u32 + 500),
            };
            let full_key = FullSessionKey {
                game_code: "TRIM01".to_string(),
                generation: 1,
            };
            let expected_small = serde_json::to_value(build_state_update_message(
                &StateUpdateSource::from_transition(&small),
                &full_key,
                PlayerId(0),
                Vec::new(),
                &[],
            ))
            .unwrap();

            let (tx, mut messages) = crate::socket_mailbox::channel();
            let connections: SharedConnections =
                Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                    full_key.game_code.clone(),
                    HashMap::from([(PlayerId(0), tx)]),
                )])));
            let executor = GameExecutor::new(NonZeroUsize::MIN);
            let publisher = StateUpdatePublisher::capture(
                &connections,
                None,
                full_key,
                2,
                Vec::new(),
                Vec::new(),
            )
            .await;
            publisher
                .publish_ai(&executor, vec![small, untrimmable, oversized])
                .await
                .finish(&executor)
                .await;

            let mut received = Vec::new();
            while let Ok(message) = messages.try_recv() {
                received.push(message);
            }
            let revisions: Vec<_> = received
                .iter()
                .map(|message| match message {
                    ServerMessage::StateUpdate { state_revision, .. } => *state_revision,
                    _ => 0,
                })
                .collect();
            assert_eq!(revisions, [1, 3]);
            assert_eq!(serde_json::to_value(&received[0]).unwrap(), expected_small);
            let ServerMessage::StateUpdate {
                state_revision,
                events,
                log_entries,
                legal_actions,
                ..
            } = &received[1]
            else {
                panic!("expected the decision StateUpdate, got {:?}", received[1]);
            };
            assert_eq!(*state_revision, 3);
            assert!(!legal_actions.is_empty());
            assert_eq!(events.len(), MAX_SNAPSHOT_EVENTS);
            assert!(matches!(
                events.last(),
                Some(GameEvent::LifeChanged { amount, .. })
                    if *amount == MAX_SNAPSHOT_EVENTS as i32 + 500
            ));
            assert_eq!(log_entries.len(), MAX_SNAPSHOT_LOG_ENTRIES);
            assert_eq!(
                log_entries.last().map(|entry| entry.seq),
                Some(MAX_SNAPSHOT_LOG_ENTRIES as u32 + 500)
            );
        });
    }

    #[test]
    fn another_games_human_submission_finishes_between_ai_decisions() {
        assert_ai_batch_yields_to_another_game(AiBatchSource::Decisions);
    }

    #[test]
    fn a_contest_followup_yields_cpu_to_another_games_human_action() {
        assert_ai_batch_yields_to_another_game(AiBatchSource::ContestSettlement);
    }

    #[derive(Clone, Copy)]
    enum AiBatchSource {
        Decisions,
        ContestSettlement,
    }

    fn assert_ai_batch_yields_to_another_game(source: AiBatchSource) {
        crate::socket_test_hooks::run_async_test(move || async move {
            use crate::issue_4548_deadlock_tests::empty_identity;
            use engine::types::actions::MulliganChoice;
            use engine::types::game_state::WaitingFor;
            use std::sync::atomic::{AtomicUsize, Ordering};

            let GamePair {
                _directory,
                db,
                game_db,
                state,
                games,
            } = game_pair(&[PlayerId(0), PlayerId(1)]);
            let [(code_a, _token_a, revision_a), (code_b, token_b, revision_b)] = games;
            let (step_tx, mut steps) = mpsc::unbounded_channel();
            let (release, released) = std::sync::mpsc::channel();
            let released = parking_lot::Mutex::new(released);
            let count = AtomicUsize::new(0);
            let executor = GameExecutor::new(NonZeroUsize::MIN).with_ai_step_hook({
                let code_a = code_a.clone();
                move |session| {
                    if session.game_code == code_a {
                        let step = count.fetch_add(1, Ordering::SeqCst) + 1;
                        step_tx.send(step).unwrap();
                        released
                            .lock()
                            .recv_timeout(Duration::from_secs(5))
                            .expect("the test releases each AI decision");
                    }
                }
            });
            let mut a = crate::lock_session(&state, &code_a).await.unwrap();
            if matches!(source, AiBatchSource::ContestSettlement) {
                use engine::types::contest::{ContestConfig, ContestKind, ContestState};
                let observed = std::time::Instant::now();
                let config = ContestConfig {
                    countdown_ms: 1,
                    reconnect_grace_ms: 1,
                    ..Default::default()
                };
                a.contest.policy.config = config;
                a.state.phase = engine::types::phase::Phase::PreCombatMain;
                a.state.waiting_for = WaitingFor::PhysicalContest {
                    contest: Box::new(ContestState::new(
                        1,
                        ContestKind::Staring,
                        PlayerId(0),
                        PlayerId(1),
                        config,
                    )),
                    source_id: engine::types::identifiers::ObjectId(1),
                };
                a.tick_contest_at(observed);
                a.note_contest_connection(PlayerId(1), false);
                a.tick_contest_at(std::time::Instant::now() + Duration::from_millis(10));
                assert!(
                    !a.contest_active(),
                    "the contest must settle before its AI follow-up"
                );
            }
            let mut ai_job = tokio::spawn({
                let executor = executor.clone();
                async move {
                    match source {
                        AiBatchSource::Decisions => executor.run_ai(a).await,
                        AiBatchSource::ContestSettlement => {
                            let (session, drain) = executor.drain_contest(a).await;
                            let failure =
                                drain.ai_failure.as_ref().map(|fault| fault.cause.clone());
                            (
                                session,
                                AiRunOutcome {
                                    transitions: drain.transitions,
                                    failure,
                                    fault: drain.ai_failure,
                                },
                            )
                        }
                    }
                }
            });
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), steps.recv())
                    .await
                    .expect("the contest follow-up did not enter the per-decision executor")
                    .expect("the AI driver stopped before its first decision"),
                1,
            );

            let (tx, mut messages) = crate::socket_mailbox::channel();
            let connections: SharedConnections = Arc::new(tokio::sync::Mutex::new(HashMap::from(
                [(code_b.clone(), HashMap::from([(PlayerId(0), tx.clone())]))],
            )));
            let spectators: SharedGameSpectators =
                Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let draft_state = Arc::new(tokio::sync::Mutex::new(crate::DraftSessionManager::new()));
            let mut identity = empty_identity();
            identity.set_session(code_b, PlayerId(0), token_b);
            let (queued, waiting) = oneshot::channel();
            let human_job = tokio::spawn({
                let executor = executor.clone();
                async move {
                    let _ = queued.send(());
                    crate::handle_full_game_submission(
                        None,
                        crate::GameSubmission::Action(GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        }),
                        &executor,
                        &state,
                        &db,
                        &draft_state,
                        &connections,
                        &tx,
                        &game_db,
                        &spectators,
                        &identity,
                    )
                    .await;
                }
            });
            // Current-thread scheduling polls the submission through its pending
            // semaphore acquisition before this receiver can resume.
            waiting.await.unwrap();
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), human_job)
                .await
                .expect("another game waited for the whole AI batch")
                .unwrap();
            assert_eq!(steps.recv().await.unwrap(), 2);
            assert!(
                !ai_job.is_finished(),
                "the human response only arrived after AI completion"
            );
            let update = messages.recv().await.unwrap();
            let ServerMessage::StateUpdate {
                state,
                state_revision,
                ..
            } = update
            else {
                panic!("the real human action was not published: {update:?}");
            };
            assert_eq!(state_revision, revision_b + 1);
            let WaitingFor::MulliganDecision { pending, .. } = state.waiting_for else {
                panic!("the other human must still owe its opening decision");
            };
            assert!(pending.iter().all(|entry| entry.player != PlayerId(0)));
            release.send(()).unwrap();
            let (session, outcome) = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    tokio::select! {
                        result = &mut ai_job => break result.unwrap(),
                        step = steps.recv() => {
                            if step.is_some() { release.send(()).unwrap(); }
                        }
                    }
                }
            })
            .await
            .expect("the driven game did not finish its bounded sequence");
            for (index, transition) in outcome.transitions.iter().enumerate() {
                assert_eq!(transition.state_revision, revision_a + index as u64 + 1);
            }
            let rendered_revision = revision_a + outcome.transitions.len() as u64;
            if let Some(fault) = outcome.fault.as_ref() {
                assert_eq!(fault.after_state_revision, rendered_revision);
                assert!(
                    session.state_revision > rendered_revision,
                    "a durable driver fault needs its own persistence fence, not a fake transition",
                );
            } else {
                assert_eq!(session.state_revision, rendered_revision);
            }
            match outcome.failure {
                None => assert!(matches!(
                    session.state.waiting_for,
                    WaitingFor::GameOver { .. }
                )),
                Some(server_core::AiDriverFailure::ActionSafetyCapReached { limit }) => {
                    assert_eq!(limit, phase_ai::auto_play::MAX_AI_ACTIONS_PER_SEQUENCE)
                }
                failure => panic!("the driven fixture failed for a different reason: {failure:?}"),
            }
        });
    }
}

#[cfg(test)]
mod bandwidth_budget_tests;

#[cfg(test)]
mod network_tests;
