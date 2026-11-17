//! One Anthropic message thread per LLM seat, so a seat keeps its reasoning
//! from one decision to the next.
//!
//! # Why
//!
//! A self-contained decision request rebuilds the seat's whole view and asks
//! the model to plan from scratch, and the reply's reasoning is never fed back.
//! A plan worked out at one priority window ("cast this, the trigger draws two,
//! then block with that") is therefore re-derived at the next. With a message
//! thread the API keeps the conversation, the model's earlier turns and
//! thinking included, and each later decision sends only what changed.
//!
//! # How a seat's thread moves
//!
//! - **Create.** The seat's first decision is today's full request plus
//!   `thread: create` ([`crate::wire::build_thread_request`]).
//! - **Continue.** Each later decision sends one user message —
//!   [`GameDecisionRequest::thread_update`]: what was logged since the seat's
//!   last answered decision, the live position, and the options — and
//!   continues from the id of the seat's last accepted reply. When more was
//!   logged than the seat's history window shows, the newest entries go entry
//!   by entry and the older ones as one line of key events per turn, so a
//!   busy round at a multiplayer table does not start the thread over.
//! - **Commit on accept only.** A request is remembered as pending under its
//!   own turn id ([`ThreadTurnId`]), and becomes the thread's new turn only
//!   when its own reply is accepted ([`LlmThreadMemory::record_reply`]), so a
//!   cancelled attempt's late reply never touches the request that replaced
//!   it. A failed or
//!   cancelled decision leaves the thread at its last answered turn, with one
//!   exception: a 2xx reply the engine refuses was still stored by the API,
//!   and a reply id can be continued only once, so the thread cannot go on
//!   from its last answered turn and the seat's next decision starts a new one.
//! - **Start over.** A new thread is created, with the full request, when the
//!   match moves on to its next game, when the endpoint, API key or model
//!   changes, when the caller reports any change to the seat's provider
//!   settings, even one undone before a decision saw it, such as message
//!   threads turned off and on again
//!   ([`LlmThreadMemory::observe_settings_revision`]),
//!   when the system prompt changes (difficulty, format, or the table itself),
//!   when the last reply reported more context than
//!   [`THREAD_CONTEXT_BUDGET_TOKENS`] or no usage at all, when the log was
//!   rewound under the thread, and when the log handed over no longer reaches
//!   back to the seat's last answer, so a continue cannot carry everything
//!   since it.
//! - **No thread at all.** The two lowest difficulties never use one: they
//!   deliberately reason from a near-term window, which a thread would undo.
//!   Neither does a decision between games (the play/draw choice), which
//!   belongs to no game, nor one another player makes for the seat while they
//!   control its turn, which must neither read nor add to the seat's own
//!   reasoning ([`GameDecisionRequest::thread_game`]).
//! - **Thread errors.** A thread is best effort: the API drops one after five
//!   idle minutes, among other reasons. On a thread error the seat's thread is
//!   dropped ([`LlmThreadMemory::record_reply`]) and the caller asks the same
//!   decision once more, which then goes out as a fresh create. An API that
//!   refuses threads outright is answered with self-contained requests for that
//!   endpoint and account. A conversation the API lost again at the first
//!   continue of its replacement, while the caller's clocks agree that the
//!   replacement cannot have expired yet ([`ClockReading`]), is one the API
//!   cannot store: its decisions are self-contained from then on, while any
//!   other conversation still uses threads. When the clocks cannot rule the
//!   expiry out, the conversation is recovered again instead. The seat
//!   remembers every conversation it lost or could not store, so switching to
//!   another one and back changes neither.
//!
//! A thread belongs to one seat of one game. Nothing in it reaches another seat
//! or the shared game log. The owner of this memory clears it when a new game
//! is installed or the game is restored; the next game of a match replaces the
//! game in place without a clear, and the game number keeps its threads apart.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use engine::types::log::GameLogEntry;
use engine::types::player::PlayerId;

use crate::error::{LlmError, LlmResult, ThreadErrorCode};
use crate::fingerprint::fingerprint_of;
use crate::game_decision::GameDecisionRequest;
use crate::provider::{HttpRequestSpec, LlmEndpointConfig};
use crate::wire::{
    build_chat_request, build_thread_request, continuable_message_id, token_usage, LlmReply,
    ThreadTurn, TokenUsage,
};

/// The context a seat's thread may reach before its next decision starts a
/// new thread with a full request.
///
/// A thread is append-only, so its context only grows: every turn adds a
/// position, the options and a reply. The budget keeps a long game bounded. It
/// sits well inside the smallest context window a current Claude model has
/// (200k tokens), with room for the next turn and its thinking, and it bounds
/// the cache reads each turn pays for. Starting over costs one full request,
/// exactly what every decision costs without a thread, so a lower budget only
/// trades continuity for smaller turns.
pub const THREAD_CONTEXT_BUDGET_TOKENS: u64 = 150_000;

/// One game decision's HTTP request, with the size of the prompt it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionHttpRequest {
    pub request: HttpRequestSpec,
    /// Characters of prompt the request carries: system and user for a full
    /// request, the one update message for a continue. What prompt-size work
    /// is measured by, for every provider, including those that report no
    /// token counts.
    pub prompt_chars: usize,
    /// The thread turn this request is, for a request sent through a thread.
    /// The transport hands it back with the reply ([`LlmThreadMemory::record_reply`]),
    /// so a reply is only ever recorded against the request that produced it.
    pub thread_turn: Option<ThreadTurnId>,
}

/// Identifies one threaded request among a seat's, so a reply can be bound to
/// the request that produced it rather than to whichever request was built
/// last for the same decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadTurnId(pub u32);

/// How the thread a request creates or continues came to be, which decides
/// what losing it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadOrigin {
    /// Created by an ordinary decision.
    Fresh,
    /// Created to replace, in the same conversation, a thread the API had just
    /// lost (`thread_not_found`), and not continued since. Anthropic: "If a
    /// continue request still returns a 404 error right after you replay, the
    /// API might be unable to store that conversation … Send it without
    /// `thread`." Losing a recovery thread at its first continue is that case
    /// only when the thread cannot have expired first ([`Idle`]), judged when
    /// the loss is reported ([`LlmThreadMemory::record_reply`]).
    Recovery,
    /// Continued at least once.
    Continued,
}

/// How long a thread the API holds lives without a request. Anthropic: "A
/// thread is best-effort. It expires 5 minutes after its last request".
const THREAD_IDLE_EXPIRY_MS: u64 = 5 * 60 * 1000;

/// One reading of the caller's two clocks, taken together, in milliseconds.
///
/// The API's thread expiry runs in real time, through a sleeping machine too,
/// and neither platform clock measures that alone. The wall clock
/// (`Date.now()`) counts the time a machine sleeps, but a clock correction can
/// step it either way. The monotonic clock (`performance.now()`) never steps,
/// but can stop while the machine sleeps. Two readings measure an idle time
/// only where both clocks agree on it ([`Idle::between`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// The wall clock (`Date.now()`).
    pub wall_ms: u64,
    /// The monotonic clock (`performance.now()`).
    pub monotonic_ms: u64,
}

/// How far the two clocks may disagree over an interval that still measures
/// an idle time.
///
/// While the machine is awake both clocks advance together, apart from the
/// rounding a browser applies to each reading and a slow drift, both far below
/// a second over a thread's five-minute lifetime. A sleep that stops the
/// monotonic clock, or a correction that steps the wall clock, moves them
/// apart by its whole length. A smaller tolerance reads more intervals as
/// unknown, at one more create each; a larger one lets a wall-clock step back
/// that nearly cancels a sleep pass for agreement.
const CLOCK_AGREEMENT_MS: u64 = 1_000;

/// How long a thread had been idle by the time the API reported it lost,
/// which tells an ordinary expiry apart from a conversation the API cannot
/// keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Idle {
    /// Both clocks agree that less than [`THREAD_IDLE_EXPIRY_MS`] passed since
    /// the request whose reply the continue builds on: the thread cannot have
    /// expired, so a loss is the API's doing.
    WithinExpiry,
    /// Long enough for the thread to have expired, or not known to be shorter:
    /// a clock went back, or the two disagree (the machine slept, or the wall
    /// clock was corrected). A loss is read as an ordinary expiry, and the
    /// conversation is recovered again, which costs one more create.
    MayHaveExpired,
}

impl Idle {
    /// How long a thread whose last request was built at `requested_at` had
    /// idled at `now`.
    ///
    /// Only positive evidence reads as [`Idle::WithinExpiry`]: both clocks
    /// moved forward, by amounts within [`CLOCK_AGREEMENT_MS`] of each other,
    /// and the larger, with that tolerance added, is still short of the
    /// expiry. A sleep anywhere between the two readings, in any of the awaits
    /// between them, is counted by both clocks, or, where it stops the
    /// monotonic clock, leaves them apart; a correction leaves them apart. The
    /// request went out no earlier than it was built, and a loss is reported
    /// no earlier than the API read the continue, so an interval the clocks
    /// agree on errs long. Erring long costs one more create; erring short would end
    /// threading for a conversation the API can keep.
    ///
    /// Two clocks cannot see a discontinuity that moves both alike: a wall
    /// clock that stops with the monotonic clock while the machine sleeps (as
    /// a paused virtual machine's can, until it is synchronized), or a sleep
    /// that a later wall-clock step back cancels to within the tolerance.
    /// Either leaves the clocks agreeing on less time than passed.
    fn between(requested_at: ClockReading, now: ClockReading) -> Idle {
        let wall = now.wall_ms.checked_sub(requested_at.wall_ms);
        let monotonic = now.monotonic_ms.checked_sub(requested_at.monotonic_ms);
        match (wall, monotonic) {
            (Some(wall), Some(monotonic))
                if wall.abs_diff(monotonic) <= CLOCK_AGREEMENT_MS
                    && wall.max(monotonic).saturating_add(CLOCK_AGREEMENT_MS)
                        < THREAD_IDLE_EXPIRY_MS =>
            {
                Idle::WithinExpiry
            }
            _ => Idle::MayHaveExpired,
        }
    }
}

/// Every LLM seat's message thread for the game in progress.
///
/// Session state held by the transport beside
/// [`LlmDecisionMemory`](crate::game_decision::LlmDecisionMemory), and cleared
/// with it when a new game is installed or the game is restored. Within a
/// match, each thread is bound to its game number.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LlmThreadMemory {
    seats: HashMap<PlayerId, SeatThread>,
    /// The id the next threaded request gets.
    next_turn: u32,
}

