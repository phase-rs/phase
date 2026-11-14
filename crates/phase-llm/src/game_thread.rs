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
//!   continues from the id of the seat's last accepted reply.
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
//!   changes,
//!   when the system prompt changes (difficulty, format, or the table itself),
//!   when the last reply reported more context than
//!   [`THREAD_CONTEXT_BUDGET_TOKENS`] or no usage at all, when the log was
//!   rewound under the thread, and when a continue cannot carry everything
//!   since the seat's last answer: more was logged than its history window
//!   shows, or the log handed over no longer reaches back that far (the full
//!   request summarizes older turns, which a continue cannot).
//! - **No thread at all.** The two lowest difficulties never use one: they
//!   deliberately reason from a near-term window, which a thread would undo.
//!   Neither does a decision between games (the play/draw choice), which
//!   belongs to no game, nor one another player makes for the seat while they
//!   control its turn, which must neither read nor add to the seat's own
//!   reasoning ([`GameDecisionRequest::thread_game`]).
//! - **Thread errors.** A thread is best effort: the API drops one after five
//!   idle minutes, among other reasons. On a thread error the seat's thread is
//!   dropped ([`LlmThreadMemory::record_reply`]) and the caller asks the same
//!   decision once more, which then goes out as a fresh create. An API
//!   that refuses threads outright, or loses the recovery thread at its first
//!   continue, is answered with self-contained requests for that endpoint for
//!   as long as this memory lasts.
//!
//! A thread belongs to one seat of one game. Nothing in it reaches another seat
//! or the shared game log. The owner of this memory clears it when a new game
//! is installed or the game is restored; the next game of a match replaces the
//! game in place without a clear, and the game number keeps its threads apart.

use std::collections::HashMap;

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
    /// `thread`." A seat whose recovery thread is lost at its first continue
    /// therefore sends self-contained requests from then on.
    Recovery,
    /// Continued at least once.
    Continued,
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
    /// The conversation whose thread the API lost (`thread_not_found`) and
    /// that no accepted reply has replaced yet. The seat's next create for
    /// that same conversation is a recovery ([`ThreadOrigin::Recovery`]); a
    /// create for any other conversation (another account, game or system
    /// prompt) is an ordinary one, with a first recovery of its own to come.
    lost: Option<Conversation>,
    /// An endpoint the seat sends self-contained requests to for as long as
    /// this memory lasts: the rest of the match, unless a restore (an undo or
    /// a resume) clears it first. Its API refused threads
    /// (`thread_unsupported_request`), or lost a recovery thread at its first
    /// continue. A fresh create would only be refused, or lost, again.
    refused: Option<ThreadEndpoint>,
}

