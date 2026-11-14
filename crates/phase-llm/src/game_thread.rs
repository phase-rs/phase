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
//! - **Commit on accept only.** A request is remembered as pending, keyed by
//!   its decision fingerprint, and becomes the thread's new turn only when its
//!   reply is accepted ([`LlmThreadMemory::record_reply`]). A failed or
//!   cancelled decision leaves the thread at its last answered turn, with one
//!   exception: a 2xx reply the engine refuses was still stored by the API,
//!   and a reply id can be continued only once, so the thread cannot go on
//!   from its last answered turn and the seat's next decision starts a new one.
//! - **Start over.** A new thread is created, with the full request, when the
//!   match moves on to its next game, when the endpoint or model changes, when the system prompt changes (difficulty,
//!   format, or the table itself), when the last reply reported more context
//!   than [`THREAD_CONTEXT_BUDGET_TOKENS`] or no usage at all, when the log
//!   was rewound under the thread, and when more was logged since the seat's
//!   last answer than its history window shows (the full request summarizes
//!   older turns, which a continue cannot).
//! - **Difficulty.** The two lowest difficulties never use a thread: they
//!   deliberately reason from a near-term window, which a thread would undo
//!   ([`GameDecisionRequest::allows_message_thread`]).
//! - **Thread errors.** A thread is best effort: the API drops one after five
//!   idle minutes, among other reasons. On a thread error the seat's thread is
//!   dropped ([`LlmThreadMemory::record_reply`]) and the caller asks the same
//!   decision once more, which then goes out as a fresh create. An API
//!   that refuses threads outright is answered with self-contained requests
//!   for that endpoint from then on.
//!
//! A thread belongs to one seat of one game. Nothing in it reaches another seat
//! or the shared game log, and the owner of this memory clears it whenever the
//! game it describes is replaced.

use std::collections::HashMap;

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
}

/// Every LLM seat's message thread for the game in progress.
///
/// Session state held by the transport beside
/// [`LlmDecisionMemory`](crate::game_decision::LlmDecisionMemory), and cleared
/// with it whenever the game is replaced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LlmThreadMemory {
    seats: HashMap<PlayerId, SeatThread>,
}

/// One seat's thread state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SeatThread {
    /// The thread the seat's next decision may continue.
    open: Option<OpenThread>,
    /// The threaded request in flight, committed only when its reply is
    /// accepted.
    pending: Option<PendingTurn>,
    /// An endpoint whose API refused threads (`thread_unsupported_request`).
    /// The seat's decisions to it are self-contained for the rest of the game,
    /// because a fresh create would only be refused again.
    refused: Option<ThreadEndpoint>,
}

/// The endpoint a thread lives on. A thread is stored by the account that
/// created it and keeps one model, so a different endpoint or model is a
/// different thread.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreadEndpoint {
    base_url: String,
    model: String,
}

impl ThreadEndpoint {
    fn of(config: &LlmEndpointConfig) -> LlmResult<ThreadEndpoint> {
        Ok(ThreadEndpoint {
            base_url: config.resolved_base_url()?,
            model: config.model.trim().to_string(),
        })
    }
}

/// A thread the API holds for a seat, as of the seat's last accepted reply.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenThread {
    /// The game of the match the thread belongs to. A thread never continues
    /// into the next game of a match, which replaces the game in place.
    game_number: u8,
    endpoint: ThreadEndpoint,
    /// Fingerprint of the system prompt the thread was created with. A
    /// continue cannot send a new one, so a change starts a new thread.
    system: String,
    /// The id of the seat's last accepted reply, which the next decision
    /// continues from.
    message_id: String,
    /// The newest log entry the thread has been shown, or `None` when the log
    /// was empty.
    seen_through: Option<u32>,
    /// How much context the last reply reported ([`context_tokens`]), or
    /// `None` when it reported no usage.
    context_tokens: Option<u64>,
}

