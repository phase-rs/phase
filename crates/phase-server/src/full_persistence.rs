//! One latest-wins metadata slot per Full lifetime; no queued GameState copies.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use engine::types::game_state::WaitingFor;
use parking_lot::Mutex;
use server_core::session::GameSession;
use server_core::{FullPersistDisposition, FullPersistSnapshot, FullSessionKey};
use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::game_executor::{GameExecutor, SessionJob};
use crate::persistence::GameDb;
use crate::SharedGameDb;

#[derive(Clone, Copy)]
struct Request {
    revision: u64,
    activation_epoch: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Scheduler {
    pending: Mutex<HashMap<FullSessionKey, watch::Sender<Request>>>,
}

impl Scheduler {
    pub(crate) fn request(
        &self,
        executor: &GameExecutor,
        db: &SharedGameDb,
        session: &SessionJob<'_>,
    ) {
        let Some(runtime) = session.full_runtime.as_ref() else {
            tracing::warn!(game = %session.game_code, "skipping persistence for unbound Full session");
            return;
        };
        if matches!(session.state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }
        let request = Request {
            revision: session.state_revision,
            activation_epoch: runtime.activation_epoch,
        };
        let mut pending = self.pending.lock();
        if let Some(sender) = pending
            .get(&runtime.key)
            .filter(|sender| !sender.is_closed())
        {
            // Replacing a weak handle never clones or retains the game payload.
            sender.send_replace(request);
            return;
        }
        // Keep the weak handle in the worker, not watch's shared RwLock value.
        // GameDb then shares only scalar request metadata instead of adding a
        // Sync proof through GameSession's recursively carried engine traces.
        let (sender, receiver) = watch::channel(request);
        pending.insert(runtime.key.clone(), sender);
        tokio::spawn(run(
            executor.clone(),
            Arc::downgrade(db),
            Arc::new(runtime.key.clone()),
            session.weak_session(),
            receiver,
        ));
    }

    pub(crate) fn cancel(&self, key: &FullSessionKey) {
        self.pending.lock().remove(key);
    }

