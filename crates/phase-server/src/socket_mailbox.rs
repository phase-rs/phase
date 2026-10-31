//! Application payloads and optional command ownership share one ordered FIFO.
//! Correlation is envelope metadata: legacy payloads, including unit replies,
//! retain their existing wire shape.
//! A single resync admission owner travels from decoder to dispatch to this
//! FIFO and the encoding worker. Only its response carries a forced-FULL hint.

use serde::{Deserialize, Serialize};
use server_core::protocol::{ClientMessage, CommandId, ServerMessage};
use std::sync::Arc;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

pub(crate) const MAX_PENDING_QUOTA_REFUSALS: usize = 8;

#[derive(Debug)]
pub(crate) enum RefusalError {
    Full,
    Closed,
}

#[derive(Debug)]
pub(crate) struct ResyncPermit {
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Default)]
pub(crate) enum StateEncoding {
    #[default]
    Automatic,
    Resync(ResyncPermit),
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClientEnvelope {
    #[serde(flatten)]
    pub(crate) message: ClientMessage,
    #[serde(default)]
    command_id: Option<CommandId>,
    #[serde(skip)]
    pub(crate) resync: Option<ResyncPermit>,
}

pub(crate) enum QuotaRecovery {
    Correlated(Option<CommandId>),
    ContestClick,
    Other,
}

impl ClientEnvelope {
    pub(crate) fn command_id(&self) -> Option<CommandId> {
        match self.message {
            ClientMessage::Action { .. } | ClientMessage::Interaction { .. } => self.command_id,
            _ => None,
        }
    }

    /// Recover valid or malformed command correlation without decoding its body.
    /// Serde skips all fields except this fixed-size header with `IgnoredAny`;
    /// no engine action/interaction or per-payload-field structure is allocated.
    pub(crate) fn malformed_command_id(text: &str) -> Option<CommandId> {
        match Self::quota_recovery(text)? {
            QuotaRecovery::Correlated(id) => id,
            QuotaRecovery::ContestClick | QuotaRecovery::Other => None,
        }
    }

    pub(crate) fn quota_recovery(text: &str) -> Option<QuotaRecovery> {
        #[derive(Deserialize)]
        enum Kind {
            Action,
            ContestClick,
            Interaction,
            #[serde(other)]
            Other,
        }
        #[derive(Deserialize)]
        struct Header {
            #[serde(rename = "type")]
            kind: Kind,
            command_id: Option<CommandId>,
        }
        let header: Header = serde_json::from_str(text).ok()?;
        Some(match header.kind {
            Kind::Action | Kind::Interaction => QuotaRecovery::Correlated(header.command_id),
            Kind::ContestClick => QuotaRecovery::ContestClick,
            Kind::Other => QuotaRecovery::Other,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Envelope {
    #[serde(flatten)]
    pub(crate) message: ServerMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) command_id: Option<CommandId>,
    #[serde(skip)]
    pub(crate) quota_refusal: Option<OwnedSemaphorePermit>,
    #[serde(skip)]
    pub(crate) state_encoding: StateEncoding,
}

#[derive(Clone, Debug)]
pub(crate) struct Sender(
    mpsc::UnboundedSender<Envelope>,
    Arc<Semaphore>,
    Arc<Semaphore>,
);

pub(crate) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Sender(
            tx,
            Arc::new(Semaphore::new(MAX_PENDING_QUOTA_REFUSALS)),
            Arc::new(Semaphore::new(1)),
        ),
        Receiver(rx),
    )
}

impl Sender {
    pub(crate) fn try_admit_resync(&self) -> Result<ResyncPermit, RefusalError> {
        if self.is_closed() {
            return Err(RefusalError::Closed);
        }
        self.2
            .clone()
            .try_acquire_owned()
            .map(|permit| ResyncPermit { _permit: permit })
            .map_err(|_| RefusalError::Full)
    }

    /// Recipient-local recovery only; failed large projections stay owned by
    /// the caller until it can drop them outside route locks on a CPU worker.
    pub(crate) fn send_resync(
        &self,
        message: ServerMessage,
        permit: ResyncPermit,
        on_closed: impl FnOnce(ServerMessage),
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.enqueue(
            message,
            None,
            None,
            StateEncoding::Resync(permit),
            on_closed,
        )
    }