/// One seat's thread state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SeatThread {
    /// The thread the seat's next decision may continue.
    open: Option<OpenThread>,
    /// The threaded request in flight, committed only when its own reply is
    /// accepted.
    pending: Option<PendingTurn>,
    /// Every conversation whose thread the API lost (`thread_not_found`) and
    /// that no accepted reply in that conversation has replaced yet. The
    /// seat's next create for one of them is a recovery
    /// ([`ThreadOrigin::Recovery`]); a create for any other conversation
    /// (another account, model, game or system prompt) is an ordinary one,
    /// with a first recovery of its own to come. A seat that switches to
    /// another conversation and back still owes the first one its recovery.
    lost: HashSet<Conversation>,
    /// Every conversation the API could not store: it lost the thread
    /// replacing a lost one at its first continue, within the thread's
    /// lifetime ([`Idle::WithinExpiry`]). The seat's decisions in each of them
    /// are self-contained from then on, as Anthropic advises ("Send it without
    /// `thread`"), however often the seat switches between conversations;
    /// every other conversation still uses threads.
    unstored: HashSet<Conversation>,
    /// The endpoint and account whose API refused threads
    /// (`thread_unsupported_request`), which the seat sends self-contained
    /// requests to, since a fresh create would only be refused again. The seat
    /// keeps the most recent refusal until another replaces it or a restore
    /// (an undo or a resume) clears this memory.
    refused: Option<ThreadEndpoint>,
    /// The revision of the seat's own provider settings that its thread state
    /// belongs to, as the caller numbers them
    /// ([`LlmThreadMemory::observe_settings_revision`]).
    settings_revision: u32,
}

/// The endpoint and account a thread lives on. A thread is stored by the
/// account that created it and keeps one model, so a different endpoint, key
/// or model is a different thread, and an account's refusal of threads says
/// nothing about another account's.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThreadEndpoint {
    base_url: String,
    model: String,
    /// A fingerprint of the API key, never the key itself. It tells two
    /// credentials apart and is never serialized, logged or sent anywhere.
    credential: String,
}

impl ThreadEndpoint {
    fn of(config: &LlmEndpointConfig) -> LlmResult<ThreadEndpoint> {
        Ok(ThreadEndpoint {
            base_url: config.resolved_base_url()?,
            model: config.model.trim().to_string(),
            credential: fingerprint_of([config.api_key.trim()]),
        })
    }
}

/// The conversation a thread carries: one game of the match, on one endpoint
/// and account, under one system prompt. A thread is continued only within
/// its conversation, and a recovery belongs to the conversation it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Conversation {
    /// The game of the match ([`GameDecisionRequest::thread_game`]). A thread
    /// never continues into the next game.
    game_number: u8,
    endpoint: ThreadEndpoint,
    /// Fingerprint of the system prompt the thread was created with. A
    /// continue cannot send a new one, so a change starts a new thread.
    system: String,
}

/// A thread the API holds for a seat, as of the seat's last accepted reply.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenThread {
    conversation: Conversation,
    /// The id of the seat's last accepted reply, which the next decision
    /// continues from.
    message_id: String,
    /// The newest log entry the thread has been shown, or `None` when the log
    /// was empty.
    seen_through: Option<u32>,
    /// How much context the last reply reported ([`context_tokens`]), or
    /// `None` when it reported no usable usage.
    context_tokens: Option<u64>,
    origin: ThreadOrigin,
    /// When the request whose reply this is was built, on the caller's
    /// clocks. It went out no earlier, so the thread expires
    /// [`THREAD_IDLE_EXPIRY_MS`] after this at the earliest.
    requested_at: ClockReading,
}

/// A threaded request awaiting its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingTurn {
    /// The request this is. A reply is committed only against its own request.
    turn: ThreadTurnId,
    conversation: Conversation,
    /// The newest log entry the request showed the seat.
    seen_through: Option<u32>,
    shape: PendingShape,
    /// When the request was built, on the caller's clocks.
    requested_at: ClockReading,
}

/// Whether a pending request creates a thread or continues one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingShape {
    /// A create, and the origin the thread it creates will have.
    Create(ThreadOrigin),
    /// A continue of a thread of this origin.
    Continue {
        origin: ThreadOrigin,
        /// When the thread's last request was built
        /// ([`OpenThread::requested_at`]), which its idle time runs from.
        thread_requested_at: ClockReading,
    },
}

impl PendingTurn {
    /// Whether losing this request's thread at `now` shows the API cannot
    /// store its conversation: the request is the first continue of a
    /// recovery thread, and the loss is reported while that thread cannot
    /// have expired yet ([`Idle::WithinExpiry`]).
    ///
    /// The idle time is read as the loss is reported, not when the continue
    /// was built. A machine can sleep across the expiry while a continue is
    /// being built or is in flight, and only the later readings count that
    /// time.
    fn lost_an_unstorable_conversation(&self, now: ClockReading) -> bool {
        match self.shape {
            PendingShape::Continue {
                origin: ThreadOrigin::Recovery,
                thread_requested_at,
            } => Idle::between(thread_requested_at, now) == Idle::WithinExpiry,
            PendingShape::Continue {
                origin: ThreadOrigin::Fresh | ThreadOrigin::Continued,
                ..
            }
            | PendingShape::Create(_) => false,
        }
    }
}

impl OpenThread {
    /// Whether `next` may continue this thread rather than start a new one.
    fn continues(&self, next: &PendingTurn) -> bool {
        let log_kept_its_place = match (self.seen_through, next.seen_through) {
            (Some(seen), Some(newest)) => newest >= seen,
            // A log emptied or renumbered under the thread: what it has seen
            // can no longer be told apart from what it has not.
            (Some(_), None) => false,
            (None, _) => true,
        };
        self.conversation == next.conversation
            && self
                .context_tokens
                .is_some_and(|tokens| tokens <= THREAD_CONTEXT_BUDGET_TOKENS)
            && log_kept_its_place
    }
}