/// A threaded request awaiting its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingTurn {
    /// The decision the request was built for. A reply is committed only for
    /// this decision.
    fingerprint: String,
    game_number: u8,
    endpoint: ThreadEndpoint,
    system: String,
    /// The newest log entry the request showed the seat.
    seen_through: Option<u32>,
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
        self.game_number == next.game_number
            && self.endpoint == next.endpoint
            && self.system == next.system
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
    /// difficulty does not allow one get exactly [`build_chat_request`]'s
    /// request, and any thread the seat held is forgotten. Otherwise the request continues the seat's thread when it
    /// can and creates a new one when it cannot, and is remembered as pending
    /// until [`Self::record_reply`].
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
        };
        if !endpoint.uses_message_threads() || !decision.allows_message_thread() {
            self.seats.remove(&seat);
            return build_chat_request(endpoint, &decision.prompt).map(full);
        }
        let target = ThreadEndpoint::of(endpoint)?;
        let thread = self.seats.entry(seat).or_default();
        if thread.refused.as_ref() == Some(&target) {
            thread.pending = None;
            return build_chat_request(endpoint, &decision.prompt).map(full);
        }

        let next = PendingTurn {
            fingerprint: decision.fingerprint.clone(),
            game_number: decision.game_number(),
            endpoint: target,
            system: fingerprint_of([decision.prompt.system.as_str()]),
            seen_through: history.iter().map(|entry| entry.seq).max(),
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
            Some((open, update)) => DecisionHttpRequest {
                request: build_thread_request(
                    endpoint,
                    ThreadTurn::Continue {
                        previous_message_id: &open.message_id,
                        user: &update,
                    },
                )?,
                prompt_chars: update.chars().count(),
            },
            None => full(build_thread_request(
                endpoint,
                ThreadTurn::Create {
                    prompt: &decision.prompt,
                },
            )?),
        };
        thread.pending = Some(next);
        Ok(request)
    }

    /// Record what became of the reply to `seat`'s decision `fingerprint`.
    ///
    /// `outcome` is the engine's verdict on the reply:
    ///
    /// - **Accepted.** The reply becomes the turn the seat's next decision
    ///   continues from. One that cannot be continued (a refusal, or no `id`)
    ///   leaves the seat with no thread, so its next decision starts a new one.
    /// - **A thread error.** The seat's thread is dropped, so the same decision
    ///   asked again goes out as a fresh create. When the API refuses threads
    ///   outright (`thread_unsupported_request`), the endpoint is remembered and
    ///   the seat's decisions to it are self-contained from then on, because a
    ///   fresh create would only be refused again.
    /// - **Any other refusal of a 2xx reply.** The API stored that turn after
    ///   the seat's last answered one, and an id can be continued only once, so
    ///   the seat's next decision starts a new thread instead of sending a
    ///   continue the API would refuse.
    /// - **Any other failure.** A non-2xx reply stored nothing, so the thread
    ///   stays at the seat's last answered turn.
    ///
    /// Nothing happens for a reply to a decision that was not sent through a
    /// thread, or to a decision other than the one pending.
    pub fn record_reply(
        &mut self,
        seat: PlayerId,
        fingerprint: &str,
        reply: LlmReply<'_>,
        outcome: Result<(), &LlmError>,
    ) {
        let Some(thread) = self.seats.get_mut(&seat) else {
            return;
        };
        let Some(turn) = thread
            .pending
            .take_if(|pending| pending.fingerprint == fingerprint)
        else {
            return;
        };
        match outcome {
            Ok(()) => {
                thread.open =
                    continuable_message_id(reply.provider, reply.body).map(|message_id| {
                        OpenThread {
                            game_number: turn.game_number,
                            endpoint: turn.endpoint,
                            system: turn.system,
                            message_id,
                            seen_through: turn.seen_through,
                            context_tokens: token_usage(reply.provider, reply.body)
                                .as_ref()
                                .and_then(context_tokens),
                        }
                    });
            }
            Err(LlmError::ThreadUnavailable { code }) => {
                thread.open = None;
                if *code == ThreadErrorCode::UnsupportedRequest {
                    thread.refused = Some(turn.endpoint);
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
/// count cannot be measured, and its thread is not continued.
fn context_tokens(usage: &TokenUsage) -> Option<u64> {
    Some(
        usage.input_tokens?
            + usage.cached_input_tokens.unwrap_or(0)
            + usage.cache_creation_input_tokens.unwrap_or(0)
            + usage.output_tokens?,
    )
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
        let contract = AiDecisionContract {
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
        };
        let mut state = GameState::default();
        state.game_number = game_number;
        build_game_decision_prompt(&state, &contract, difficulty, None, &[]).unwrap()
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

    /// The engine accepted `body`, a 2xx reply, as the answer to `fingerprint`.
    fn accept_reply(
        memory: &mut LlmThreadMemory,
        fingerprint: &str,
        provider: LlmProvider,
        body: &str,
    ) {
        let reply = LlmReply {
            provider,
            status: 200,
            body,
        };
        memory.record_reply(SEAT, fingerprint, reply, Ok(()));
    }

    /// The API refused to continue the thread `fingerprint` was sent on.
    fn lose_thread(memory: &mut LlmThreadMemory, fingerprint: &str, code: ThreadErrorCode) {
        let reply = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 404,
            body: "{}",
        };
        let error = LlmError::ThreadUnavailable { code };
        memory.record_reply(SEAT, fingerprint, reply, Err(&error));
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
        accept_reply(
            memory,
            &decision.fingerprint,
            LlmProvider::Anthropic,
            &reply(id, 900),
        );
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
        accept_reply(
            &mut memory,
            &first.fingerprint,
            LlmProvider::Anthropic,
            &reply("msg_1", 900),
        );

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
        accept_reply(
            &mut memory,
            "not-this-decision",
            LlmProvider::Anthropic,
            &reply("msg_x", 900),
        );

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
        memory.record_reply(SEAT, &second.fingerprint, overloaded, Err(&error));

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
        memory.record_reply(SEAT, &second.fingerprint, stored, Err(&error));
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

            lose_thread(&mut memory, &second.fingerprint, code);
            assert!(!memory.has_open_thread(SEAT), "{code:?}");

            let retry = memory
                .decision_request(SEAT, &threaded(), &second, &log(6))
                .unwrap()
                .request;
            assert!(is_create(&retry), "{code:?}");
            assert_eq!(body(&retry)["system"], json!(second.prompt.system));
            assert_eq!(user_message(&retry), second.prompt.user, "{code:?}");

            // The recovered decision's reply starts the new thread.
            accept_reply(
                &mut memory,
                &second.fingerprint,
                LlmProvider::Anthropic,
                &reply("msg_2", 900),
            );
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
        lose_thread(
            &mut memory,
            &first.fingerprint,
            ThreadErrorCode::UnsupportedRequest,
        );

        let today = build_chat_request(&threaded(), &first.prompt).unwrap();
        let retry = memory
            .decision_request(SEAT, &threaded(), &first, &log(2))
            .unwrap()
            .request;
        assert_eq!(retry, today);
        accept_reply(
            &mut memory,
            &first.fingerprint,
            LlmProvider::Anthropic,
            &reply("msg_1", 900),
        );
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
                &first.fingerprint,
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
        accept_reply(
            &mut memory,
            &first.fingerprint,
            LlmProvider::Anthropic,
            &cached,
        );
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
        accept_reply(
            &mut memory,
            &first.fingerprint,
            LlmProvider::Anthropic,
            &unmeasured,
        );
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
                accept_reply(
                    &mut memory,
                    &decision.fingerprint,
                    LlmProvider::Anthropic,
                    &reply("msg", 900),
                );
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
                accept_reply(
                    &mut memory,
                    &decision.fingerprint,
                    config.provider,
                    &reply("msg", 900),
                );
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
        accept_reply(
            &mut memory,
            &first.fingerprint,
            LlmProvider::Anthropic,
            &refused,
        );
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