    pub(crate) fn send(&self, message: ServerMessage) -> Result<(), mpsc::error::SendError<()>> {
        self.send_completion(message, None)
    }

    pub(crate) fn send_completion(
        &self,
        message: ServerMessage,
        command_id: Option<CommandId>,
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.enqueue(message, command_id, None, StateEncoding::Automatic, drop)
    }

    pub(crate) fn send_refusal(
        &self,
        message: ServerMessage,
        command_id: Option<CommandId>,
    ) -> Result<(), RefusalError> {
        let permit = self
            .1
            .clone()
            .try_acquire_owned()
            .map_err(|_| RefusalError::Full)?;
        self.enqueue(
            message,
            command_id,
            Some(permit),
            StateEncoding::Automatic,
            drop,
        )
        .map_err(|_| RefusalError::Closed)
    }

    /// Preserve failed payload ownership for callers that defer large drops
    /// to a CPU worker, without allocating or returning a message-sized error.
    pub(crate) fn send_recovering(
        &self,
        message: ServerMessage,
        on_closed: impl FnOnce(ServerMessage),
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.enqueue(message, None, None, StateEncoding::Automatic, on_closed)
    }

    fn enqueue(
        &self,
        message: ServerMessage,
        command_id: Option<CommandId>,
        quota_refusal: Option<OwnedSemaphorePermit>,
        state_encoding: StateEncoding,
        on_closed: impl FnOnce(ServerMessage),
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.0
            .send(Envelope {
                message,
                command_id,
                quota_refusal,
                state_encoding,
            })
            .map_err(|error| {
                on_closed(error.0.message);
                mpsc::error::SendError(())
            })
    }

    /// Own-command replies are deliberately not cloneable connection routes.
    pub(crate) fn for_completion(&self, command_id: Option<CommandId>) -> Completion<'_> {
        Completion {
            sender: self,
            command_id,
        }
    }

    pub(crate) fn same_channel(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

pub(crate) struct Completion<'a> {
    sender: &'a Sender,
    command_id: Option<CommandId>,
}

impl Completion<'_> {
    pub(crate) fn send(&self, message: ServerMessage) -> Result<(), mpsc::error::SendError<()>> {
        self.sender.send_completion(message, self.command_id)
    }
}

pub(crate) struct Receiver(mpsc::UnboundedReceiver<Envelope>);

impl Receiver {
    pub(crate) async fn recv_envelope(&mut self) -> Option<Envelope> {
        self.0.recv().await
    }

