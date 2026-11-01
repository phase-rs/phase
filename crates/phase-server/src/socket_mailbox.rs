//! Application payloads and optional command ownership share one ordered FIFO.
//! Correlation is envelope metadata: legacy payloads, including unit replies,
//! retain their existing wire shape.
//! A single resync admission owner travels from decoder to dispatch to this
//! FIFO and the encoding worker. Only its response carries a forced-FULL hint.

use serde::{Deserialize, Serialize};
use server_core::protocol::{ClientMessage, CommandId, ServerMessage, WithheldFrame};
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

impl StateEncoding {
    /// The admission owner of a mailbox-local resync response, which forces FULL.
    pub(crate) fn resync(&self) -> Option<&ResyncPermit> {
        match self {
            Self::Automatic => None,
            Self::Resync(permit) => Some(permit),
        }
    }
}

/// Why a correlated command was refused before reaching its handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalReason {
    RateQuota,
    GameBusy,
}

impl RefusalReason {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::RateQuota => "Command rate quota exceeded",
            Self::GameBusy => "Server is still processing an earlier command",
        }
    }
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

/// Shown in place of a `StateUpdate` over the frame bound.
pub(crate) const STATE_UPDATE_WITHHELD: &str = "The game state after this action is too large to send. A player can undo or request a takeback of the last action from the game menu to continue (for a loop, take it with a smaller count).";
/// Shown in place of a `GameStarted` over the frame bound.
pub(crate) const GAME_STARTED_WITHHELD: &str = "The game state is too large to send. A player whose window still shows the board can undo or request a takeback of the last action from the game menu to continue (for a loop, take it with a smaller count).";

/// What the encoder sends instead of a frame over the bound.
#[derive(Debug)]
pub(crate) enum OverBound {
    Retry(Box<Envelope>),
    Skip(String),
}