impl LlmThreadMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the HTTP request for one game decision by `seat`.
    ///
    /// An endpoint that does not use message threads — every profile that did
    /// not opt in, and every protocol but Anthropic Messages — and a seat whose
    /// decision may not use one ([`GameDecisionRequest::thread_game`]) get
    /// exactly [`build_chat_request`]'s request, and any thread the seat held
    /// is forgotten (what the seat knows of each conversation, and an
    /// endpoint's refusal of threads, are kept). So do the
    /// decisions of a refused endpoint and of a conversation the API could not
    /// store. Otherwise the request continues the seat's thread when it can
    /// and creates a new one when it cannot, and is remembered as pending until
    /// [`Self::record_reply`].
    ///
    /// `history` is the log handed to the prompt builder; its newest entry is
    /// what the seat will have seen once this decision is answered. `now` is
    /// the caller's two clocks as the request is built: the request goes out
    /// no earlier, and the idle time of the thread its reply opens runs from
    /// it ([`Idle`]).
    ///
    /// # Errors
    ///
    /// Whatever the request builders refuse, as [`LlmError`](crate::LlmError).
    pub fn decision_request(
        &mut self,
        seat: PlayerId,
        endpoint: &LlmEndpointConfig,
        decision: &GameDecisionRequest,
        history: &[GameLogEntry],
        now: ClockReading,
    ) -> LlmResult<DecisionHttpRequest> {
        let full = |request| DecisionHttpRequest {
            request,
            prompt_chars: decision.prompt.char_count(),
            thread_turn: None,
        };
        let game_number = match decision.thread_game() {
            Some(game_number) if endpoint.uses_message_threads() => game_number,
            _ => {
                if let Some(thread) = self.seats.get_mut(&seat) {
                    thread.forget_thread();
                }
                return build_chat_request(endpoint, &decision.prompt).map(full);
            }
        };
        let target = ThreadEndpoint::of(endpoint)?;
        let thread = self.seats.entry(seat).or_default();
        if thread.refused.as_ref() == Some(&target) {
            thread.pending = None;
            return build_chat_request(endpoint, &decision.prompt).map(full);
        }

        let conversation = Conversation {
            game_number,
            endpoint: target,
            system: fingerprint_of([decision.prompt.system.as_str()]),
        };
        if thread.unstored.contains(&conversation) {
            thread.pending = None;
            return build_chat_request(endpoint, &decision.prompt).map(full);
        }
        let create_origin = if thread.lost.contains(&conversation) {
            ThreadOrigin::Recovery
        } else {
            ThreadOrigin::Fresh
        };
        let turn = ThreadTurnId(self.next_turn);
        self.next_turn = self.next_turn.wrapping_add(1);
        let mut next = PendingTurn {
            turn,
            conversation,
            seen_through: history.iter().map(|entry| entry.seq).max(),
            shape: PendingShape::Create(create_origin),
            requested_at: now,
        };
        let continuation = thread
            .open
            .as_ref()
            .filter(|open| open.continues(&next))
            .and_then(|open| {
                decision
                    .thread_update(history, open.seen_through)
                    .map(|update| (open, update))
            });
        let request = match continuation {
            Some((open, update)) => {
                next.shape = PendingShape::Continue {
                    origin: open.origin,
                    thread_requested_at: open.requested_at,
                };
                DecisionHttpRequest {
                    request: build_thread_request(
                        endpoint,
                        ThreadTurn::Continue {
                            previous_message_id: &open.message_id,
                            user: &update,
                        },
                    )?,
                    prompt_chars: update.chars().count(),
                    thread_turn: Some(turn),
                }
            }
            None => DecisionHttpRequest {
                thread_turn: Some(turn),
                ..full(build_thread_request(
                    endpoint,
                    ThreadTurn::Create {
                        prompt: &decision.prompt,
                    },
                )?)
            },
        };
        thread.pending = Some(next);
        Ok(request)
    }

    /// Record what became of the reply to `seat`'s threaded request `turn`
    /// (the [`DecisionHttpRequest::thread_turn`] it was built with).
    ///
    /// `outcome` is the engine's verdict on the reply:
    ///
    /// - **Accepted.** The reply becomes the turn the seat's next decision
    ///   continues from. One that cannot be continued (a refusal, or no `id`)
    ///   leaves the seat with no thread, so its next decision starts a new one.
    /// - **A thread error.** The seat's thread is dropped, so the same decision
    ///   asked again goes out as a fresh create. After `thread_not_found` that
    ///   create is a recovery of the lost conversation; a create for another
    ///   one (a new account, game or system prompt) is not. When the API
    ///   refuses threads outright (`thread_unsupported_request`), the endpoint
    ///   and account are remembered and the seat's decisions to them are
    ///   self-contained, because a fresh create would only be refused again.
    ///   When it loses a recovery thread at its first continue while both of
    ///   the caller's clocks agree the thread cannot have expired yet
    ///   ([`Idle::WithinExpiry`]), the API cannot store that conversation, and
    ///   its decisions are self-contained from then on. Lost when it may have
    ///   expired, or when the clocks cannot tell, the conversation is
    ///   recovered again like any other. `now` is the caller's two clocks as
    ///   the reply arrived, which the thread's idle time is measured to: the
    ///   API read the continue no later, so a machine that slept while the
    ///   continue was built or in flight reads as idle that long, or leaves
    ///   the clocks apart.
    /// - **Any other refusal of a 2xx reply.** The API stored that turn after
    ///   the seat's last answered one, and an id can be continued only once, so
    ///   the seat's next decision starts a new thread instead of sending a
    ///   continue the API would refuse.
    /// - **Any other failure.** A non-2xx reply stored nothing, so the thread
    ///   stays at the seat's last answered turn.
    ///
    /// Nothing happens for a self-contained request (`turn` is `None`), or for
    /// a reply to any request but the seat's pending one: a cancelled
    /// attempt's late reply never touches the request that replaced it.
    pub fn record_reply(
        &mut self,
        seat: PlayerId,
        turn: Option<ThreadTurnId>,
        reply: LlmReply<'_>,
        outcome: Result<(), &LlmError>,
        now: ClockReading,
    ) {
        let Some(turn) = turn else {
            return;
        };
        let Some(thread) = self.seats.get_mut(&seat) else {
            return;
        };
        let Some(answered) = thread.pending.take_if(|pending| pending.turn == turn) else {
            return;
        };
        match outcome {
            Ok(()) => {
                let origin = match answered.shape {
                    PendingShape::Create(origin) => origin,
                    PendingShape::Continue { .. } => ThreadOrigin::Continued,
                };
                // The conversation has an answered turn again, so it no longer
                // waits on a recovery. Any other conversation the seat lost
                // still does.
                thread.lost.remove(&answered.conversation);
                thread.open =
                    continuable_message_id(reply.provider, reply.body).map(|message_id| {
                        OpenThread {
                            conversation: answered.conversation,
                            message_id,
                            seen_through: answered.seen_through,
                            context_tokens: token_usage(reply.provider, reply.body)
                                .as_ref()
                                .and_then(context_tokens),
                            origin,
                            requested_at: answered.requested_at,
                        }
                    });
            }
            Err(LlmError::ThreadUnavailable { code }) => {
                thread.open = None;
                match code {
                    ThreadErrorCode::UnsupportedRequest => {
                        thread.refused = Some(answered.conversation.endpoint);
                    }
                    // The replacement for a lost thread was lost at its first
                    // continue, too soon to have expired: Anthropic's "right
                    // after you replay" case.
                    ThreadErrorCode::NotFound if answered.lost_an_unstorable_conversation(now) => {
                        thread.unstored.insert(answered.conversation);
                    }
                    // Any other lost thread, an expired replacement included:
                    // the create that replaces it, in the same conversation,
                    // is a recovery.
                    ThreadErrorCode::NotFound => {
                        thread.lost.insert(answered.conversation);
                    }
                    // The API still holds this thread, or holds it with other
                    // fixed fields. Nothing was lost, so the create that
                    // replaces it is an ordinary one.
                    ThreadErrorCode::AlreadyContinued
                    | ThreadErrorCode::ContinueInFlight
                    | ThreadErrorCode::FingerprintMismatch => {}
                }
            }
            Err(
                LlmError::Configuration { .. }
                | LlmError::MalformedResponse { .. }
                | LlmError::Provider { .. }
                | LlmError::EmptyCompletion
                | LlmError::UndecodableChoice { .. }
                | LlmError::ChoiceOutOfRange { .. }
                | LlmError::UnsupportedDecision { .. }
                | LlmError::StaleDecision,
            ) => {
                // A 2xx reply was stored by the API whatever the engine made
                // of it; a non-2xx one stored nothing.
                if (200..300).contains(&reply.status) {
                    thread.open = None;
                }
            }
        }
    }

    /// Note the revision of `seat`'s own provider settings, as the caller
    /// numbers them, before the seat's next decision is built.
    ///
    /// The player can change a seat's settings, the message-thread toggle
    /// included, without any decision being built under the change: threads
    /// turned off and on again between two decisions, or during one the
    /// caller then drops unsent. A seat's thread is therefore kept only under
    /// the settings revision it was opened under. A new revision forgets the
    /// seat's thread and any request pending on it, as a decision sent without
    /// a thread does, so the seat's next threaded decision is a create. What
    /// the seat knows of each conversation, and an endpoint's refusal of
    /// threads, are kept.
    pub fn observe_settings_revision(&mut self, seat: PlayerId, revision: u32) {
        let thread = self.seats.entry(seat).or_default();
        if thread.settings_revision != revision {
            thread.settings_revision = revision;
            thread.forget_thread();
        }
    }

    /// Whether the API holds a thread for `seat` that its next decision could
    /// continue.
    pub fn has_open_thread(&self, seat: PlayerId) -> bool {
        self.seats
            .get(&seat)
            .is_some_and(|thread| thread.open.is_some())
    }

    /// Forget every seat's thread (a new game, or a restore over this one).
    pub fn clear(&mut self) {
        self.seats.clear();
    }
}

impl SeatThread {
    /// Forget the seat's thread and any request pending on it. What the seat
    /// knows of each conversation (a recovery it is owed, or that the API
    /// cannot store it) and an endpoint's refusal of threads are kept, because
    /// they outlast any one thread.
    fn forget_thread(&mut self) {
        self.open = None;
        self.pending = None;
    }
}

