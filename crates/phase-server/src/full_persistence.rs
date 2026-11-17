//! One coalesced request and at most one captured checkpoint per Full lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use engine::types::game_state::WaitingFor;
use parking_lot::Mutex;
use server_core::session::GameSession;
use server_core::{FullPersistDisposition, FullPersistSnapshot, FullSessionKey};
use tokio::sync::{watch, Mutex as AsyncMutex, OwnedSemaphorePermit};

use crate::game_executor::{GameExecutor, SessionJob};
use crate::persistence::GameDb;
use crate::SharedGameDb;

#[derive(Clone, Copy)]
struct Request {
    revision: u64,
    activation_epoch: Option<u64>,
}

type PendingMap = Mutex<HashMap<FullSessionKey, PendingWorker>>;

struct PendingWorker {
    requests: watch::Sender<Request>,
    identity: Arc<()>,
}

struct Registration {
    pending: Weak<PendingMap>,
    key: Arc<FullSessionKey>,
    identity: Arc<()>,
}

impl Registration {
    fn remove_current(&self, pending: &mut HashMap<FullSessionKey, PendingWorker>) {
        if pending
            .get(self.key.as_ref())
            .is_some_and(|worker| Arc::ptr_eq(&worker.identity, &self.identity))
        {
            pending.remove(self.key.as_ref());
        }
    }

    fn finish_if_idle(&self, receiver: &watch::Receiver<Request>) -> bool {
        let Some(pending) = self.pending.upgrade() else {
            return true;
        };
        let mut pending = pending.lock();
        // Request publication uses this same lock. Never retire between a new
        // request's send_replace and observing its watch version.
        if matches!(newer(receiver), Some(Next::Refresh)) {
            return false;
        }
        self.remove_current(&mut pending);
        true
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.upgrade() {
            self.remove_current(&mut pending.lock());
        }
    }
}