impl Envelope {
    /// Each state-bearing frame shrinks toward a notice; the rest cannot shrink.
    pub(crate) fn over_bound(self, reason: String) -> OverBound {
        let Self {
            mut message,
            command_id,
            quota_refusal: _,
            state_encoding,
        } = self;
        let withheld = |full_key: &mut Option<_>, state_revision: u64, frame| {
            Some(ServerMessage::StateWithheld {
                full_key: full_key.take(),
                state_revision,
                withheld: frame,
                reason: match frame {
                    WithheldFrame::GameStarted { .. } => GAME_STARTED_WITHHELD,
                    WithheldFrame::StateUpdate => STATE_UPDATE_WITHHELD,
                }
                .to_string(),
            })
        };
        let replacement = match &mut message {
            ServerMessage::StateUpdate {
                events,
                log_entries,
                ..
            } if !events.is_empty() || !log_entries.is_empty() => {
                events.clear();
                log_entries.clear();
                None
            }
            ServerMessage::StateUpdate {
                full_key,
                state_revision,
                ..
            } => withheld(full_key, *state_revision, WithheldFrame::StateUpdate),
            ServerMessage::GameStarted {
                full_key,
                state_revision,
                your_player,
                ..
            } => withheld(
                full_key,
                *state_revision,
                WithheldFrame::GameStarted {
                    your_player: *your_player,
                },
            ),
            ServerMessage::TerminalResult {
                delivery: Some(delivery),
            }
            | ServerMessage::TerminalBootstrapResult {
                delivery: Some(delivery),
            } if delivery.final_view.is_some() => {
                delivery.final_view = None;
                None
            }
            ServerMessage::AuthoritativeStateExport { .. } => {
                Some(ServerMessage::AuthoritativeStateExportFailed { message: reason })
            }
            ServerMessage::ManaPaymentPreview { request_id, .. } => {
                Some(ServerMessage::ManaPaymentPreviewFailed {
                    request_id: *request_id,
                    message: reason,
                })
            }
            ServerMessage::InteractionPreview { preview } => {
                Some(ServerMessage::InteractionPreviewFailed {
                    request_id: preview.request_id.clone(),
                    message: reason,
                })
            }
            ServerMessage::ServerHello { .. }
            | ServerMessage::GameCreated { .. }
            | ServerMessage::SessionAttached { .. }
            | ServerMessage::ActionRejected { .. }
            | ServerMessage::ActionFailed { .. }
            | ServerMessage::RequestRejected { .. }
            | ServerMessage::ActionNoOp
            | ServerMessage::StateWithheld { .. }
            | ServerMessage::GameAbandoned { .. }
            | ServerMessage::ManaPaymentPreviewRejected { .. }
            | ServerMessage::ManaPaymentPreviewFailed { .. }
            | ServerMessage::AuthoritativeStateExportFailed { .. }
            | ServerMessage::InteractionPreviewFailed { .. }
            | ServerMessage::OpponentDisconnected { .. }
            | ServerMessage::OpponentReconnected { .. }
            | ServerMessage::GameOver { .. }
            | ServerMessage::AiDriverFault { .. }
            | ServerMessage::TerminalBootstrapResult { .. }
            | ServerMessage::TerminalResult { .. }
            | ServerMessage::TerminalDeliveryAcknowledged { .. }
            | ServerMessage::Error { .. }
            | ServerMessage::LobbyUpdate { .. }
            | ServerMessage::LobbyGameAdded { .. }
            | ServerMessage::LobbyGameUpdated { .. }
            | ServerMessage::LobbyGameRemoved { .. }
            | ServerMessage::PlayerCount { .. }
            | ServerMessage::PasswordRequired { .. }
            | ServerMessage::JoinTargetInfo { .. }
            | ServerMessage::PlayerSlotsUpdate { .. }
            | ServerMessage::Conceded { .. }
            | ServerMessage::Emote { .. }
            | ServerMessage::ContestProgress { .. }
            | ServerMessage::TimerUpdate { .. }
            | ServerMessage::Pong { .. }
            | ServerMessage::PeerInfo { .. }
            | ServerMessage::DraftCreated { .. }
            | ServerMessage::DraftJoined { .. }
            | ServerMessage::DraftStateUpdate { .. }
            | ServerMessage::DraftMatchStart { .. }
            | ServerMessage::DraftTimerSync { .. }
            | ServerMessage::DraftActionRejected { .. }
            | ServerMessage::DraftOver { .. }
            | ServerMessage::DraftSpectatorView { .. }
            | ServerMessage::TakebackRequested { .. }
            | ServerMessage::TakebackResolved { .. }
            | ServerMessage::TournamentCreated { .. }
            | ServerMessage::TournamentJoined { .. }
            | ServerMessage::TournamentUpdate { .. }
            | ServerMessage::TournamentRemoved { .. }
            | ServerMessage::TournamentListUpdate { .. }
            | ServerMessage::TournamentActionAck { .. }
            | ServerMessage::TournamentActionRejected { .. }
            | ServerMessage::TournamentCredentialRenewed { .. } => return OverBound::Skip(reason),
        };
        OverBound::Retry(Box::new(Envelope {
            message: replacement.unwrap_or(message),
            command_id,
            quota_refusal: None,
            // A forced-FULL resync answered by a stripped update stays FULL,
            // and its permit stays held until the retry is prepared.
            state_encoding,
        }))
    }
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