/// The size of the conversation a reply closes, in Anthropic's accounting.
///
/// Anthropic's `input_tokens` is only the uncached remainder of the prompt,
/// so the cache reads and cache writes are added back, and the reply itself is
/// part of what the next turn carries. A reply that reports no input or output
/// count cannot be measured, and neither can one whose counts overflow (they
/// come from across the network): its thread is not continued.
fn context_tokens(usage: &TokenUsage) -> Option<u64> {
    usage
        .input_tokens?
        .checked_add(usage.cached_input_tokens.unwrap_or(0))?
        .checked_add(usage.cache_creation_input_tokens.unwrap_or(0))?
        .checked_add(usage.output_tokens?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_decision::build_game_decision_prompt;
    use crate::provider::{DecisionContext, LlmProvider};
    use engine::ai_support::{ActionMetadata, AiDecisionContract, CandidateAction, TacticalClass};
    use engine::types::actions::GameAction;
    use engine::types::format::FormatConfig;
    use engine::types::game_state::GameState;
    use engine::types::log::{LogCategory, LogImportance, LogPresentation, LogSegment};
    use engine::types::phase::Phase;
    use phase_ai::config::AiDifficulty;
    use serde_json::{json, Value};

    const SEAT: PlayerId = PlayerId(1);
    /// The caller's clocks for a request whose timing does not matter.
    const CLOCKS: ClockReading = ClockReading {
        wall_ms: 0,
        monotonic_ms: 0,
    };

    /// A reading on which both clocks show `ms`, as they do on a machine that
    /// stays awake through an interval with no clock correction.
    fn in_step(ms: u64) -> ClockReading {
        ClockReading {
            wall_ms: ms,
            monotonic_ms: ms,
        }
    }

    fn endpoint(provider: LlmProvider, context: DecisionContext) -> LlmEndpointConfig {
        LlmEndpointConfig {
            provider,
            base_url: (provider == LlmProvider::Jev).then(|| "https://phase.example".to_string()),
            api_key: "secret".to_string(),
            model: "claude-test".to_string(),
            max_output_tokens: None,
            temperature: None,
            decision_context: context,
        }
    }

    fn threaded() -> LlmEndpointConfig {
        endpoint(LlmProvider::Anthropic, DecisionContext::MessageThread)
    }

    /// One decision. A different `revision` is a different decision, with its
    /// own fingerprint, over the same two options.
    fn decision(revision: u64, difficulty: AiDifficulty) -> GameDecisionRequest {
        decision_in_game(revision, difficulty, GameState::default().game_number)
    }

    /// [`decision`] in game `game_number` of a match.
    fn decision_in_game(
        revision: u64,
        difficulty: AiDifficulty,
        game_number: u8,
    ) -> GameDecisionRequest {
        let mut state = GameState::default();
        state.game_number = game_number;
        decision_on(&state, revision, difficulty)
    }

    /// [`decision`] on `state`.
    fn decision_on(
        state: &GameState,
        revision: u64,
        difficulty: AiDifficulty,
    ) -> GameDecisionRequest {
        build_game_decision_prompt(state, &decision_contract(revision), difficulty, None, &[])
            .unwrap()
    }

    /// The seat's own two-option contract at `revision`.
    fn decision_contract(revision: u64) -> AiDecisionContract {
        seat_contract(SEAT, revision)
    }

    /// [`decision_contract`] for `seat`.
    fn seat_contract(seat: PlayerId, revision: u64) -> AiDecisionContract {
        AiDecisionContract {
            semantic_owner: seat,
            authorized_actor: seat,
            state_revision: revision,
            candidates: [
                GameAction::PassPriority,
                GameAction::ChoosePlayDraw { play_first: true },
            ]
            .into_iter()
            .map(|action| CandidateAction {
                action,
                metadata: ActionMetadata::for_actor(Some(seat), TacticalClass::Utility),
            })
            .collect(),
        }
    }

    /// A log of `count` public events numbered `0..count`, as the client
    /// numbers the log it accumulates.
    fn log(count: u32) -> Vec<GameLogEntry> {
        (0..count)
            .map(|seq| GameLogEntry {
                seq,
                game_number: 1,
                turn: 1,
                phase: Phase::PreCombatMain,
                category: LogCategory::Stack,
                segments: vec![LogSegment::Text(format!("event {seq}"))],
                presentation: LogPresentation {
                    importance: LogImportance::Essential,
                    ..LogPresentation::default()
                },
            })
            .collect()
    }

    /// [`log`] over `turns` turns of `per_turn` events each, `turn T event I`.
    fn turns_log(turns: u32, per_turn: u32) -> Vec<GameLogEntry> {
        log(turns * per_turn)
            .into_iter()
            .map(|mut entry| {
                let (turn, event) = (entry.seq / per_turn + 1, entry.seq % per_turn);
                entry.turn = turn;
                entry.segments = vec![LogSegment::Text(format!("turn {turn} event {event}"))];
                entry
            })
            .collect()
    }

    fn body(spec: &HttpRequestSpec) -> Value {
        serde_json::from_str(&spec.body).unwrap()
    }

    fn reply(id: &str, input_tokens: u64) -> String {
        json!({
            "id": id,
            "type": "message",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{\"choice\": 0, \"reason\": \"hold up mana\"}" }],
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": 20,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
            },
        })
        .to_string()
    }

    /// The seat's pending request, as the transport would hand it back.
    fn pending_turn(memory: &LlmThreadMemory) -> Option<ThreadTurnId> {
        pending_turn_of(memory, SEAT)
    }

    /// [`pending_turn`] for `seat`.
    fn pending_turn_of(memory: &LlmThreadMemory, seat: PlayerId) -> Option<ThreadTurnId> {
        memory
            .seats
            .get(&seat)
            .and_then(|thread| thread.pending.as_ref())
            .map(|pending| pending.turn)
    }

    /// The engine accepted `body`, a 2xx reply to the seat's pending request.
    fn accept_reply(memory: &mut LlmThreadMemory, provider: LlmProvider, body: &str) {
        accept_reply_for(memory, SEAT, provider, body);
    }

    /// [`accept_reply`] for `seat`.
    fn accept_reply_for(
        memory: &mut LlmThreadMemory,
        seat: PlayerId,
        provider: LlmProvider,
        body: &str,
    ) {
        let turn = pending_turn_of(memory, seat);
        let reply = LlmReply {
            provider,
            status: 200,
            body,
        };
        memory.record_reply(seat, turn, reply, Ok(()), CLOCKS);
    }

    /// The API refused to continue the thread the seat's pending request was
    /// sent on, in a reply that arrived at `now`.
    fn lose_thread_at(memory: &mut LlmThreadMemory, code: ThreadErrorCode, now: ClockReading) {
        let turn = pending_turn(memory);
        let reply = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 404,
            body: "{}",
        };
        let error = LlmError::ThreadUnavailable { code };
        memory.record_reply(SEAT, turn, reply, Err(&error), now);
    }

    /// [`lose_thread_at`] for a loss whose timing does not matter.
    fn lose_thread(memory: &mut LlmThreadMemory, code: ThreadErrorCode) {
        lose_thread_at(memory, code, CLOCKS);
    }

    /// Build `decision` for the seat and accept `id` as its answer.
    fn answer(
        memory: &mut LlmThreadMemory,
        config: &LlmEndpointConfig,
        decision: &GameDecisionRequest,
        history: &[GameLogEntry],
        id: &str,
    ) -> HttpRequestSpec {
        answer_for(memory, SEAT, config, decision, history, id)
    }

    /// [`answer`] for `seat`.
    fn answer_for(
        memory: &mut LlmThreadMemory,
        seat: PlayerId,
        config: &LlmEndpointConfig,
        decision: &GameDecisionRequest,
        history: &[GameLogEntry],
        id: &str,
    ) -> HttpRequestSpec {
        let spec = memory
            .decision_request(seat, config, decision, history, CLOCKS)
            .unwrap()
            .request;
        accept_reply_for(memory, seat, LlmProvider::Anthropic, &reply(id, 900));
        spec
    }

    fn thread_field(spec: &HttpRequestSpec) -> Value {
        body(spec)["thread"].clone()
    }

    fn continued_from(spec: &HttpRequestSpec) -> Option<String> {
        let thread = thread_field(spec);
        (thread["type"] == "continue")
            .then(|| thread["previous_message_id"].as_str().unwrap().to_string())
    }

    fn is_create(spec: &HttpRequestSpec) -> bool {
        thread_field(spec) == json!({ "type": "create" })
    }

    fn user_message(spec: &HttpRequestSpec) -> String {
        let body = body(spec);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "{body}");
        messages[0]["content"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_seats_first_decision_creates_its_thread_with_the_full_request() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        let spec = memory
            .decision_request(SEAT, &threaded(), &first, &log(4), CLOCKS)
            .unwrap()
            .request;

        assert_eq!(
            spec,
            build_thread_request(
                &threaded(),
                ThreadTurn::Create {
                    prompt: &first.prompt
                }
            )
            .unwrap()
        );
        let body = body(&spec);
        assert_eq!(body["system"], json!(first.prompt.system));
        assert_eq!(user_message(&spec), first.prompt.user);
        assert_eq!(body["cache_control"], json!({ "type": "ephemeral" }));
        assert!(spec
            .headers
            .iter()
            .any(|header| header.name == "anthropic-beta"
                && header.value == "message-threads-2026-08-12"));
        // Nothing is remembered as the thread until the reply is accepted.
        assert!(!memory.has_open_thread(SEAT));
    }

    #[test]
    fn a_later_decision_sends_only_what_happened_since_the_last_answer() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(4),
            "msg_1",
        );
        assert!(memory.has_open_thread(SEAT));

        let second = decision(2, AiDifficulty::Medium);
        let spec = memory
            .decision_request(SEAT, &threaded(), &second, &log(7), CLOCKS)
            .unwrap()
            .request;

        assert_eq!(continued_from(&spec), Some("msg_1".to_string()));
        assert_eq!(
            Some(user_message(&spec)),
            second.thread_update(&log(7), Some(3))
        );
        let body = body(&spec);
        assert!(body.get("system").is_none(), "{body}");
        let message = user_message(&spec);
        for new in ["event 4", "event 5", "event 6"] {
            assert!(message.contains(new), "{new} missing: {message}");
        }
        for answered in ["event 0", "event 3"] {
            assert!(!message.contains(answered), "{answered} resent: {message}");
        }
    }

    /// The prompt size reported for a decision is what its request carries,
    /// so a thread's smaller continues show up in the measurement.
    #[test]
    fn the_reported_prompt_size_is_what_the_request_carries() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        let create = memory
            .decision_request(SEAT, &threaded(), &first, &log(4), CLOCKS)
            .unwrap();
        assert_eq!(create.prompt_chars, first.prompt.char_count());
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_1", 900));

        let second = decision(2, AiDifficulty::Medium);
        let continuation = memory
            .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
            .unwrap();
        let update = second.thread_update(&log(6), Some(3)).unwrap();
        assert_eq!(continuation.prompt_chars, update.chars().count());
        assert!(continuation.prompt_chars < second.prompt.char_count());
    }

    #[test]
    fn a_failed_decision_leaves_the_thread_at_its_last_answered_turn() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(4),
            "msg_1",
        );

        // The second decision goes out but its reply is never accepted.
        let unanswered = decision(2, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &unanswered, &log(6), CLOCKS)
            .unwrap();
        // A reply to some other decision does not advance it either.
        let elsewhere = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 200,
            body: &reply("msg_x", 900),
        };
        memory.record_reply(
            SEAT,
            Some(ThreadTurnId(u32::MAX)),
            elsewhere,
            Ok(()),
            CLOCKS,
        );

        let third = decision(3, AiDifficulty::Medium);
        let spec = memory
            .decision_request(SEAT, &threaded(), &third, &log(8), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(continued_from(&spec), Some("msg_1".to_string()));
        // Everything since the last ANSWERED decision, the unanswered one's
        // events included.
        let message = user_message(&spec);
        assert!(
            message.contains("event 4") && message.contains("event 7"),
            "{message}"
        );
        assert!(!message.contains("event 3"), "{message}");
    }

    /// A non-2xx reply stored nothing, so the thread goes on from the seat's
    /// last answered turn.
    #[test]
    fn a_reply_that_failed_before_the_api_stored_it_keeps_the_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(4),
            "msg_1",
        );
        let second = decision(2, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
            .unwrap();
        let overloaded = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 529,
            body: r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        };
        let error = LlmError::Provider {
            detail: "HTTP 529: Overloaded".to_string(),
        };
        let turn = pending_turn(&memory);
        memory.record_reply(SEAT, turn, overloaded, Err(&error), CLOCKS);

        let retry = memory
            .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(continued_from(&retry), Some("msg_1".to_string()));
    }

    /// A 2xx reply the engine refuses was still stored by the API, and an id
    /// can be continued only once: continuing from the last answered turn
    /// would be refused, so the next decision starts a new thread instead.
    #[test]
    fn a_stored_reply_the_engine_refuses_starts_a_new_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(4),
            "msg_1",
        );
        let second = decision(2, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
            .unwrap();
        // Whatever the body says, the engine's verdict is what counts here.
        let body = reply("msg_2", 900);
        let stored = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 200,
            body: &body,
        };
        let error = LlmError::ChoiceOutOfRange {
            choice: 9,
            option_count: 2,
        };
        let turn = pending_turn(&memory);
        memory.record_reply(SEAT, turn, stored, Err(&error), CLOCKS);
        assert!(!memory.has_open_thread(SEAT));

        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(7),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next));
    }

    #[test]
    fn a_thread_error_drops_the_thread_and_the_same_decision_goes_out_as_a_create() {
        for code in [
            ThreadErrorCode::NotFound,
            ThreadErrorCode::AlreadyContinued,
            ThreadErrorCode::ContinueInFlight,
            ThreadErrorCode::FingerprintMismatch,
        ] {
            let mut memory = LlmThreadMemory::new();
            answer(
                &mut memory,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(4),
                "msg_1",
            );
            let second = decision(2, AiDifficulty::Medium);
            let lost = memory
                .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
                .unwrap()
                .request;
            assert_eq!(continued_from(&lost), Some("msg_1".to_string()));

            lose_thread(&mut memory, code);
            assert!(!memory.has_open_thread(SEAT), "{code:?}");

            let retry = memory
                .decision_request(SEAT, &threaded(), &second, &log(6), CLOCKS)
                .unwrap()
                .request;
            assert!(is_create(&retry), "{code:?}");
            assert_eq!(body(&retry)["system"], json!(second.prompt.system));
            assert_eq!(user_message(&retry), second.prompt.user, "{code:?}");

            // The recovered decision's reply starts the new thread.
            accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));
            let third = memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(3, AiDifficulty::Medium),
                    &log(8),
                    CLOCKS,
                )
                .unwrap()
                .request;
            assert_eq!(
                continued_from(&third),
                Some("msg_2".to_string()),
                "{code:?}"
            );
        }
    }

    /// A fresh create to an API that refuses threads would only be refused
    /// again, so that endpoint is answered with self-contained requests.
    #[test]
    fn an_api_that_refuses_threads_gets_todays_request_from_then_on() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::UnsupportedRequest);

        let today = build_chat_request(&threaded(), &first.prompt).unwrap();
        let retry = memory
            .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(retry, today);
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_1", 900));
        assert!(!memory.has_open_thread(SEAT));

        // Another endpoint is not covered by that refusal.
        let mut elsewhere = threaded();
        elsewhere.model = "claude-other".to_string();
        let moved = memory
            .decision_request(
                SEAT,
                &elsewhere,
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&moved));
    }

    #[test]
    fn a_thread_past_its_size_budget_restarts_with_a_full_request() {
        let measured = |input_tokens: u64| {
            let mut memory = LlmThreadMemory::new();
            let first = decision(1, AiDifficulty::Medium);
            memory
                .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
                .unwrap();
            accept_reply(
                &mut memory,
                LlmProvider::Anthropic,
                &reply("msg_1", input_tokens),
            );
            memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(2, AiDifficulty::Medium),
                    &log(3),
                    CLOCKS,
                )
                .unwrap()
                .request
        };
        // The reply's 20 output tokens count toward the context it closes.
        let at_budget = measured(THREAD_CONTEXT_BUDGET_TOKENS - 20);
        assert_eq!(continued_from(&at_budget), Some("msg_1".to_string()));
        let past_budget = measured(THREAD_CONTEXT_BUDGET_TOKENS - 19);
        assert!(is_create(&past_budget));
    }

    /// The budget counts every part of the context, not just the uncached
    /// remainder Anthropic reports as `input_tokens`: cache reads and cache
    /// writes each count, so either alone can carry a thread past it, and the
    /// two together just inside it are continued.
    #[test]
    fn the_size_budget_counts_cache_reads_and_writes() {
        // What 10 input and 20 output tokens leave of the budget.
        let room = THREAD_CONTEXT_BUDGET_TOKENS - 30;
        // Cache reads, cache writes, and whether the thread is continued.
        let cases = [
            (room + 1, 0, false),
            (0, room + 1, false),
            (room / 2, room - room / 2, true),
        ];
        for (read, written, continued) in cases {
            let mut memory = LlmThreadMemory::new();
            let first = decision(1, AiDifficulty::Medium);
            memory
                .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
                .unwrap();
            let cached = json!({
                "id": "msg_1",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "{\"choice\": 0}" }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "cache_read_input_tokens": read,
                    "cache_creation_input_tokens": written,
                },
            })
            .to_string();
            accept_reply(&mut memory, LlmProvider::Anthropic, &cached);
            let next = memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(2, AiDifficulty::Medium),
                    &log(3),
                    CLOCKS,
                )
                .unwrap()
                .request;
            assert_eq!(
                continued_from(&next).is_some(),
                continued,
                "{read} cache reads, {written} cache writes: {}",
                body(&next)
            );
        }
    }

    #[test]
    fn a_reply_that_reports_no_usage_is_not_continued() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
            .unwrap();
        let unmeasured = json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{\"choice\": 0}" }],
        })
        .to_string();
        accept_reply(&mut memory, LlmProvider::Anthropic, &unmeasured);
        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next));
    }

    #[test]
    fn a_new_endpoint_model_or_system_prompt_starts_a_new_thread() {
        let mut moved_endpoint = threaded();
        moved_endpoint.base_url = Some("https://proxy.example/v1".to_string());
        let mut other_model = threaded();
        other_model.model = "claude-other".to_string();
        let changes: [(&str, LlmEndpointConfig, AiDifficulty); 3] = [
            ("endpoint", moved_endpoint, AiDifficulty::Medium),
            ("model", other_model, AiDifficulty::Medium),
            // A continue cannot carry a new system prompt.
            ("system prompt", threaded(), AiDifficulty::Hard),
        ];
        for (change, config, difficulty) in changes {
            let mut memory = LlmThreadMemory::new();
            answer(
                &mut memory,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(2),
                "msg_1",
            );
            let next = memory
                .decision_request(SEAT, &config, &decision(2, difficulty), &log(3), CLOCKS)
                .unwrap()
                .request;
            assert!(is_create(&next), "a new {change} must start a new thread");
        }
        // The system prompt alone decides that case: a continue could carry
        // what was logged since the last answer.
        let hard = decision(2, AiDifficulty::Hard);
        assert_ne!(
            hard.prompt.system,
            decision(1, AiDifficulty::Medium).prompt.system
        );
        assert!(hard.thread_update(&log(3), Some(1)).is_some());
        // A change the API allows within a thread does not.
        let mut longer_replies = threaded();
        longer_replies.max_output_tokens = Some(4096);
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        let next = memory
            .decision_request(
                SEAT,
                &longer_replies,
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&next), Some("msg_1".to_string()));
    }

    /// The two near-term difficulties keep reasoning from their window: their
    /// decisions are today's request, whatever the profile's setting.
    #[test]
    fn the_two_near_term_difficulties_never_use_a_thread() {
        for difficulty in [AiDifficulty::VeryEasy, AiDifficulty::Easy] {
            let mut memory = LlmThreadMemory::new();
            for revision in 1..=2 {
                let decision = decision(revision, difficulty);
                let sent = memory
                    .decision_request(SEAT, &threaded(), &decision, &log(4), CLOCKS)
                    .unwrap();
                assert_eq!(
                    sent.request,
                    build_chat_request(&threaded(), &decision.prompt).unwrap(),
                    "{difficulty:?}"
                );
                accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg", 900));
                assert!(!memory.has_open_thread(SEAT), "{difficulty:?}");
            }
        }
    }

    /// More was logged since the last answer than the seat's window shows,
    /// at every difficulty that keeps a thread: the decision still continues
    /// the thread. Exactly the window goes entry by entry; one entry more
    /// moves exactly the oldest into its turn's line of key events.
    #[test]
    fn a_continue_past_the_history_window_keeps_the_thread() {
        for difficulty in [
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ] {
            let window = crate::prompt::history_budget(difficulty).current_cycle_entries as u32;
            let after = |unseen: u32| {
                let mut memory = LlmThreadMemory::new();
                answer(
                    &mut memory,
                    &threaded(),
                    &decision(1, difficulty),
                    &log(4),
                    "msg_1",
                );
                memory
                    .decision_request(
                        SEAT,
                        &threaded(),
                        &decision(2, difficulty),
                        &log(4 + unseen),
                        CLOCKS,
                    )
                    .unwrap()
                    .request
            };
            // Entries 4 onward were logged after the answer.
            let newest = format!("T1 PreCombatMain: event {}\n", 3 + window);

            let at_window = after(window);
            assert_eq!(
                continued_from(&at_window),
                Some("msg_1".to_string()),
                "{difficulty:?}"
            );
            let message = user_message(&at_window);
            assert!(message.contains("T1 PreCombatMain: event 4\n"), "{message}");
            assert!(message.contains(&newest), "{message}");
            assert!(
                !message.contains("SINCE YOUR LAST DECISION, EARLIER"),
                "{message}"
            );

            let past_window = after(window + 1);
            assert_eq!(
                continued_from(&past_window),
                Some("msg_1".to_string()),
                "{difficulty:?}"
            );
            let message = user_message(&past_window);
            assert!(
                message.contains(
                    "--- SINCE YOUR LAST DECISION, EARLIER (key events, one line per turn) ---\n\
                     T1 (Turn 1, continued below): event 4\n\
                     --- SINCE YOUR LAST DECISION (oldest first) ---\n\
                     T1 PreCombatMain: event 5\n"
                ),
                "{message}"
            );
            let newest = format!("T1 PreCombatMain: event {}\n", 4 + window);
            assert!(message.contains(&newest), "{message}");
        }
    }

    /// A four-seat table with two LLM seats. Each turn logs a whole history
    /// window of events, so a round of the other seats' turns logs several
    /// windows between two of one seat's decisions. Each LLM seat still
    /// continues its own thread at every decision, across the other seat's
    /// accepted replies, and each continue starts after that seat's own last
    /// answer.
    #[test]
    fn two_seats_at_a_busy_table_each_continue_their_own_thread() {
        let window =
            crate::prompt::history_budget(AiDifficulty::Medium).current_cycle_entries as u32;
        let table = GameState::new(FormatConfig::commander(), 4, 1);
        let decide = |seat: PlayerId, revision: u64| {
            build_game_decision_prompt(
                &table,
                &seat_contract(seat, revision),
                AiDifficulty::Medium,
                None,
                &[],
            )
            .unwrap()
        };
        let history = turns_log(9, window);
        let through_turn = |turn: u32| &history[..(turn * window) as usize];
        let last_event_of =
            |turn: u32| format!("T{turn} PreCombatMain: turn {turn} event {}\n", window - 1);
        let (first, second) = (PlayerId(1), PlayerId(3));
        let mut memory = LlmThreadMemory::new();
        answer_for(
            &mut memory,
            first,
            &threaded(),
            &decide(first, 1),
            through_turn(1),
            "msg_first_1",
        );
        answer_for(
            &mut memory,
            second,
            &threaded(),
            &decide(second, 2),
            through_turn(1),
            "msg_second_1",
        );

        // The first seat, four turns after its answer.
        let sent = memory
            .decision_request(
                first,
                &threaded(),
                &decide(first, 3),
                through_turn(5),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&sent), Some("msg_first_1".to_string()));
        let message = user_message(&sent);
        assert!(
            message.contains("T2 (Turn 2): turn 2 event 0; turn 2 event 1;"),
            "{message}"
        );
        assert!(
            message.contains("T4 (Turn 4): turn 4 event 0;"),
            "{message}"
        );
        assert!(
            message.contains("T5 PreCombatMain: turn 5 event 0\n"),
            "{message}"
        );
        assert!(message.contains(&last_event_of(5)), "{message}");
        assert!(!message.contains("turn 1 event"), "answered: {message}");
        accept_reply_for(
            &mut memory,
            first,
            LlmProvider::Anthropic,
            &reply("msg_first_2", 900),
        );

        // The second seat, five turns after its answer: its thread is where
        // its own reply left it.
        let sent = memory
            .decision_request(
                second,
                &threaded(),
                &decide(second, 4),
                through_turn(6),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&sent), Some("msg_second_1".to_string()));
        let message = user_message(&sent);
        assert!(
            message.contains("T2 (Turn 2): turn 2 event 0;"),
            "{message}"
        );
        assert!(message.contains(&last_event_of(6)), "{message}");
        accept_reply_for(
            &mut memory,
            second,
            LlmProvider::Anthropic,
            &reply("msg_second_2", 900),
        );

        // The first seat again: from its own last answer, at the end of turn
        // 5, not from the second seat's, at the end of turn 6.
        let sent = memory
            .decision_request(
                first,
                &threaded(),
                &decide(first, 5),
                through_turn(9),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&sent), Some("msg_first_2".to_string()));
        let message = user_message(&sent);
        assert!(
            message.contains("T6 (Turn 6): turn 6 event 0;"),
            "{message}"
        );
        assert!(message.contains(&last_event_of(9)), "{message}");
        assert!(!message.contains("turn 5 event"), "answered: {message}");
    }

    /// The next game of a match replaces the game in place; its first
    /// decision starts a new thread rather than continuing the last game's.
    #[test]
    fn the_next_game_of_a_match_starts_a_new_thread() {
        let mut memory = LlmThreadMemory::new();
        let game_one = decision_in_game(1, AiDifficulty::Medium, 1);
        answer(&mut memory, &threaded(), &game_one, &log(4), "msg_1");
        let same_game = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision_in_game(2, AiDifficulty::Medium, 1),
                &log(6),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&same_game), Some("msg_1".to_string()));

        let mut memory = LlmThreadMemory::new();
        answer(&mut memory, &threaded(), &game_one, &log(4), "msg_1");
        let next_game = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision_in_game(2, AiDifficulty::Medium, 2),
                &log(6),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next_game));
    }

    /// The client's log runs on across the games of a match, so the next
    /// game's first decision is handed the last game's entries too, even a
    /// game that ended before its first turn (game one here logs only at turn
    /// 0). Its new thread is created with the game in progress only.
    #[test]
    fn the_next_games_thread_is_created_with_that_games_history_only() {
        let game_one: Vec<GameLogEntry> = log(4)
            .into_iter()
            .map(|mut entry| {
                entry.turn = 0;
                entry
            })
            .collect();
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision_in_game(1, AiDifficulty::Medium, 1),
            &game_one,
            "msg_1",
        );

        // Game two's start is logged at turn 0, then its first turn begins.
        let mut history = game_one;
        for (seq, turn, text) in [(4, 0, "game two started"), (5, 1, "game two land")] {
            let mut entry = log(seq + 1).pop().unwrap();
            entry.game_number = 2;
            entry.turn = turn;
            entry.segments = vec![LogSegment::Text(text.to_string())];
            history.push(entry);
        }
        let mut state = GameState::default();
        state.game_number = 2;
        state.turn_number = 1;
        let game_two = build_game_decision_prompt(
            &state,
            &decision_contract(2),
            AiDifficulty::Medium,
            None,
            &history,
        )
        .unwrap();
        let sent = memory
            .decision_request(SEAT, &threaded(), &game_two, &history, CLOCKS)
            .unwrap()
            .request;
        assert!(is_create(&sent));
        let prompt = user_message(&sent);
        assert!(prompt.contains("game two land"), "{prompt}");
        assert!(!prompt.contains("event 1"), "game one leaked: {prompt}");
    }

    /// A decision between games belongs to no game: it is today's request,
    /// and it leaves no thread for the next game to continue.
    #[test]
    fn a_decision_between_games_uses_no_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision_in_game(1, AiDifficulty::Medium, 1),
            &log(4),
            "msg_1",
        );

        let mut between = GameState::default();
        between.game_number = 2;
        between.match_phase = engine::types::match_config::MatchPhase::BetweenGames;
        let play_draw = decision_on(&between, 2, AiDifficulty::Medium);
        let sent = memory
            .decision_request(SEAT, &threaded(), &play_draw, &log(5), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(
            sent,
            build_chat_request(&threaded(), &play_draw.prompt).unwrap()
        );
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));
        assert!(!memory.has_open_thread(SEAT));

        let next_game = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision_in_game(3, AiDifficulty::Medium, 2),
                &log(6),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next_game));
    }

    /// An endpoint's refusal of threads outlasts the game it was learned in:
    /// a decision between games that uses no thread does not forget it.
    #[test]
    fn a_refused_endpoint_stays_self_contained_across_a_decision_between_games() {
        let mut memory = LlmThreadMemory::new();
        let first = decision_in_game(1, AiDifficulty::Medium, 1);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::UnsupportedRequest);

        let mut between = GameState::default();
        between.game_number = 2;
        between.match_phase = engine::types::match_config::MatchPhase::BetweenGames;
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision_on(&between, 2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap();

        let next_game = decision_in_game(3, AiDifficulty::Medium, 2);
        let sent = memory
            .decision_request(SEAT, &threaded(), &next_game, &log(4), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(
            sent,
            build_chat_request(&threaded(), &next_game.prompt).unwrap()
        );
    }

    /// A thread is stored by the account that created it: a new key at the
    /// same endpoint and model starts a new thread, and an account's refusal
    /// of threads says nothing about another account's.
    #[test]
    fn a_new_api_key_starts_a_new_thread_and_forgets_the_old_accounts_refusal() {
        let mut new_key = threaded();
        new_key.api_key = "other-secret".to_string();

        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        let after_accept = memory
            .decision_request(
                SEAT,
                &new_key,
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&after_accept));

        let mut memory = LlmThreadMemory::new();
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(2),
                CLOCKS,
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::UnsupportedRequest);
        let after_refusal = memory
            .decision_request(
                SEAT,
                &new_key,
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&after_refusal));
    }

    /// Lose the seat's thread on `threaded()` in game one, then recover it with
    /// a create built at `recovered_at`, whose reply `msg_2` is accepted.
    fn lose_and_recover(memory: &mut LlmThreadMemory, recovered_at: ClockReading) {
        answer(
            memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap();
        lose_thread(memory, ThreadErrorCode::NotFound);
        let recovery = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                recovered_at,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovery));
        accept_reply(memory, LlmProvider::Anthropic, &reply("msg_2", 900));
    }

    /// The recovery contract: when the first continue of a thread created to
    /// recover a lost one is lost too, while the thread cannot have expired
    /// yet, the API cannot store that conversation. Its decisions are
    /// self-contained from then on, instead of a lost continue and a create
    /// each; every other conversation, on the same endpoint, still uses
    /// threads.
    #[test]
    fn a_recovery_thread_lost_before_it_could_expire_makes_its_conversation_self_contained() {
        let mut memory = LlmThreadMemory::new();
        lose_and_recover(&mut memory, in_step(1_000));
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                in_step(1_001),
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        // Reported lost as late as the clocks still rule the expiry out, and
        // as far apart as they may be while they agree.
        let last_within = 1_000 + THREAD_IDLE_EXPIRY_MS - CLOCK_AGREEMENT_MS - 1;
        lose_thread_at(
            &mut memory,
            ThreadErrorCode::NotFound,
            ClockReading {
                wall_ms: last_within,
                monotonic_ms: last_within - CLOCK_AGREEMENT_MS,
            },
        );
        for revision in 3..=5 {
            let decision = decision(revision, AiDifficulty::Medium);
            let sent = memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision,
                    &log(revision as u32 + 2),
                    CLOCKS,
                )
                .unwrap()
                .request;
            assert_eq!(
                sent,
                build_chat_request(&threaded(), &decision.prompt).unwrap()
            );
        }

        // The endpoint is not given up: the match's next game threads again.
        let next_game = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision_in_game(6, AiDifficulty::Medium, 2),
                &log(9),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next_game));
    }

    /// A replacement thread left idle past the API's expiry before its first
    /// continue has simply expired: it is recovered again, and the endpoint is
    /// never treated as one that cannot store the conversation.
    #[test]
    fn a_recovery_thread_that_expired_before_its_first_continue_is_recovered_again() {
        let mut memory = LlmThreadMemory::new();
        lose_and_recover(&mut memory, in_step(1_000));
        let after_the_pause = 1_000 + THREAD_IDLE_EXPIRY_MS;
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                in_step(after_the_pause),
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        lose_thread_at(
            &mut memory,
            ThreadErrorCode::NotFound,
            in_step(after_the_pause),
        );
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                in_step(after_the_pause),
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered));

        // That recovery is bounded like the first: lost again right away, its
        // conversation is self-contained.
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_3", 900));
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
                in_step(after_the_pause + 1),
            )
            .unwrap();
        lose_thread_at(
            &mut memory,
            ThreadErrorCode::NotFound,
            in_step(after_the_pause + 1),
        );
        let after = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(
            after,
            build_chat_request(&threaded(), &decision(4, AiDifficulty::Medium).prompt).unwrap()
        );
    }

    /// A clock correction can step the wall clock back past an earlier
    /// request's reading, while the monotonic clock runs on. A replacement
    /// thread reported lost at its first continue then has an unknown idle
    /// time, which is read as an ordinary expiry: the conversation is
    /// recovered again, not made self-contained.
    #[test]
    fn a_recovery_thread_lost_after_the_clock_stepped_back_is_recovered_again() {
        let mut memory = LlmThreadMemory::new();
        let recovered_at = 10 * THREAD_IDLE_EXPIRY_MS;
        lose_and_recover(&mut memory, in_step(recovered_at));
        let stepped_back = ClockReading {
            wall_ms: recovered_at - 1_000,
            monotonic_ms: recovered_at + 1,
        };
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                stepped_back,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        lose_thread_at(&mut memory, ThreadErrorCode::NotFound, stepped_back);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                stepped_back,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered));
    }

    /// A continue can wait a long time after it was built, behind a busy
    /// worker or in flight. The recovery thread's first continue is built a
    /// moment after its create, well within the API's expiry, and the loss is
    /// reported once both clocks have run past it. The thread may simply have
    /// expired, so the conversation gets one fresh create, as any expired
    /// thread does, and is not made self-contained.
    #[test]
    fn a_continue_built_before_a_pause_across_the_expiry_is_recovered_again() {
        let mut memory = LlmThreadMemory::new();
        lose_and_recover(&mut memory, in_step(1_000));
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                in_step(1_001),
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));

        let after_the_pause = in_step(1_000 + THREAD_IDLE_EXPIRY_MS);
        lose_thread_at(&mut memory, ThreadErrorCode::NotFound, after_the_pause);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                after_the_pause,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered), "{}", body(&recovered));
    }

    /// The machine sleeps for 310 seconds, longer than a thread lives, while a
    /// recovery thread's first continue waits on its build. The monotonic
    /// clock stops for the sleep, and a correction steps the wall clock back
    /// twenty seconds within it, so neither clock alone shows the expiry: the
    /// wall clock has moved 290 seconds, the monotonic clock almost none. The
    /// two disagree, so the idle time is unknown. The API reports the thread
    /// lost, and the conversation gets exactly one fresh create, which then
    /// threads as before; it is not made self-contained.
    #[test]
    fn a_recovery_thread_lost_after_a_sleep_and_a_clock_step_is_recovered_once() {
        let mut memory = LlmThreadMemory::new();
        lose_and_recover(&mut memory, in_step(1_000));
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                in_step(1_001),
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));

        let after_sleep = ClockReading {
            wall_ms: 1_001 + 310_000 - 20_000,
            monotonic_ms: 1_002,
        };
        lose_thread_at(&mut memory, ThreadErrorCode::NotFound, after_sleep);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                after_sleep,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered), "{}", body(&recovered));
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_3", 900));
        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
                after_sleep,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&next), Some("msg_3".to_string()));
    }

    /// A sleep earlier in a recovery thread's life leaves its idle time
    /// unknown for good: here the machine sleeps, with the same stopped
    /// monotonic clock and stepped-back wall clock, while the recovery's own
    /// create is in flight. Its first continue is then built and lost within
    /// a few milliseconds by either clock, but the interval from the create
    /// still spans the sleep, so the conversation is recovered again.
    #[test]
    fn a_sleep_before_a_recovery_threads_first_continue_still_reads_as_a_possible_expiry() {
        let mut memory = LlmThreadMemory::new();
        lose_and_recover(&mut memory, in_step(1_000));
        let built = ClockReading {
            wall_ms: 1_000 + 310_000 - 20_000,
            monotonic_ms: 1_001,
        };
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                built,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        let reported = ClockReading {
            wall_ms: built.wall_ms + 2,
            monotonic_ms: built.monotonic_ms + 2,
        };
        lose_thread_at(&mut memory, ThreadErrorCode::NotFound, reported);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                reported,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered), "{}", body(&recovered));
    }

    /// A recovery belongs to the conversation it replaces. The seat's thread
    /// on one account is lost and the create replacing it fails before the
    /// API stores anything; a create for another account, or for the next
    /// game, is then an ordinary thread with a first recovery of its own, while
    /// the lost conversation itself still falls back after a second loss.
    #[test]
    fn a_recovery_belongs_to_the_conversation_it_replaces() {
        let mut other_account = threaded();
        other_account.api_key = "other-secret".to_string();
        let game_one = |revision| decision_in_game(revision, AiDifficulty::Medium, 1);
        let game_two = |revision| decision_in_game(revision, AiDifficulty::Medium, 2);
        let lose_and_fail_the_recovery = |memory: &mut LlmThreadMemory| {
            answer(memory, &threaded(), &game_one(1), &log(2), "msg_1");
            memory
                .decision_request(SEAT, &threaded(), &game_one(2), &log(3), CLOCKS)
                .unwrap();
            lose_thread(memory, ThreadErrorCode::NotFound);
            let recovery = memory
                .decision_request(SEAT, &threaded(), &game_one(2), &log(3), CLOCKS)
                .unwrap()
                .request;
            assert!(is_create(&recovery));
            let unavailable = LlmReply {
                provider: LlmProvider::Anthropic,
                status: 503,
                body: "{}",
            };
            let error = LlmError::Provider {
                detail: "overloaded".to_string(),
            };
            memory.record_reply(SEAT, pending_turn(memory), unavailable, Err(&error), CLOCKS);
        };

        for (config, next) in [(&other_account, game_one(3)), (&threaded(), game_two(3))] {
            let mut memory = LlmThreadMemory::new();
            lose_and_fail_the_recovery(&mut memory);
            answer(&mut memory, config, &next, &log(4), "msg_2");
            let first_continue = memory
                .decision_request(SEAT, config, &next, &log(5), CLOCKS)
                .unwrap()
                .request;
            assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
            lose_thread(&mut memory, ThreadErrorCode::NotFound);
            let recovered = memory
                .decision_request(SEAT, config, &next, &log(5), CLOCKS)
                .unwrap()
                .request;
            assert!(is_create(&recovered), "{}", body(&recovered));
        }

        // The lost conversation's own recovery still falls back when its
        // first continue is lost too.
        let mut memory = LlmThreadMemory::new();
        lose_and_fail_the_recovery(&mut memory);
        answer(&mut memory, &threaded(), &game_one(2), &log(3), "msg_2");
        memory
            .decision_request(SEAT, &threaded(), &game_one(3), &log(4), CLOCKS)
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        let after = memory
            .decision_request(SEAT, &threaded(), &game_one(3), &log(4), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(
            after,
            build_chat_request(&threaded(), &game_one(3).prompt).unwrap()
        );
    }

    /// Make `config`'s conversation one the API cannot store: its thread is
    /// answered and lost, and the create recovering it is lost at its first
    /// continue right away. `revision` numbers the first of the three
    /// decisions this takes.
    fn make_unstorable(memory: &mut LlmThreadMemory, config: &LlmEndpointConfig, revision: u64) {
        let medium = |revision| decision(revision, AiDifficulty::Medium);
        answer(memory, config, &medium(revision), &log(2), "msg_1");
        memory
            .decision_request(SEAT, config, &medium(revision + 1), &log(3), CLOCKS)
            .unwrap();
        lose_thread(memory, ThreadErrorCode::NotFound);
        answer(memory, config, &medium(revision + 1), &log(3), "msg_2");
        let first_continue = memory
            .decision_request(SEAT, config, &medium(revision + 2), &log(4), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        lose_thread(memory, ThreadErrorCode::NotFound);
    }

    /// The seat keeps every conversation the API could not store. Model A's
    /// conversation becomes self-contained, then model B's; switching back to
    /// A keeps A's decisions self-contained instead of starting its
    /// create-and-fail cycle again, and B's stay self-contained too.
    #[test]
    fn every_conversation_the_api_cannot_store_stays_self_contained() {
        let model_a = threaded();
        let mut model_b = threaded();
        model_b.model = "claude-other".to_string();

        let mut memory = LlmThreadMemory::new();
        make_unstorable(&mut memory, &model_a, 1);
        make_unstorable(&mut memory, &model_b, 4);
        for config in [&model_a, &model_b, &model_a] {
            let decision = decision(7, AiDifficulty::Medium);
            let sent = memory
                .decision_request(SEAT, config, &decision, &log(5), CLOCKS)
                .unwrap()
                .request;
            assert_eq!(
                sent,
                build_chat_request(config, &decision.prompt).unwrap(),
                "{}",
                config.model
            );
        }
    }

    /// A recovery is owed to its conversation until that conversation has an
    /// answered turn again. Model A's thread is lost and the create replacing
    /// it fails before the API stores anything. The seat then turns to
    /// something else: model B, whose thread is answered and then lost too,
    /// or a decision sent without a thread (the setting turned off). Back on
    /// A, the create is still A's recovery, so losing it at its first continue
    /// right away makes A self-contained, without one more create-and-fail
    /// cycle.
    #[test]
    fn a_conversation_still_owes_its_recovery_after_the_seat_switches_away_and_back() {
        /// Where the seat turns between losing A's thread and coming back.
        #[derive(Debug, Clone, Copy)]
        enum Detour {
            AnotherModel,
            NoThread,
        }

        let model_a = threaded();
        let mut model_b = threaded();
        model_b.model = "claude-other".to_string();
        let unthreaded = endpoint(LlmProvider::Anthropic, DecisionContext::SelfContained);
        let medium = |revision| decision(revision, AiDifficulty::Medium);
        for detour in [Detour::AnotherModel, Detour::NoThread] {
            let mut memory = LlmThreadMemory::new();
            answer(&mut memory, &model_a, &medium(1), &log(2), "msg_1");
            memory
                .decision_request(SEAT, &model_a, &medium(2), &log(3), CLOCKS)
                .unwrap();
            lose_thread(&mut memory, ThreadErrorCode::NotFound);
            let failed_recovery = memory
                .decision_request(SEAT, &model_a, &medium(2), &log(3), CLOCKS)
                .unwrap()
                .request;
            assert!(is_create(&failed_recovery), "{detour:?}");
            let unavailable = LlmReply {
                provider: LlmProvider::Anthropic,
                status: 503,
                body: "{}",
            };
            let error = LlmError::Provider {
                detail: "overloaded".to_string(),
            };
            memory.record_reply(
                SEAT,
                pending_turn(&memory),
                unavailable,
                Err(&error),
                CLOCKS,
            );

            match detour {
                Detour::AnotherModel => {
                    answer(&mut memory, &model_b, &medium(3), &log(4), "msg_2");
                    memory
                        .decision_request(SEAT, &model_b, &medium(4), &log(5), CLOCKS)
                        .unwrap();
                    lose_thread(&mut memory, ThreadErrorCode::NotFound);
                }
                Detour::NoThread => {
                    memory
                        .decision_request(SEAT, &unthreaded, &medium(3), &log(4), CLOCKS)
                        .unwrap();
                }
            }

            answer(&mut memory, &model_a, &medium(5), &log(6), "msg_3");
            let first_continue = memory
                .decision_request(SEAT, &model_a, &medium(6), &log(7), CLOCKS)
                .unwrap()
                .request;
            assert_eq!(
                continued_from(&first_continue),
                Some("msg_3".to_string()),
                "{detour:?}"
            );
            lose_thread(&mut memory, ThreadErrorCode::NotFound);
            let after = memory
                .decision_request(SEAT, &model_a, &medium(6), &log(7), CLOCKS)
                .unwrap()
                .request;
            assert_eq!(
                after,
                build_chat_request(&model_a, &medium(6).prompt).unwrap(),
                "{detour:?}"
            );
        }
    }

    /// Only a recovery thread's first continue counts: a thread that has been
    /// continued, and is later lost, is recovered again.
    #[test]
    fn a_thread_lost_after_it_was_continued_is_recovered_again() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap();
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));
        // The recovery thread's first continue succeeds...
        let first_continue = answer(
            &mut memory,
            &threaded(),
            &decision(3, AiDifficulty::Medium),
            &log(4),
            "msg_3",
        );
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        // ...so a later loss is an ordinary one.
        let second_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&second_continue), Some("msg_3".to_string()));
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&recovered));
    }

    /// Only a lost thread (`thread_not_found`) makes its replacement a
    /// recovery. A thread the API still holds, recovered from after
    /// `thread_already_continued` and its siblings, is an ordinary one: losing
    /// it at its first continue is recovered again, not a reason to give
    /// threads up.
    #[test]
    fn only_a_lost_thread_makes_its_replacement_a_recovery() {
        for code in [
            ThreadErrorCode::AlreadyContinued,
            ThreadErrorCode::ContinueInFlight,
            ThreadErrorCode::FingerprintMismatch,
        ] {
            let mut memory = LlmThreadMemory::new();
            answer(
                &mut memory,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(2),
                "msg_1",
            );
            memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(2, AiDifficulty::Medium),
                    &log(3),
                    CLOCKS,
                )
                .unwrap();
            lose_thread(&mut memory, code);
            answer(
                &mut memory,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                "msg_2",
            );

            let first_continue = memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(3, AiDifficulty::Medium),
                    &log(4),
                    CLOCKS,
                )
                .unwrap()
                .request;
            assert_eq!(
                continued_from(&first_continue),
                Some("msg_2".to_string()),
                "{code:?}"
            );
            lose_thread(&mut memory, ThreadErrorCode::NotFound);
            let recovered = memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(3, AiDifficulty::Medium),
                    &log(4),
                    CLOCKS,
                )
                .unwrap()
                .request;
            assert!(is_create(&recovered), "{code:?}");
        }
    }

    /// A cancelled attempt's late reply belongs to its own request: it never
    /// touches the request that replaced it for the same decision.
    #[test]
    fn a_late_reply_to_a_replaced_request_leaves_the_new_one_pending() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        let second = decision(2, AiDifficulty::Medium);
        let cancelled = memory
            .decision_request(SEAT, &threaded(), &second, &log(3), CLOCKS)
            .unwrap()
            .thread_turn;
        let live = memory
            .decision_request(SEAT, &threaded(), &second, &log(3), CLOCKS)
            .unwrap()
            .thread_turn;
        assert_ne!(cancelled, live);

        let lost = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 400,
            body: "{}",
        };
        let error = LlmError::ThreadUnavailable {
            code: ThreadErrorCode::ContinueInFlight,
        };
        memory.record_reply(SEAT, cancelled, lost, Err(&error), CLOCKS);
        assert!(
            memory.has_open_thread(SEAT),
            "the late reply dropped the thread"
        );

        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));
        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&next), Some("msg_2".to_string()));
    }

    /// Usage counts come from across the network: an oversized report is not
    /// a measurement, and its thread is not continued.
    #[test]
    fn an_oversized_usage_report_is_not_continued() {
        let mut memory = LlmThreadMemory::new();
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(2),
                CLOCKS,
            )
            .unwrap();
        let oversized = json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{\"choice\": 0}" }],
            "usage": { "input_tokens": u64::MAX, "output_tokens": 2 },
        })
        .to_string();
        accept_reply(&mut memory, LlmProvider::Anthropic, &oversized);
        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&next));
    }

    /// A thread created on an empty log has seen nothing, so it may continue
    /// only with a log that starts at the client's first entry.
    #[test]
    fn a_thread_created_on_an_empty_log_needs_the_log_from_its_first_entry() {
        let after = |history: Vec<GameLogEntry>| {
            let mut memory = LlmThreadMemory::new();
            answer(
                &mut memory,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &[],
                "msg_1",
            );
            memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(2, AiDifficulty::Medium),
                    &history,
                    CLOCKS,
                )
                .unwrap()
                .request
        };
        assert_eq!(continued_from(&after(log(3))), Some("msg_1".to_string()));
        // Entries 0 to 699 were trimmed off the hand-over.
        let trimmed: Vec<GameLogEntry> = log(703).split_off(700);
        assert!(is_create(&after(trimmed)));
    }

    /// While another player controls the seat's turn, the decisions they make
    /// for it never touch the seat's own thread, in either direction.
    #[test]
    fn a_decision_another_player_makes_for_the_seat_uses_no_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );

        let mut delegated = decision_contract(2);
        delegated.authorized_actor = PlayerId(2);
        let controlled = build_game_decision_prompt(
            &GameState::default(),
            &delegated,
            AiDifficulty::Medium,
            None,
            &[],
        )
        .unwrap();
        let sent = memory
            .decision_request(SEAT, &threaded(), &controlled, &log(3), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(
            sent,
            build_chat_request(&threaded(), &controlled.prompt).unwrap()
        );

        // Control ends: like every decision that may not use a thread, the
        // controlled one ended the seat's thread, so the seat starts afresh.
        let own = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&own));
    }

    /// The thread setting off, or any protocol but Anthropic Messages, sends
    /// exactly the request a game decision has always sent.
    #[test]
    fn an_opted_out_profile_and_every_other_protocol_send_todays_request() {
        let mut cases = vec![endpoint(
            LlmProvider::Anthropic,
            DecisionContext::SelfContained,
        )];
        for provider in [
            LlmProvider::OpenAi,
            LlmProvider::Gemini,
            LlmProvider::DeepSeek,
            LlmProvider::Jev,
        ] {
            cases.push(endpoint(provider, DecisionContext::MessageThread));
            cases.push(endpoint(provider, DecisionContext::SelfContained));
        }
        for config in cases {
            let mut memory = LlmThreadMemory::new();
            for revision in 1..=3 {
                let decision = decision(revision, AiDifficulty::Medium);
                let spec = memory
                    .decision_request(SEAT, &config, &decision, &log(revision as u32 * 2), CLOCKS)
                    .unwrap()
                    .request;
                assert_eq!(
                    spec,
                    build_chat_request(&config, &decision.prompt).unwrap(),
                    "{:?} {:?}",
                    config.provider,
                    config.decision_context
                );
                accept_reply(&mut memory, config.provider, &reply("msg", 900));
                assert!(!memory.has_open_thread(SEAT));
            }
        }
    }

    #[test]
    fn turning_the_setting_off_forgets_the_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        let off = endpoint(LlmProvider::Anthropic, DecisionContext::SelfContained);
        memory
            .decision_request(
                SEAT,
                &off,
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap();
        assert!(!memory.has_open_thread(SEAT));
        let back_on = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&back_on));
    }

    /// The caller numbers each change to a seat's provider settings, so a
    /// change undone before any decision saw it still starts a new thread:
    /// here message threads are turned off and on again while a continue is
    /// built, and the caller drops that continue unsent. A seat's thread is
    /// kept only under the settings revision it was opened under; the same
    /// revision noted again, or another seat's, keeps it.
    #[test]
    fn a_new_settings_revision_starts_a_new_thread() {
        let mut memory = LlmThreadMemory::new();
        memory.observe_settings_revision(SEAT, 0);
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        memory.observe_settings_revision(SEAT, 0);
        memory.observe_settings_revision(PlayerId(2), 7);
        let dropped = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&dropped), Some("msg_1".to_string()));

        // Off, then on again: two revisions later.
        memory.observe_settings_revision(SEAT, 2);
        assert!(!memory.has_open_thread(SEAT));
        assert_eq!(pending_turn(&memory), None);
        let back_on = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&back_on), "{}", body(&back_on));
    }

    /// A new settings revision forgets the seat's thread and nothing else the
    /// seat knows: a conversation the API cannot store stays self-contained,
    /// an endpoint that refused threads stays refused, and a conversation
    /// still owed its recovery gets one, so losing that recovery thread at
    /// its first continue, within its lifetime, makes the conversation
    /// self-contained.
    #[test]
    fn a_new_settings_revision_keeps_what_the_seat_knows_of_each_conversation() {
        let medium = |revision| decision(revision, AiDifficulty::Medium);
        let self_contained =
            |decision: &GameDecisionRequest| build_chat_request(&threaded(), &decision.prompt);

        let mut unstorable = LlmThreadMemory::new();
        make_unstorable(&mut unstorable, &threaded(), 1);
        unstorable.observe_settings_revision(SEAT, 1);
        let sent = unstorable
            .decision_request(SEAT, &threaded(), &medium(4), &log(5), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(sent, self_contained(&medium(4)).unwrap());

        let mut refused = LlmThreadMemory::new();
        refused
            .decision_request(SEAT, &threaded(), &medium(1), &log(2), CLOCKS)
            .unwrap();
        lose_thread(&mut refused, ThreadErrorCode::UnsupportedRequest);
        refused.observe_settings_revision(SEAT, 1);
        let sent = refused
            .decision_request(SEAT, &threaded(), &medium(2), &log(3), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(sent, self_contained(&medium(2)).unwrap());

        let mut owed = LlmThreadMemory::new();
        answer(&mut owed, &threaded(), &medium(1), &log(2), "msg_1");
        owed.decision_request(SEAT, &threaded(), &medium(2), &log(3), CLOCKS)
            .unwrap();
        lose_thread(&mut owed, ThreadErrorCode::NotFound);
        owed.observe_settings_revision(SEAT, 1);
        let recovery = answer(&mut owed, &threaded(), &medium(2), &log(3), "msg_2");
        assert!(is_create(&recovery));
        let first_continue = owed
            .decision_request(SEAT, &threaded(), &medium(3), &log(4), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        lose_thread(&mut owed, ThreadErrorCode::NotFound);
        let sent = owed
            .decision_request(SEAT, &threaded(), &medium(4), &log(5), CLOCKS)
            .unwrap()
            .request;
        assert_eq!(sent, self_contained(&medium(4)).unwrap());
    }

    #[test]
    fn a_refused_reply_is_never_continued() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2), CLOCKS)
            .unwrap();
        let refused = json!({
            "id": "msg_1",
            "stop_reason": "refusal",
            "content": [{ "type": "text", "text": "{\"choice\": 0}" }],
            "usage": { "input_tokens": 10, "output_tokens": 1 },
        })
        .to_string();
        accept_reply(&mut memory, LlmProvider::Anthropic, &refused);
        assert!(!memory.has_open_thread(SEAT));
    }

    #[test]
    fn a_log_rewound_under_the_thread_starts_a_new_one() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(8),
            "msg_1",
        );
        let rewound = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&rewound));
    }

    #[test]
    fn each_seat_keeps_its_own_thread() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        let other_seat = PlayerId(2);
        assert!(!memory.has_open_thread(other_seat));
        let spec = memory
            .decision_request(
                other_seat,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert!(is_create(&spec));
        // The other seat's request leaves this seat's thread where it was.
        let own = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
                CLOCKS,
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&own), Some("msg_1".to_string()));
        memory.clear();
        assert!(!memory.has_open_thread(SEAT));
    }
}