/// The endpoint and account a thread lives on. A thread is stored by the
/// account that created it and keeps one model, so a different endpoint, key
/// or model is a different thread, and an account's refusal of threads says
/// nothing about another account's.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// Whether a pending request creates a thread or continues one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingShape {
    /// A create, and the origin the thread it creates will have.
    Create(ThreadOrigin),
    /// A continue of a thread of this origin.
    Continue(ThreadOrigin),
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
    /// is forgotten (an endpoint's refusal of threads is kept). Otherwise the
    /// request continues the seat's thread when it can and creates a new one
    /// when it cannot, and is remembered as pending until
    /// [`Self::record_reply`].
    ///
    /// `history` is the log handed to the prompt builder; its newest entry is
    /// what the seat will have seen once this decision is answered.
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
    ) -> LlmResult<DecisionHttpRequest> {
        let full = |request| DecisionHttpRequest {
            request,
            prompt_chars: decision.prompt.char_count(),
            thread_turn: None,
        };
        let game_number = match decision.thread_game() {
            Some(game_number) if endpoint.uses_message_threads() => game_number,
            _ => {
                // The seat's thread, and any recovery owed to it, is
                // forgotten; an endpoint's refusal of threads is not, because
                // it outlasts any one thread.
                if let Some(thread) = self.seats.get_mut(&seat) {
                    thread.open = None;
                    thread.pending = None;
                    thread.lost = None;
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
        let create_origin = if thread.lost.as_ref() == Some(&conversation) {
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
                next.shape = PendingShape::Continue(open.origin);
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
    ///   refuses threads outright (`thread_unsupported_request`), or loses a
    ///   recovery thread at its first continue (`thread_not_found`), the
    ///   endpoint is remembered and the seat's decisions to it are
    ///   self-contained from then on, because a fresh create would only be
    ///   refused, or lost, again. That second rule cannot tell a conversation
    ///   the API cannot store from a recovery thread that aged out before its
    ///   first continue; either way the seat then costs what it costs without
    ///   threads.
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
                    PendingShape::Continue(_) => ThreadOrigin::Continued,
                };
                // The seat has an answered turn again: no conversation is
                // waiting on a recovery any more.
                thread.lost = None;
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
                        }
                    });
            }
            Err(LlmError::ThreadUnavailable { code }) => {
                thread.open = None;
                match code {
                    ThreadErrorCode::UnsupportedRequest => {
                        thread.refused = Some(answered.conversation.endpoint);
                    }
                    ThreadErrorCode::NotFound
                        if answered.shape == PendingShape::Continue(ThreadOrigin::Recovery) =>
                    {
                        thread.refused = Some(answered.conversation.endpoint);
                    }
                    // Anthropic's replay rule is about a thread the API lost:
                    // the create that replaces it, in the same conversation,
                    // is a recovery.
                    ThreadErrorCode::NotFound => thread.lost = Some(answered.conversation),
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
    use engine::types::game_state::GameState;
    use engine::types::log::{LogCategory, LogImportance, LogPresentation, LogSegment};
    use engine::types::phase::Phase;
    use phase_ai::config::AiDifficulty;
    use serde_json::{json, Value};

    const SEAT: PlayerId = PlayerId(1);

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
        AiDecisionContract {
            semantic_owner: SEAT,
            authorized_actor: SEAT,
            state_revision: revision,
            candidates: [
                GameAction::PassPriority,
                GameAction::ChoosePlayDraw { play_first: true },
            ]
            .into_iter()
            .map(|action| CandidateAction {
                action,
                metadata: ActionMetadata::for_actor(Some(SEAT), TacticalClass::Utility),
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
        memory
            .seats
            .get(&SEAT)
            .and_then(|thread| thread.pending.as_ref())
            .map(|pending| pending.turn)
    }

    /// The engine accepted `body`, a 2xx reply to the seat's pending request.
    fn accept_reply(memory: &mut LlmThreadMemory, provider: LlmProvider, body: &str) {
        let turn = pending_turn(memory);
        let reply = LlmReply {
            provider,
            status: 200,
            body,
        };
        memory.record_reply(SEAT, turn, reply, Ok(()));
    }

    /// The API refused to continue the thread the seat's pending request was
    /// sent on.
    fn lose_thread(memory: &mut LlmThreadMemory, code: ThreadErrorCode) {
        let turn = pending_turn(memory);
        let reply = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 404,
            body: "{}",
        };
        let error = LlmError::ThreadUnavailable { code };
        memory.record_reply(SEAT, turn, reply, Err(&error));
    }

    /// Build `decision` for the seat and accept `id` as its answer.
    fn answer(
        memory: &mut LlmThreadMemory,
        config: &LlmEndpointConfig,
        decision: &GameDecisionRequest,
        history: &[GameLogEntry],
        id: &str,
    ) -> HttpRequestSpec {
        let spec = memory
            .decision_request(SEAT, config, decision, history)
            .unwrap()
            .request;
        accept_reply(memory, LlmProvider::Anthropic, &reply(id, 900));
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
            .decision_request(SEAT, &threaded(), &first, &log(4))
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
            .decision_request(SEAT, &threaded(), &second, &log(7))
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
            .decision_request(SEAT, &threaded(), &first, &log(4))
            .unwrap();
        assert_eq!(create.prompt_chars, first.prompt.char_count());
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_1", 900));

        let second = decision(2, AiDifficulty::Medium);
        let continuation = memory
            .decision_request(SEAT, &threaded(), &second, &log(6))
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
            .decision_request(SEAT, &threaded(), &unanswered, &log(6))
            .unwrap();
        // A reply to some other decision does not advance it either.
        let elsewhere = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 200,
            body: &reply("msg_x", 900),
        };
        memory.record_reply(SEAT, Some(ThreadTurnId(u32::MAX)), elsewhere, Ok(()));

        let third = decision(3, AiDifficulty::Medium);
        let spec = memory
            .decision_request(SEAT, &threaded(), &third, &log(8))
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
            .decision_request(SEAT, &threaded(), &second, &log(6))
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
        memory.record_reply(SEAT, turn, overloaded, Err(&error));

        let retry = memory
            .decision_request(SEAT, &threaded(), &second, &log(6))
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
            .decision_request(SEAT, &threaded(), &second, &log(6))
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
        memory.record_reply(SEAT, turn, stored, Err(&error));
        assert!(!memory.has_open_thread(SEAT));

        let next = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(7),
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
                .decision_request(SEAT, &threaded(), &second, &log(6))
                .unwrap()
                .request;
            assert_eq!(continued_from(&lost), Some("msg_1".to_string()));

            lose_thread(&mut memory, code);
            assert!(!memory.has_open_thread(SEAT), "{code:?}");

            let retry = memory
                .decision_request(SEAT, &threaded(), &second, &log(6))
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
            .decision_request(SEAT, &threaded(), &first, &log(2))
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::UnsupportedRequest);

        let today = build_chat_request(&threaded(), &first.prompt).unwrap();
        let retry = memory
            .decision_request(SEAT, &threaded(), &first, &log(2))
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
                .decision_request(SEAT, &threaded(), &first, &log(2))
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
    /// remainder Anthropic reports as `input_tokens`.
    #[test]
    fn the_size_budget_counts_cache_reads_and_writes() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2))
            .unwrap();
        let cached = json!({
            "id": "msg_1",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{\"choice\": 0}" }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_read_input_tokens": THREAD_CONTEXT_BUDGET_TOKENS,
                "cache_creation_input_tokens": 500,
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
            )
            .unwrap()
            .request;
        assert!(is_create(&next));
    }

    #[test]
    fn a_reply_that_reports_no_usage_is_not_continued() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2))
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
                .decision_request(SEAT, &config, &decision(2, difficulty), &log(3))
                .unwrap()
                .request;
            assert!(is_create(&next), "a new {change} must start a new thread");
        }
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
                    .decision_request(SEAT, &threaded(), &decision, &log(4))
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

    /// When more was logged since the last answer than the seat's window
    /// shows, a continue would drop the oldest of it; the full request, which
    /// summarizes earlier turns, goes out as a new thread instead.
    #[test]
    fn a_continue_that_cannot_carry_everything_since_the_last_answer_is_a_create() {
        let window =
            crate::prompt::history_budget(AiDifficulty::Medium).current_cycle_entries as u32;
        let after = |unseen: u32| {
            let mut memory = LlmThreadMemory::new();
            answer(
                &mut memory,
                &threaded(),
                &decision(1, AiDifficulty::Medium),
                &log(4),
                "msg_1",
            );
            memory
                .decision_request(
                    SEAT,
                    &threaded(),
                    &decision(2, AiDifficulty::Medium),
                    &log(4 + unseen),
                )
                .unwrap()
                .request
        };
        assert_eq!(continued_from(&after(window)), Some("msg_1".to_string()));
        assert!(is_create(&after(window + 1)));
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
            )
            .unwrap()
            .request;
        assert!(is_create(&next_game));
    }

    /// The client's log runs on across the games of a match, so the next
    /// game's first decision is handed the last game's entries too. Its new
    /// thread is created with the game in progress only.
    #[test]
    fn the_next_games_thread_is_created_with_that_games_history_only() {
        let game_one = log(4);
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
            .decision_request(SEAT, &threaded(), &game_two, &history)
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
            .decision_request(SEAT, &threaded(), &play_draw, &log(5))
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
            .decision_request(SEAT, &threaded(), &first, &log(2))
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
            )
            .unwrap();

        let next_game = decision_in_game(3, AiDifficulty::Medium, 2);
        let sent = memory
            .decision_request(SEAT, &threaded(), &next_game, &log(4))
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
            .decision_request(SEAT, &new_key, &decision(2, AiDifficulty::Medium), &log(3))
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
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::UnsupportedRequest);
        let after_refusal = memory
            .decision_request(SEAT, &new_key, &decision(2, AiDifficulty::Medium), &log(3))
            .unwrap()
            .request;
        assert!(is_create(&after_refusal));
    }

    /// The recovery contract: when the first continue of a thread created to
    /// recover a lost one is lost too, the seat goes self-contained instead
    /// of paying a lost continue and a create on every decision.
    #[test]
    fn a_recovery_thread_lost_at_its_first_continue_makes_the_seat_self_contained() {
        let mut memory = LlmThreadMemory::new();
        answer(
            &mut memory,
            &threaded(),
            &decision(1, AiDifficulty::Medium),
            &log(2),
            "msg_1",
        );
        // A thread lost after ordinary use is recovered with a create.
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        let recovery = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
            )
            .unwrap()
            .request;
        assert!(is_create(&recovery));
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));

        // Its first continue is lost as well: the API cannot keep this
        // conversation, and every later decision is today's request.
        let first_continue = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
            )
            .unwrap()
            .request;
        assert_eq!(continued_from(&first_continue), Some("msg_2".to_string()));
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        for revision in 3..=5 {
            let decision = decision(revision, AiDifficulty::Medium);
            let sent = memory
                .decision_request(SEAT, &threaded(), &decision, &log(revision as u32 + 2))
                .unwrap()
                .request;
            assert_eq!(
                sent,
                build_chat_request(&threaded(), &decision.prompt).unwrap()
            );
        }
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
                .decision_request(SEAT, &threaded(), &game_one(2), &log(3))
                .unwrap();
            lose_thread(memory, ThreadErrorCode::NotFound);
            let recovery = memory
                .decision_request(SEAT, &threaded(), &game_one(2), &log(3))
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
            memory.record_reply(SEAT, pending_turn(memory), unavailable, Err(&error));
        };

        for (config, next) in [(&other_account, game_one(3)), (&threaded(), game_two(3))] {
            let mut memory = LlmThreadMemory::new();
            lose_and_fail_the_recovery(&mut memory);
            answer(&mut memory, config, &next, &log(4), "msg_2");
            memory
                .decision_request(SEAT, config, &next, &log(5))
                .unwrap();
            lose_thread(&mut memory, ThreadErrorCode::NotFound);
            let recovered = memory
                .decision_request(SEAT, config, &next, &log(5))
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
            .decision_request(SEAT, &threaded(), &game_one(3), &log(4))
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        let after = memory
            .decision_request(SEAT, &threaded(), &game_one(3), &log(4))
            .unwrap()
            .request;
        assert_eq!(
            after,
            build_chat_request(&threaded(), &game_one(3).prompt).unwrap()
        );
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
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(2, AiDifficulty::Medium),
                &log(3),
            )
            .unwrap();
        accept_reply(&mut memory, LlmProvider::Anthropic, &reply("msg_2", 900));
        // The recovery thread's first continue succeeds...
        answer(
            &mut memory,
            &threaded(),
            &decision(3, AiDifficulty::Medium),
            &log(4),
            "msg_3",
        );
        // ...so a later loss is an ordinary one.
        memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
            )
            .unwrap();
        lose_thread(&mut memory, ThreadErrorCode::NotFound);
        let recovered = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(4, AiDifficulty::Medium),
                &log(5),
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
            .decision_request(SEAT, &threaded(), &second, &log(3))
            .unwrap()
            .thread_turn;
        let live = memory
            .decision_request(SEAT, &threaded(), &second, &log(3))
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
        memory.record_reply(SEAT, cancelled, lost, Err(&error));
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
            .decision_request(SEAT, &threaded(), &controlled, &log(3))
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
                    .decision_request(SEAT, &config, &decision, &log(revision as u32 * 2))
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
            .decision_request(SEAT, &off, &decision(2, AiDifficulty::Medium), &log(3))
            .unwrap();
        assert!(!memory.has_open_thread(SEAT));
        let back_on = memory
            .decision_request(
                SEAT,
                &threaded(),
                &decision(3, AiDifficulty::Medium),
                &log(4),
            )
            .unwrap()
            .request;
        assert!(is_create(&back_on));
    }

    #[test]
    fn a_refused_reply_is_never_continued() {
        let mut memory = LlmThreadMemory::new();
        let first = decision(1, AiDifficulty::Medium);
        memory
            .decision_request(SEAT, &threaded(), &first, &log(2))
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
            )
            .unwrap()
            .request;
        assert!(is_create(&spec));
        memory.clear();
        assert!(!memory.has_open_thread(SEAT));
    }
}