    fn state_update(events: usize) -> ServerMessage {
        use engine::types::events::GameEvent;
        use engine::types::game_state::GameState;
        use engine::types::log::{GameLogEntry, LogCategory, LogSegment};
        use engine::types::phase::Phase;
        use engine::types::player::PlayerId;

        let state = GameState::new_two_player(42);
        let events: Vec<_> = (0..events)
            .map(|_| GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            })
            .collect();
        let log: Vec<_> = (0..events.len() as u32)
            .map(|seq| GameLogEntry {
                seq,
                turn: 1,
                phase: Phase::PreCombatMain,
                category: LogCategory::Game,
                segments: vec![LogSegment::Text("entry".to_string())],
                presentation: Default::default(),
            })
            .collect();
        crate::build_state_update_message(
            &crate::game_executor::StateUpdateSource {
                revision: 6,
                state: &state,
                events: &events,
                log_entries: &log,
                decision: None,
            },
            &server_core::FullSessionKey {
                game_code: "BOUND1".to_string(),
                generation: 1,
            },
            PlayerId(0),
            Vec::new(),
            &[],
        )
    }

    fn envelope(message: ServerMessage, command_id: Option<CommandId>) -> Envelope {
        Envelope {
            message,
            command_id,
            quota_refusal: None,
            state_encoding: StateEncoding::Automatic,
        }
    }

    fn retried(outcome: OverBound) -> Envelope {
        match outcome {
            OverBound::Retry(next) => *next,
            OverBound::Skip(reason) => panic!("skipped: {reason}"),
        }
    }

    #[test]
    fn codec_bound_over_bound_retry_retains_resync_permit() {
        let (sender, _mailbox) = channel();
        let mut next = envelope(state_update(3), Some(CommandId(9)));
        next.state_encoding = StateEncoding::Resync(sender.try_admit_resync().unwrap());
        let stripped = retried(next.over_bound("strip".into()));
        assert!(matches!(
            &stripped.message,
            ServerMessage::StateUpdate { events, log_entries, .. }
                if events.is_empty() && log_entries.is_empty()
        ));
        assert!(
            matches!(&stripped.state_encoding, StateEncoding::Resync(_)),
            "stripped retry must retain the resync owner"
        );
        assert!(matches!(sender.try_admit_resync(), Err(RefusalError::Full)));
        let withheld = retried(stripped.over_bound("withhold".into()));
        assert!(matches!(
            &withheld.message,
            ServerMessage::StateWithheld { .. }
        ));
        assert!(
            matches!(&withheld.state_encoding, StateEncoding::Resync(_)),
            "withheld retry must retain the resync owner"
        );
        assert!(matches!(sender.try_admit_resync(), Err(RefusalError::Full)));
        drop(withheld);
        drop(
            sender
                .try_admit_resync()
                .expect("dropping retry releases resync admission"),
        );
    }

    #[test]
    fn over_bound_state_update_strips_presentation_then_withholds_keeping_correlation() {
        let id = Some(CommandId(5));
        let stripped = retried(envelope(state_update(3), id).over_bound("r".to_string()));
        assert_eq!(stripped.command_id, id);
        let ServerMessage::StateUpdate {
            state_revision,
            events,
            log_entries,
            ..
        } = &stripped.message
        else {
            panic!("{:?}", stripped.message);
        };
        assert_eq!(*state_revision, 6);
        assert!(events.is_empty() && log_entries.is_empty());

        let notice = retried(stripped.over_bound("r".to_string()));
        assert_eq!(notice.command_id, id);
        let ServerMessage::StateWithheld {
            full_key,
            state_revision,
            withheld,
            reason,
        } = notice.message
        else {
            panic!("{:?}", notice.message);
        };
        assert_eq!(full_key.unwrap().game_code, "BOUND1");
        assert_eq!(state_revision, 6);
        assert_eq!(withheld, WithheldFrame::StateUpdate);
        assert_eq!(reason, STATE_UPDATE_WITHHELD);

        let again = envelope(state_update(0), None).over_bound("r".to_string());
        assert!(matches!(
            retried(again).message,
            ServerMessage::StateWithheld {
                withheld: WithheldFrame::StateUpdate,
                ..
            }
        ));
    }

    #[test]
    fn over_bound_game_started_is_withheld() {
        let ServerMessage::StateUpdate {
            state,
            viewer_interaction,
            derived,
            ..
        } = state_update(0)
        else {
            unreachable!()
        };
        let started = ServerMessage::GameStarted {
            state_revision: 4,
            state,
            your_player: engine::types::player::PlayerId(1),
            opponent_name: None,
            player_names: Vec::new(),
            legal_actions: Vec::new(),
            auto_pass_recommended: false,
            end_continuous_effect_offers: Vec::new(),
            mana_payment_shortcut_actions: Vec::new(),
            spell_costs: Default::default(),
            legal_actions_by_object: Default::default(),
            activation_block_reasons: Default::default(),
            derived,
            viewer_interaction,
            player_token: None,
            full_key: None,
            events: Vec::new(),
            rewind_targets: Vec::new(),
        };
        let notice = retried(envelope(started, Some(CommandId(2))).over_bound("r".to_string()));
        assert_eq!(notice.command_id, Some(CommandId(2)));
        assert!(matches!(
            notice.message,
            ServerMessage::StateWithheld {
                full_key: None,
                state_revision: 4,
                withheld: WithheldFrame::GameStarted {
                    your_player: engine::types::player::PlayerId(1)
                },
                ref reason,
            } if reason == GAME_STARTED_WITHHELD
        ));
    }

    #[test]
    fn over_bound_replies_become_their_failure_siblings() {
        let reason = "too large to send: 9 bytes; at most 8 allowed".to_string();
        let export = ServerMessage::AuthoritativeStateExport {
            state: "{}".to_string(),
        };
        let failed =
            retried(envelope(export.clone(), Some(CommandId(3))).over_bound(reason.clone()));
        assert_eq!(failed.command_id, Some(CommandId(3)));
        assert!(matches!(
            failed.message,
            ServerMessage::AuthoritativeStateExportFailed { ref message } if *message == reason
        ));
        let uncorrelated = retried(envelope(export, None).over_bound(reason.clone()));
        assert_eq!(uncorrelated.command_id, None);
        assert!(matches!(
            uncorrelated.message,
            ServerMessage::AuthoritativeStateExportFailed { .. }
        ));
        let preview = ServerMessage::ManaPaymentPreview {
            request_id: 7,
            source_ids: Vec::new(),
        };
        assert!(matches!(
            retried(envelope(preview, None).over_bound(reason.clone())).message,
            ServerMessage::ManaPaymentPreviewFailed { request_id: 7, ref message } if *message == reason
        ));
    }

    #[test]
    fn over_bound_terminal_delivery_drops_its_final_view_once() {
        let delivery = server_core::CurrentTerminalDelivery {
            key: server_core::FullSessionKey {
                game_code: "TERM01".to_string(),
                generation: 1,
            },
            terminal_revision: 9,
            delivery_id: server_core::TerminalDeliveryId("delivery".to_string()),
            credential: server_core::TerminalCredential("credential".to_string()),
            display: server_core::TerminalMatchDisplay {
                winner: None,
                reason: "Match conceded".to_string(),
                ranked_result: None,
            },
            final_view: Some(serde_json::value::RawValue::from_string("{}".to_string()).unwrap()),
        };
        let result = ServerMessage::TerminalResult {
            delivery: Some(delivery),
        };
        let viewless = retried(envelope(result, None).over_bound("r".to_string()));
        assert!(matches!(
            &viewless.message,
            ServerMessage::TerminalResult { delivery: Some(delivery) } if delivery.final_view.is_none()
        ));
        assert!(matches!(
            viewless.over_bound("r".to_string()),
            OverBound::Skip(_)
        ));
    }

    #[test]
    fn over_bound_frames_without_state_are_skipped() {
        for message in [
            ServerMessage::ActionNoOp,
            ServerMessage::StateWithheld {
                full_key: None,
                state_revision: 1,
                withheld: WithheldFrame::StateUpdate,
                reason: STATE_UPDATE_WITHHELD.to_string(),
            },
        ] {
            assert!(matches!(
                envelope(message, Some(CommandId(1))).over_bound("r".to_string()),
                OverBound::Skip(reason) if reason == "r"
            ));
        }
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