#[derive(Default)]
pub(crate) struct Scheduler {
    pending: Arc<PendingMap>,
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
            .filter(|worker| !worker.requests.is_closed())
        {
            // New requests replace scalar metadata, not the worker's checkpoint.
            sender.requests.send_replace(request);
            return;
        }
        // Keep the weak handle in the worker, not watch's shared RwLock value.
        // GameDb then shares only scalar request metadata instead of adding a
        // Sync proof through GameSession's recursively carried engine traces.
        let (sender, receiver) = watch::channel(request);
        let key = Arc::new(runtime.key.clone());
        let identity = Arc::new(());
        pending.insert(
            runtime.key.clone(),
            PendingWorker {
                requests: sender,
                identity: identity.clone(),
            },
        );
        let registration = Registration {
            pending: Arc::downgrade(&self.pending),
            key: key.clone(),
            identity,
        };
        tokio::spawn(run(
            executor.clone(),
            Arc::downgrade(db),
            key,
            session.weak_session(),
            receiver,
            registration,
        ));
    }

    pub(crate) fn cancel(&self, key: &FullSessionKey) {
        self.pending.lock().remove(key);
    }

    pub(crate) fn cancel_all(&self) {
        self.pending.lock().clear();
    }

    #[cfg(test)]
    pub(crate) fn pending_games(&self) -> usize {
        self.pending.lock().len()
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
    registration: Registration,
) {
    loop {
        let Some(db) = db.upgrade() else { return };
        // Do not retain the database while idle: its sender registry must be
        // able to disappear and close this worker during shutdown.
        match save_latest(&executor, db, key.clone(), &mut receiver, session.clone()).await {
            Next::Closed => return,
            Next::Refresh => continue,
            Next::Wait => {
                if registration.finish_if_idle(&receiver) {
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
    io: OwnedSemaphorePermit,
) -> Option<OwnedSemaphorePermit> {
    match tokio::task::spawn_blocking(move || {
        // Cancellation of the async waiter must not admit another DB-mutex
        // waiter while this blocking query is still running.
        let currency = db.full_save_currency(&key, revision, activation_epoch);
        (io, currency)
    })
    .await
    {
        Ok((io, Ok(FullPersistDisposition::Applied))) => Some(io),
        Ok((_, Ok(_))) => None,
        Ok((_, Err(error))) => {
            tracing::error!(%error, "failed to check Full snapshot currency");
            None
        }
        Err(error) => {
            tracing::error!(%error, "Full snapshot currency worker failed");
            None
        }
    }
}

/// New requests coalesce without cancelling an in-flight admission or lease wait.
/// Only removal of this lifetime's sender cancels pending work.
async fn cancelled(mut receiver: watch::Receiver<Request>) {
    while receiver.changed().await.is_ok() {}
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
    session: Weak<AsyncMutex<GameSession>>,
) -> Next {
    if session.strong_count() == 0 {
        return Next::Closed;
    }
    let Some(session) = session.upgrade() else {
        return Next::Closed;
    };
    #[cfg(test)]
    executor.notify_persistence_lease_wait();
    let session = tokio::select! {
        biased;
        _ = cancelled(receiver.clone()) => return Next::Closed,
        session = session.lock_owned() => session,
    };
    if matches!(newer(receiver), Some(Next::Closed)) {
        return Next::Closed;
    }
    let request = *receiver.borrow_and_update();
    // The lease pins the current capture revision, including a newer revision
    // that did not schedule another save. Never hold global I/O admission while
    // waiting for a game's potentially long AI batch to release this lease.
    let capture_key = key.clone();
    let (session, snapshot) = executor
        .with_session(session, move |session| {
            if session.state_revision < request.revision
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
        return Next::Wait;
    };
    // Retain one captured snapshot and this FIFO queue position per game.
    // New mutations mark the worker dirty; they never discard its checkpoint.
    let io = tokio::select! {
        biased;
        _ = cancelled(receiver.clone()) => {
            discard(executor, snapshot).await;
            return Next::Closed;
        }
        io = executor.admit_persistence_io() => io,
    };
    let Some(io) = current(
        db.clone(),
        key.clone(),
        snapshot.mutation_revision,
        snapshot.activation_epoch,
        io,
    )
    .await
    else {
        discard(executor, snapshot).await;
        return Next::Wait;
    };
    let serialization_receiver = receiver.clone();
    let preparation_db = db.clone();
    let prepared = executor
        .try_run(move || {
            // The I/O lease travels into the worker, including cancellation of
            // its async waiter. Newer requests never discard this checkpoint;
            // a retired lifetime still avoids unnecessary serialization.
            if serialization_receiver.has_changed().is_err() {
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
        Ok((_, None)) => return Next::Closed,
        Err(error) => {
            tracing::error!(%error, "Full serialization worker failed");
            return Next::Wait;
        }
    };
    // Once admitted, commit this checkpoint even if mutations keep arriving.
    // The SQL lifetime/activation/revision fences remain authoritative; later
    // requests coalesce for the next checkpoint instead of starving durability.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SessionRetention;
    use std::time::Duration;

    #[test]
    fn cancelled_currency_waiter_keeps_io_admission_until_db_query_exits() {
        crate::socket_test_hooks::run_async_test(|| async {
            let directory = tempfile::tempdir().unwrap();
            let db = Arc::new(
                GameDb::open(
                    &directory.path().join("query-cancel.db"),
                    SessionRetention::Multiplayer,
                )
                .unwrap(),
            );
            let key = Arc::new(crate::bind_full_session_key(&db, "CANCEL").unwrap());
            let executor = GameExecutor::new(std::num::NonZeroUsize::MIN);
            let query_entered = Arc::new(tokio::sync::Notify::new());
            db.set_full_currency_entry(query_entered.clone());
            let db_held = Arc::new(tokio::sync::Notify::new());
            let (release, blocked) = std::sync::mpsc::channel();
            let holder = tokio::task::spawn_blocking({
                let db = db.clone();
                let db_held = db_held.clone();
                move || db.hold_connection_for_test(db_held, blocked)
            });
            db_held.notified().await;
            let io = executor.admit_persistence_io().await;
            let query = tokio::spawn(current(db, key, 1, None, io));
            query_entered.notified().await;
            query.abort();
            assert!(query.await.unwrap_err().is_cancelled());
            let escaped =
                tokio::time::timeout(Duration::from_millis(100), executor.admit_persistence_io())
                    .await;
            let admitted_while_query_blocked = escaped.is_ok();
            drop(escaped);
            drop(release);
            holder.await.unwrap();
            let reusable =
                tokio::time::timeout(Duration::from_secs(2), executor.admit_persistence_io()).await;
            assert!(
                !admitted_while_query_blocked,
                "aborting the async query waiter released a still-running DB query's permit",
            );
            assert!(
                reusable.is_ok(),
                "completed query must release I/O admission"
            );
        });
    }
}