    pub(crate) fn cancel_all(&self) {
        self.pending.lock().clear();
    }
}

#[derive(Clone, Copy)]
enum Next {
    Wait,
    Refresh,
    Closed,
}

fn newer(receiver: &watch::Receiver<Request>) -> Option<Next> {
    match receiver.has_changed() {
        Ok(false) => None,
        Ok(true) => Some(Next::Refresh),
        Err(_) => Some(Next::Closed),
    }
}

async fn run(
    executor: GameExecutor,
    db: Weak<GameDb>,
    key: Arc<FullSessionKey>,
    session: Weak<AsyncMutex<GameSession>>,
    mut receiver: watch::Receiver<Request>,
) {
    loop {
        let request = *receiver.borrow_and_update();
        let Some(db) = db.upgrade() else { return };
        // Do not retain the database while idle: its sender registry must be
        // able to disappear and close this worker during shutdown.
        match save_latest(
            &executor,
            db,
            key.clone(),
            &mut receiver,
            request,
            session.clone(),
        )
        .await
        {
            Next::Closed => return,
            Next::Refresh => continue,
            Next::Wait => {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn current(
    db: SharedGameDb,
    key: Arc<FullSessionKey>,
    revision: u64,
    activation_epoch: Option<u64>,
) -> bool {
    match tokio::task::spawn_blocking(move || {
        db.full_save_currency(&key, revision, activation_epoch)
    })
    .await
    {
        Ok(Ok(FullPersistDisposition::Applied)) => true,
        Ok(Ok(_)) => false,
        Ok(Err(error)) => {
            tracing::error!(%error, "failed to check Full snapshot currency");
            false
        }
        Err(error) => {
            tracing::error!(%error, "Full snapshot currency worker failed");
            false
        }
    }
}

async fn discard(executor: &GameExecutor, snapshot: FullPersistSnapshot) {
    if let Err(error) = executor.try_run(move || drop(snapshot)).await {
        tracing::error!(%error, "Full snapshot disposal worker failed");
    }
}

async fn save_latest(
    executor: &GameExecutor,
    db: SharedGameDb,
    key: Arc<FullSessionKey>,
    receiver: &mut watch::Receiver<Request>,
    request: Request,
    session: Weak<AsyncMutex<GameSession>>,
) -> Next {
    if let Some(next) = newer(receiver) {
        return next;
    }
    // This check precedes both the game-state clone and JSON serialization.
    // It is an optimization, not a database lease: the final SQL fence remains.
    if !current(
        db.clone(),
        key.clone(),
        request.revision,
        request.activation_epoch,
    )
    .await
    {
        return Next::Wait;
    }
    if let Some(next) = newer(receiver) {
        return next;
    }
    let Some(session) = session.upgrade() else {
        return Next::Wait;
    };
    let session = tokio::select! {
        biased;
        changed = receiver.changed() => return if changed.is_ok() { Next::Refresh } else { Next::Closed },
        session = session.lock_owned() => session,
    };
    let capture_receiver = receiver.clone();
    let capture_key = key.clone();
    let (session, snapshot) = executor
        .with_session(session, move |session| {
            if newer(&capture_receiver).is_some()
                || session.state_revision != request.revision
                || session.full_runtime.as_ref().is_none_or(|runtime| {
                    runtime.key != *capture_key
                        || runtime.activation_epoch != request.activation_epoch
                })
                || matches!(session.state.waiting_for, WaitingFor::GameOver { .. })
            {
                return None;
            }
            session.full_persist_snapshot()
        })
        .await;
    drop(session);
    let Some(snapshot) = snapshot else {
        return newer(receiver).unwrap_or(Next::Wait);
    };
    // At most one captured snapshot per game can reach this point. Newer work
    // cancels this wait and retains only its metadata, not another snapshot.
    let io = tokio::select! {
        biased;
        changed = receiver.changed() => {
            discard(executor, snapshot).await;
            return if changed.is_ok() { Next::Refresh } else { Next::Closed };
        }
        io = executor.admit_persistence_io() => io,
    };
    if !current(
        db.clone(),
        key.clone(),
        snapshot.mutation_revision,
        snapshot.activation_epoch,
    )
    .await
    {
        discard(executor, snapshot).await;
        return Next::Wait;
    }
    let serialization_receiver = receiver.clone();
    let preparation_db = db.clone();
    let prepared = executor
        .try_run(move || {
            // The I/O lease travels into the worker, including cancellation of its
            // async waiter. No second serializer can create a prepared SQL buffer.
            if newer(&serialization_receiver).is_some() {
                return (io, None);
            }
            (io, Some(preparation_db.prepare_full_session_save(snapshot)))
        })
        .await;
    let (io, prepared) = match prepared {
        Ok((io, Some(Ok(prepared)))) => (io, prepared),
        Ok((_, Some(Err((key, error))))) => {
            tracing::error!(game = %key.game_code, %error, "failed to serialize Full session");
            return Next::Wait;
        }
        Ok((_, None)) => return newer(receiver).unwrap_or(Next::Wait),
        Err(error) => {
            tracing::error!(%error, "Full serialization worker failed");
            return Next::Wait;
        }
    };
    if let Some(next) = newer(receiver) {
        // Prepared strings are released off the I/O runtime as well.
        if let Err(error) = executor.try_run(move || drop((io, prepared))).await {
            tracing::error!(%error, "Full prepared-save disposal worker failed");
        }
        return next;
    }
    if let Err(error) = tokio::task::spawn_blocking(move || {
        let _io = io;
        match db.save_prepared_full_session(&prepared) {
            Ok(FullPersistDisposition::Applied) => {}
            Ok(disposition) => tracing::warn!(game = %prepared.key.game_code, ?disposition, "Full snapshot was no longer current"),
            Err(error) => tracing::error!(game = %prepared.key.game_code, %error, "failed to persist Full session"),
        }
    }).await {
        tracing::error!(%error, "Full persistence I/O worker failed");
    }
    Next::Wait
}