    pub(crate) fn queued_frames(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    pub(crate) async fn recv(&mut self) -> Option<ServerMessage> {
        self.recv_envelope().await.map(|envelope| envelope.message)
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Result<ServerMessage, mpsc::error::TryRecvError> {
        self.0.try_recv().map(|envelope| envelope.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unflushed_refusal_admission_survives_dequeue_and_is_reusable_after_flush() {
        let (sender, mut receiver) = channel();
        for id in 0..MAX_PENDING_QUOTA_REFUSALS {
            sender
                .send_refusal(ServerMessage::ActionNoOp, Some(CommandId(id as u64)))
                .unwrap();
        }
        let mut in_flight = receiver.recv_envelope().await.unwrap();
        let flushed = in_flight.quota_refusal.take();
        // Consuming the encoded envelope does not release the writer's
        // unflushed frame capacity.
        let _wire = serde_json::to_string(&in_flight).unwrap();
        drop(in_flight);
        assert!(matches!(
            sender.send_refusal(ServerMessage::ActionNoOp, Some(CommandId(99))),
            Err(RefusalError::Full),
        ));
        drop(flushed);
        sender
            .send_refusal(ServerMessage::ActionNoOp, Some(CommandId(100)))
            .unwrap();
        while let Ok(envelope) = receiver.0.try_recv() {
            drop(envelope);
        }
        // A healthy consumer has no lifetime strike count.
        for id in 0..MAX_PENDING_QUOTA_REFUSALS * 3 {
            sender
                .send_refusal(ServerMessage::ActionNoOp, Some(CommandId(id as u64)))
                .unwrap();
            drop(receiver.recv_envelope().await.unwrap());
        }
    }

    #[test]
    fn command_metadata_does_not_change_the_canonical_action_payload() {
        let command: ClientEnvelope = serde_json::from_value(serde_json::json!({
            "type": "Action",
            "command_id": 37,
            "data": { "action": { "type": "PassPriority" } },
        }))
        .unwrap();
        assert_eq!(command.command_id(), Some(CommandId(37)));
        assert!(matches!(
            command.message,
            ClientMessage::Action {
                action: engine::types::actions::GameAction::PassPriority
            }
        ));
        let ping: ClientEnvelope = serde_json::from_value(serde_json::json!({
            "type": "Ping", "data": { "timestamp": 1 }, "command_id": 37,
        }))
        .unwrap();
        assert_eq!(
            ping.command_id(),
            None,
            "control traffic never owns an application completion"
        );
    }

    #[test]
    fn correlated_unit_completion_still_decodes_with_the_legacy_protocol_enum() {
        let envelope = Envelope {
            message: ServerMessage::ActionNoOp,
            command_id: Some(CommandId(37)),
            quota_refusal: None,
            state_encoding: StateEncoding::Automatic,
        };
        let wire = serde_json::to_value(envelope).unwrap();
        assert_eq!(wire["command_id"], 37);
        assert!(
            wire.get("data").is_none(),
            "ActionNoOp must remain a unit wire variant"
        );
        assert!(matches!(
            serde_json::from_value::<ServerMessage>(wire).unwrap(),
            ServerMessage::ActionNoOp
        ));
        let plain = serde_json::to_value(Envelope {
            message: ServerMessage::ActionNoOp,
            command_id: None,
            quota_refusal: None,
            state_encoding: StateEncoding::Automatic,
        })
        .unwrap();
        assert!(
            plain.get("command_id").is_none(),
            "ambient replies must not invent completion ownership"
        );
    }

    #[tokio::test]
    async fn resync_ownership_releases_on_drop_and_closed_queue() {
        let (sender, mut receiver) = channel();
        let mut request: ClientEnvelope = serde_json::from_value(serde_json::json!({
            "type": "ResyncState", "command_id": 99, "resync": true,
        }))
        .unwrap();
        assert!(request.resync.is_none(), "clients cannot mint ownership");
        assert_eq!(request.command_id(), None);
        request.resync = Some(sender.try_admit_resync().unwrap());
        assert!(matches!(sender.try_admit_resync(), Err(RefusalError::Full)));
        drop(request);
        let permit = sender.try_admit_resync().unwrap();
        sender
            .send_resync(ServerMessage::ActionNoOp, permit, drop)
            .unwrap();
        let response = receiver.recv_envelope().await.unwrap();
        assert!(
            matches!(sender.try_admit_resync(), Err(RefusalError::Full)),
            "dequeue must not release ownership"
        );
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({"type": "ActionNoOp"})
        );
        drop(response);
        let permit = sender.try_admit_resync().unwrap();
        drop(receiver);
        let mut recovered = None;
        assert!(sender
            .send_resync(ServerMessage::Pong { timestamp: 17 }, permit, |message| {
                recovered = Some(message)
            })
            .is_err());
        assert!(matches!(
            recovered,
            Some(ServerMessage::Pong { timestamp: 17 })
        ));
        assert_eq!(sender.2.available_permits(), 1, "failed enqueue releases");
        assert!(matches!(
            sender.try_admit_resync(),
            Err(RefusalError::Closed)
        ));

        let (sender, receiver) = channel();
        sender
            .send_resync(
                ServerMessage::ActionNoOp,
                sender.try_admit_resync().unwrap(),
                drop,
            )
            .unwrap();
        drop(receiver);
        assert_eq!(
            sender.2.available_permits(),
            1,
            "teardown drops queued owners"
        );
        let (fresh, _receiver) = channel();
        assert!(fresh.try_admit_resync().is_ok());
    }
}
