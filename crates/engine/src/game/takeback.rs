//! "Request takeback" — multiplayer-safe undo (GH #1507).
//!
//! A player may request to roll the authoritative state back to the snapshot
//! taken just before their own most recent action, or to a turn boundary where
//! the host offers one. The rollback only takes effect once every voter has
//! approved it; any single decline ends the request and leaves the state
//! untouched.
//!
//! This is intentionally a session/room-level concern, not an engine rule —
//! there is no Comprehensive Rules concept of "undo." The engine is never
//! told why it was handed an older `GameState`; it just continues from
//! whatever state it's given, exactly as it does on reconnect/restore.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::types::action_rejection::{ActionRejection, ActionRejectionCode};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::player::PlayerId;

/// How many prior authoritative snapshots a ledger retains for takeback
/// purposes. Bounded so a long game session can't accumulate unbounded
/// memory — a takeback can only reach back to the most recent action, so
/// anything beyond a handful of entries is never reachable anyway.
pub const MAX_TAKEBACK_HISTORY: usize = 12;

/// How many turn-boundary snapshots a ledger retains. Deliberately the same
/// depth as the browser-local analogue (`client/src/constants/game.ts`'s
/// `MAX_UNDO_HISTORY = 5`), so desktop solo-vs-AI offers the same reach the
/// browser sandbox already does. Small because these are whole `GameState`
/// clones and — unlike `MAX_TAKEBACK_HISTORY` — only ever populated by a
/// ledger built with [`TakebackLedger::with_turn_rewind`].
pub const MAX_TURN_REWIND_HISTORY: usize = 5;

/// How far back a rollback request reaches.
///
/// Parameterizes the one rollback machine rather than growing a sibling wire
/// message per granularity: the approval state machine, the pending-request
/// interlock, `rekey_after_trusted_restore`, and the reconnect replay are all
/// shared. `LastAction` is the pre-existing GH #1507 behaviour and is the
/// `Default` so an absent wire payload from a pre-rewind client normalizes to
/// exactly what that client meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RewindTarget {
    /// Roll back to the snapshot taken immediately before the requester's own
    /// most recent state-mutating action.
    #[default]
    LastAction,
    /// Roll back to the start of the numbered turn *of the current game*.
    /// Turn numbers are only unique within a game (see
    /// [`TakebackLedger::observe_transition`]), which is why the rings are
    /// cleared at a match's game boundary rather than keyed by game number.
    TurnStart { turn_number: u32 },
}

/// One turn boundary a client may ask to roll back to, as published by the
/// server. Both fields are read from the stored snapshot's *own* state, never
/// recomputed at publication time, so the label a player clicks is exactly the
/// state they get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindOption {
    pub turn_number: u32,
    pub active_player: PlayerId,
}

/// Why a takeback vote ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakebackResolution {
    /// Every voter approved; the target state has been installed.
    Approved,
    /// A voter declined.
    Declined,
    /// The requester withdrew its own request: by cancelling, or by a concession
    /// that removed no one.
    Cancelled,
    /// A seat actually left the game while the vote was open.
    Withdrawn,
}

/// Outcome of requesting or responding to a takeback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakebackOutcome {
    /// Still waiting on one or more voters.
    Pending,
    /// The vote ended; on `Approved` the state has already been rolled back.
    Resolved(TakebackResolution),
}

/// An open vote as hosts announce it: who asked, and who is asked to approve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakebackVote {
    pub requester: PlayerId,
    /// The seats asked to approve, in seat order; never the requester.
    pub voters: Vec<PlayerId>,
}

/// A takeback request awaiting approval.
#[derive(Debug, Clone)]
struct PendingTakeback {
    /// Implicitly counted as having approved their own request.
    requested_by: PlayerId,
    target_state: GameState,
    /// The ledger clock the target was recorded at.
    target_at: u64,
    approvals: HashSet<PlayerId>,
    /// How much of `history` remains a genuine ancestor chain of
    /// `target_state` once the rollback lands. Sound to compute at request
    /// time because no point is recorded while a vote is open.
    history_truncate_len: usize,
}

/// What a host captures before applying a human action, for
/// [`TakebackLedger::record_human_action`].
#[derive(Debug, Clone)]
pub struct TakebackCapture {
    snapshot: Option<GameState>,
    eliminated_before: Vec<PlayerId>,
}

#[derive(Debug, Clone)]
struct TakebackPoint {
    actor: PlayerId,
    at: u64,
    state: GameState,
}

#[derive(Debug, Clone)]
struct TurnPoint {
    at: u64,
    state: GameState,
}

/// The takeback authority for one game session: rollback history, the open
/// vote, its voters and its resolution. In-memory only; never serialized.
#[derive(Debug, Clone)]
pub struct TakebackLedger {
    history: VecDeque<TakebackPoint>,
    /// `None` when this host offers no turn rewind.
    turn_history: Option<VecDeque<TurnPoint>>,
    pending: Option<PendingTakeback>,
    /// The `game_number` the rings were last observed at.
    game_number: u8,
    /// Counts observed transitions.
    clock: u64,
    /// Each seat's earliest no-op concession, at the clock it was observed.
    noop_concessions: BTreeMap<PlayerId, u64>,
}

impl TakebackLedger {
    /// A ledger that offers no turn rewind.
    pub fn new(game_number: u8) -> Self {
        Self {
            history: VecDeque::new(),
            turn_history: None,
            pending: None,
            game_number,
            clock: 0,
            noop_concessions: BTreeMap::new(),
        }
    }

    /// A ledger that also records turn boundaries for `RewindTarget::TurnStart`.
    pub fn with_turn_rewind(game_number: u8) -> Self {
        Self {
            turn_history: Some(VecDeque::new()),
            ..Self::new(game_number)
        }
    }

    /// Captures what [`Self::record_human_action`] needs from the state before
    /// a human action is applied. `action` is `None` when the action is not
    /// yet known.
    pub fn capture(state: &GameState, action: Option<&GameAction>) -> TakebackCapture {
        let records = action.is_none_or(|action| !action.is_actor_scoped_preference());
        TakebackCapture {
            snapshot: records.then(|| state.clone()),
            eliminated_before: state.eliminated_players.clone(),
        }
    }

    /// The bookkeeping after a human action was applied to `state`: the
    /// takeback point, the concession rules, then the transition itself.
    /// Returns how an open vote ended, if this action ended it.
    pub fn record_human_action(
        &mut self,
        state: &mut GameState,
        actor: PlayerId,
        capture: TakebackCapture,
        action: &GameAction,
        result: &ActionResult,
        ai_seats: &[PlayerId],
    ) -> Option<TakebackResolution> {
        if let Some(snapshot) = capture.snapshot {
            // A reversed activation (CR 602.2b + CR 601.2h) restored the state
            // from before the action: it is not a takeback point.
            if !action.is_actor_scoped_preference() && result.disposition.is_applied() {
                self.record_point(actor, snapshot);
            }
        }

        let mut resolution = None;
        let mut installed = false;
        if matches!(action, GameAction::Concede { .. }) {
            let departed = state
                .eliminated_players
                .iter()
                .any(|player| !capture.eliminated_before.contains(player));
            if departed {
                resolution = self.pending.take().map(|_| TakebackResolution::Withdrawn);
                self.retire();
            } else {
                self.noop_concessions.entry(actor).or_insert(self.clock);
                match &self.pending {
                    Some(pending) if pending.requested_by == actor => {
                        self.pending = None;
                        resolution = Some(TakebackResolution::Cancelled);
                    }
                    Some(_) => {
                        if let Some(TakebackOutcome::Resolved(resolved)) =
                            self.try_resolve(state, ai_seats)
                        {
                            installed = true;
                            resolution = Some(resolved);
                        }
                    }
                    None => {}
                }
            }
        }

        if installed {
            // The installed target is not this transition's post-state.
            self.clock += 1;
        } else {
            self.observe_transition(&result.events, state);
        }
        resolution
    }

    /// An accepted match forfeit is always an actual departure: withdraw any
    /// open vote and retire both rings.
    pub fn observe_match_forfeit(&mut self) -> Option<TakebackResolution> {
        let resolution = self.pending.take().map(|_| TakebackResolution::Withdrawn);
        self.retire();
        resolution
    }

    /// The single per-transition rewind bookkeeping authority. Every
    /// authoritative state transition calls this with **its own** event batch
    /// and post-state.
    ///
    /// Retires both rings at a match's game boundary, then records the turn
    /// boundary this transition came to rest in (if any).
    ///
    /// **Why the game-boundary clear.** `turn_number` is *assigned* 1 at each
    /// game's start rather than carried across a Bo3 match (CR 100.6a: a
    /// two-player match is a series of *games*), and the between-games
    /// `ChoosePlayDraw` action rebuilds the state through a human action with
    /// `TurnStarted { turn_number: 1 }` in its batch. Without this,
    /// `turn_number` is not monotone across the session, and a
    /// `TurnStart { n }` request would resolve by first match to a *finished
    /// game's* turn n and install that board into the live game. Both rings
    /// are retired, not just the turn ring: a rollback into a previous game of
    /// the match is not a coherent offer at any granularity, and clearing both
    /// is what restores the within-game monotonicity that `partition_point`,
    /// `retain`, and the turn capture's `debug_assert!` all rely on.
    pub fn observe_transition(&mut self, events: &[GameEvent], post_state: &GameState) {
        self.clock += 1;
        if post_state.game_number != self.game_number {
            self.game_number = post_state.game_number;
            // No game boundary is crossed while a vote is open: `record_human_action` and
            // `observe_match_forfeit` withdraw the vote on the departure that ends a game.
            debug_assert!(self.pending.is_none());
            self.retire();
        }
        self.record_turn_point(events, post_state);
    }

    /// Records `state` as a turn-boundary rewind point iff the transition that
    /// produced it *came to rest in* the turn its own events announced.
    ///
    /// The `started != state.turn_number` guard is an honesty guard, not an
    /// optimization: a batch that both started *and* ended a turn leaves no state
    /// that could be truthfully labelled with either turn, so that boundary is not
    /// offered rather than mislabelled.
    ///
    /// **Uniqueness.** No two retained entries share a `turn_number`, *within a
    /// game*: `state.turn_number` is assigned 1 at a game start
    /// (`engine::game::engine`'s two `start_game_*` helpers) and thereafter only
    /// incremented, unconditionally and once per turn start
    /// (`engine/src/game/turns.rs`, CR 500.7 — including extra turns; a *skipped*
    /// turn increments without emitting `TurnStarted`). `TurnStarted` has three
    /// production emission sites — `turns.rs` plus those two `start_game_*`
    /// helpers, which both emit `TurnStarted { turn_number: 1 }` — so uniqueness is
    /// a within-game property only. Crossing into another game of a Bo3 match is
    /// handled by [`Self::observe_transition`], which clears both rings, and
    /// the `debug_assert!` below is what fails loudly if that ever stops happening.
    fn record_turn_point(&mut self, events: &[GameEvent], state: &GameState) {
        let Some(ring) = self.turn_history.as_mut() else {
            return;
        };
        let Some(started) = events.iter().rev().find_map(|event| match event {
            GameEvent::TurnStarted { turn_number, .. } => Some(*turn_number),
            _ => None,
        }) else {
            return;
        };
        if started != state.turn_number {
            return;
        }
        debug_assert!(
            ring.back()
                .is_none_or(|point| point.state.turn_number < state.turn_number),
            "turn rewind ring must stay strictly increasing within a game"
        );
        if ring.len() >= MAX_TURN_REWIND_HISTORY {
            ring.pop_front();
        }
        ring.push_back(TurnPoint {
            at: self.clock,
            state: state.clone(),
        });
    }

    /// Records `pre_state` as a takeback point tagged with the player about to
    /// act, unless a vote is open. Caps retention at [`MAX_TAKEBACK_HISTORY`]
    /// (oldest dropped first).
    pub fn record_point(&mut self, actor: PlayerId, pre_state: GameState) {
        if self.pending.is_some() {
            return;
        }
        if self.history.len() >= MAX_TAKEBACK_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(TakebackPoint {
            actor,
            at: self.clock,
            state: pre_state,
        });
    }

    /// While a vote is open the authoritative state must not move out from
    /// under it; only a concession is admitted.
    pub fn admit_action(&self, action: &GameAction) -> Result<(), ActionRejection> {
        if matches!(action, GameAction::Concede { .. }) {
            return Ok(());
        }
        self.refuse_while_pending()
    }

    /// [`Self::admit_action`] without the concession exception.
    pub fn refuse_while_pending(&self) -> Result<(), ActionRejection> {
        if self.pending.is_some() {
            return Err(ActionRejection::new(ActionRejectionCode::ActionNotAllowed));
        }
        Ok(())
    }

    /// `player` requests rolling the game back to the state just before *their
    /// own* most recent action — not simply the most recent action by anyone.
    /// Other players may have acted since; rolling back to before the
    /// requester's action necessarily discards those later actions too, but it
    /// must never target a snapshot that precedes a different player's action
    /// while leaving the requester's own action untouched. Auto-resolves to
    /// `Approved` immediately when there is nobody to ask.
    ///
    /// `target` selects the granularity. `RewindTarget::TurnStart` reaches a
    /// boundary the action-granular ring structurally cannot: every wire
    /// `PassPriority` burns a `MAX_TAKEBACK_HISTORY` slot, so a whole turn
    /// (CR 500.1) never fits inside twelve entries.
    pub fn request(
        &mut self,
        state: &mut GameState,
        player: PlayerId,
        target: RewindTarget,
        ai_seats: &[PlayerId],
    ) -> Result<TakebackOutcome, String> {
        // A contest is host-driven and real-time: ordinary actions are frozen while a
        // takeback vote is open, so a vote opened mid-contest would leave the contest
        // mutating under it. Refuse before any vote is installed.
        if matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }) {
            return Err("A takeback cannot be requested while a contest is running".to_string());
        }
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return Err("A takeback cannot be requested after the game has ended".to_string());
        }
        if self.pending.is_some() {
            return Err("A takeback request is already pending for this game".to_string());
        }
        if !is_human_seat(state, player, ai_seats) {
            return Err("Only human players may request a takeback".to_string());
        }
        let (target_state, target_at, history_truncate_len) = match target {
            RewindTarget::LastAction => {
                let index = self
                    .history
                    .iter()
                    .rposition(|point| point.actor == player)
                    .ok_or_else(|| {
                        "There is no previous action of yours to take back".to_string()
                    })?;
                let point = &self.history[index];
                (point.state.clone(), point.at, index)
            }
            RewindTarget::TurnStart { turn_number } => {
                // Refuse explicitly rather than letting an always-empty ring
                // produce a "no longer available" miss. This is a system
                // boundary, and a refusal that depends on a collection being
                // empty is untestably vacuous.
                let Some(ring) = &self.turn_history else {
                    return Err("Turn rewind is not available in this game".to_string());
                };
                let point = ring
                    .iter()
                    .find(|point| point.state.turn_number == turn_number)
                    .ok_or_else(|| "That turn is no longer available to rewind to".to_string())?;
                // An exact prefix predicate: within a game the action ring is
                // chronological and `turn_number` is monotone, so every entry
                // recorded before turn `n` began is an ancestor of turn `n`'s
                // opening state, and every entry from turn `n` onwards is not.
                let history_truncate_len = self
                    .history
                    .partition_point(|entry| entry.state.turn_number < turn_number);
                (point.state.clone(), point.at, history_truncate_len)
            }
        };

        self.pending = Some(PendingTakeback {
            requested_by: player,
            target_state,
            target_at,
            approvals: HashSet::from([player]),
            history_truncate_len,
        });
        Ok(self
            .try_resolve(state, ai_seats)
            .unwrap_or(TakebackOutcome::Pending))
    }

    /// A voter approves or declines the open vote. A single decline ends it.
    pub fn respond(
        &mut self,
        state: &mut GameState,
        player: PlayerId,
        approve: bool,
        ai_seats: &[PlayerId],
    ) -> Result<TakebackOutcome, String> {
        let Some(pending) = self.pending.as_ref() else {
            return Err("There is no pending takeback request".to_string());
        };
        if !is_human_seat(state, player, ai_seats) {
            return Err("Only human players may respond to a takeback request".to_string());
        }
        if !self.voters(pending, state, ai_seats).contains(&player) {
            return Err("Only the players asked to approve this takeback may respond".to_string());
        }
        if !approve {
            self.pending = None;
            return Ok(TakebackOutcome::Resolved(TakebackResolution::Declined));
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.approvals.insert(player);
        }
        Ok(self
            .try_resolve(state, ai_seats)
            .unwrap_or(TakebackOutcome::Pending))
    }

    /// The original requester withdraws their own pending takeback request.
    pub fn cancel(&mut self, player: PlayerId) -> Result<(), String> {
        match &self.pending {
            Some(pending) if pending.requested_by == player => {
                self.pending = None;
                Ok(())
            }
            Some(_) => Err("Only the player who requested the takeback may cancel it".to_string()),
            None => Err("There is no pending takeback request".to_string()),
        }
    }

    /// The seats whose approval the open vote needs, besides the requester's
    /// implicit one: the other human seats in the game now, or, when there are
    /// none, the other human seats that were in the game at the target and
    /// have not conceded since it.
    fn voters(
        &self,
        pending: &PendingTakeback,
        state: &GameState,
        ai_seats: &[PlayerId],
    ) -> BTreeSet<PlayerId> {
        let candidates = state
            .players
            .iter()
            .map(|player| player.id)
            .filter(|seat| *seat != pending.requested_by && !ai_seats.contains(seat));
        let current: BTreeSet<PlayerId> = candidates
            .clone()
            .filter(|seat| !state.eliminated_players.contains(seat))
            .collect();
        if !current.is_empty() {
            return current;
        }
        candidates
            .filter(|seat| {
                !pending.target_state.eliminated_players.contains(seat)
                    && self
                        .noop_concessions
                        .get(seat)
                        .is_none_or(|at| *at < pending.target_at)
            })
            .collect()
    }

    /// Resolves the open vote if every voter has approved, installing the
    /// target into `state`. Returns `None` if still pending.
    fn try_resolve(
        &mut self,
        state: &mut GameState,
        ai_seats: &[PlayerId],
    ) -> Option<TakebackOutcome> {
        let pending = self.pending.as_ref()?;
        if !self
            .voters(pending, state, ai_seats)
            .iter()
            .all(|seat| pending.approvals.contains(seat))
        {
            return None;
        }
        let pending = self.pending.take().expect("checked above");
        *state = pending.target_state;
        crate::game::rekey_after_trusted_restore(state);
        // The rolled-back state is the new baseline. Snapshots *after* the
        // restored one belong to the branch the table just discarded, and
        // taking another takeback back through them would resurrect actions
        // the table just agreed to undo. Snapshots *before* it are still
        // genuine ancestors of it, and a takeback that targets one of those
        // goes strictly further back — discarding strictly more, resurrecting
        // nothing. `history_truncate_len`, computed at request time, is
        // exactly that ancestor prefix; truncating is what makes undo
        // repeatable rather than one-shot.
        self.history.truncate(pending.history_truncate_len);
        // A concession recorded at or after the target belongs to the branch the table just undid.
        self.noop_concessions
            .retain(|_, at| *at < pending.target_at);
        // `<=`, not `<`: a `TurnStart { n }` rewind targets the turn-n
        // snapshot itself, so keeping turn n selectable is what makes turn
        // rewind idempotent and repeatable rather than consuming its own
        // target.
        let restored_turn = state.turn_number;
        if let Some(ring) = self.turn_history.as_mut() {
            ring.retain(|point| point.state.turn_number <= restored_turn);
        }
        Some(TakebackOutcome::Resolved(TakebackResolution::Approved))
    }

    /// Clears both rings and the concession records.
    fn retire(&mut self) {
        self.history.clear();
        if let Some(ring) = self.turn_history.as_mut() {
            ring.clear();
        }
        self.noop_concessions.clear();
    }

    /// The turn boundaries this ledger currently offers, oldest first. Each
    /// option is labelled from its snapshot's *own* fields, so the label and
    /// the state a player gets cannot disagree.
    pub fn rewind_options(&self) -> Vec<RewindOption> {
        self.turn_history
            .iter()
            .flatten()
            .map(|point| RewindOption {
                turn_number: point.state.turn_number,
                active_player: point.state.active_player,
            })
            .collect()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The seats that have approved the open vote, requester included, in seat order.
    #[cfg(feature = "test-support")]
    pub fn approvals(&self) -> Option<Vec<PlayerId>> {
        self.pending.as_ref().map(|pending| {
            let mut approvals: Vec<_> = pending.approvals.iter().copied().collect();
            approvals.sort();
            approvals
        })
    }

    /// The open vote, with the voter set `respond` enforces.
    pub fn pending_vote(&self, state: &GameState, ai_seats: &[PlayerId]) -> Option<TakebackVote> {
        self.pending.as_ref().map(|pending| TakebackVote {
            requester: pending.requested_by,
            voters: self.voters(pending, state, ai_seats).into_iter().collect(),
        })
    }

    /// The takeback points, oldest first, as `(actor, pre-action state)`.
    pub fn history(
        &self,
    ) -> impl ExactSizeIterator<Item = (PlayerId, &GameState)> + DoubleEndedIterator {
        self.history.iter().map(|point| (point.actor, &point.state))
    }

    pub fn turn_history_len(&self) -> usize {
        self.turn_history.as_ref().map_or(0, VecDeque::len)
    }

    pub fn game_number(&self) -> u8 {
        self.game_number
    }
}

fn is_human_seat(state: &GameState, player: PlayerId, ai_seats: &[PlayerId]) -> bool {
    state.players.iter().any(|seat| seat.id == player) && !ai_seats.contains(&player)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply;
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::types::ability::TargetRef;
    use crate::types::game_state::CastPaymentMode;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::match_config::MatchType;
    use crate::types::phase::Phase;

    const P2: PlayerId = PlayerId(2);
    const P3: PlayerId = PlayerId(3);
    const P4: PlayerId = PlayerId(4);
    const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
    const EARTHQUAKE: &str =
        "Earthquake deals X damage to each creature without flying and each player.";

    fn add_bolt(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
        scenario
            .add_spell_to_hand_from_oracle(owner, "Lightning Bolt", true, LIGHTNING_BOLT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id()
    }

    fn add_earthquake(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
        scenario
            .add_spell_to_hand_from_oracle(owner, "Earthquake", false, EARTHQUAKE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 0,
            })
            .id()
    }

    fn cast_action(state: &GameState, spell: ObjectId) -> GameAction {
        GameAction::CastSpell {
            object_id: spell,
            card_id: state.objects[&spell].card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        }
    }

    /// The answer to the current cast prompt, or `None` once the spell is on
    /// the stack.
    fn cast_prompt_answer(
        state: &GameState,
        x: u32,
        target: &Option<TargetRef>,
    ) -> Option<GameAction> {
        match state.waiting_for {
            WaitingFor::ChooseXValue { .. } => Some(GameAction::ChooseX { value: x }),
            WaitingFor::TargetSelection { .. } => Some(GameAction::ChooseTarget {
                target: target.clone(),
            }),
            _ => None,
        }
    }

    /// Setup only: casts through the reducer before any ledger exists.
    fn setup_cast(runner: &mut GameRunner, spell: ObjectId, x: u32, target: Option<TargetRef>) {
        let cast = cast_action(runner.state(), spell);
        runner.act(cast).expect("the spell is castable");
        while let Some(answer) = cast_prompt_answer(runner.state(), x, &target) {
            runner
                .act(answer)
                .expect("the cast prompt accepts the answer");
        }
    }

    fn setup_pass(runner: &mut GameRunner, expected: PlayerId) {
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: expected }
        );
        runner.act(GameAction::PassPriority).expect("priority pass");
    }

    fn hand_turn_to(runner: &mut GameRunner, player: PlayerId) {
        let state = runner.state_mut();
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
    }

    /// A host's view of one game: the authoritative state, its ledger and the
    /// AI seats.
    #[derive(Clone)]
    struct Table {
        state: GameState,
        ledger: TakebackLedger,
        ai: Vec<PlayerId>,
    }

    impl Table {
        fn new(state: GameState, ai: &[PlayerId]) -> Self {
            Self {
                ledger: TakebackLedger::new(state.game_number),
                state,
                ai: ai.to_vec(),
            }
        }

        /// What a host runs for one submitted action: an AI seat's transition
        /// is only observed; a human's goes through the freeze, the capture
        /// and `record_human_action`.
        fn act(&mut self, actor: PlayerId, action: GameAction) -> Option<TakebackResolution> {
            if self.ai.contains(&actor) {
                let result = apply(&mut self.state, actor, action).expect("AI action applies");
                self.ledger.observe_transition(&result.events, &self.state);
                return None;
            }
            self.ledger
                .admit_action(&action)
                .expect("the host admits it");
            let capture = TakebackLedger::capture(&self.state, Some(&action));
            let shown = format!("{action:?}");
            let result = apply(&mut self.state, actor, action.clone())
                .unwrap_or_else(|error| panic!("{shown}: {error:?}"));
            self.ledger.record_human_action(
                &mut self.state,
                actor,
                capture,
                &action,
                &result,
                &self.ai,
            )
        }

        fn pass(&mut self, actor: PlayerId) -> Option<TakebackResolution> {
            assert_eq!(
                self.state.waiting_for,
                WaitingFor::Priority { player: actor }
            );
            self.act(actor, GameAction::PassPriority)
        }

        fn concede(&mut self, player: PlayerId) -> Option<TakebackResolution> {
            self.act(player, GameAction::Concede { player_id: player })
        }

        fn cast(&mut self, caster: PlayerId, spell: ObjectId, x: u32, target: Option<TargetRef>) {
            let cast = cast_action(&self.state, spell);
            self.act(caster, cast);
            while let Some(answer) = cast_prompt_answer(&self.state, x, &target) {
                self.act(caster, answer);
            }
        }

        fn request(&mut self, player: PlayerId) -> Result<TakebackOutcome, String> {
            self.ledger
                .request(&mut self.state, player, RewindTarget::LastAction, &self.ai)
        }

        fn respond(&mut self, player: PlayerId, approve: bool) -> Result<TakebackOutcome, String> {
            self.ledger
                .respond(&mut self.state, player, approve, &self.ai)
        }

        fn life(&self, player: PlayerId) -> i32 {
            self.state.players[player.0 as usize].life
        }
    }

    const APPROVED: TakebackOutcome = TakebackOutcome::Resolved(TakebackResolution::Approved);

    /// Bolt-3H: P0, P1 and P2 human, P3 an AI seat. P1 cast Lightning Bolt at
    /// P2 (3 life) and it resolved, so P2 has left the game while it goes on;
    /// P1 and P3 then passed, and P0 holds priority. Returns the table and the
    /// state at P0's priority, before P0's pass (P0's takeback point).
    fn bolt_3h(p0_life: i32) -> (Table, GameState, ObjectId) {
        let mut scenario = GameScenario::new_n_player(4, 11);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P2, 3).with_life(P0, p0_life);
        scenario.add_basic_land(P1, ManaColor::Red);
        scenario.add_basic_land(P1, ManaColor::Red);
        let bolt = add_bolt(&mut scenario, P1);
        let second_bolt = add_bolt(&mut scenario, P1);
        let mut runner = scenario.build();
        hand_turn_to(&mut runner, P1);
        setup_cast(&mut runner, bolt, 0, Some(TargetRef::Player(P2)));
        for seat in [P1, P2, P3, P0] {
            setup_pass(&mut runner, seat);
        }
        assert_eq!(runner.state().eliminated_players, vec![P2]);
        setup_pass(&mut runner, P1);
        setup_pass(&mut runner, P3);
        let before_pass = runner.state().clone();
        let mut table = Table::new(runner.state().clone(), &[P3]);
        table.pass(P0);
        (table, before_pass, second_bolt)
    }

    /// Quake-5: P0, P1 and P2 human at 3 life, P3 and P4 AI seats at 20. P1's
    /// Lightning Bolt removed P2; P1's Earthquake with X=3 is on the stack and
    /// P0's pass resolves it, removing P0 and P1 while P3 and P4 play on.
    /// Returns the table after P0's pass and the target state before it.
    fn quake_5() -> (Table, GameState) {
        let (mut runner, quake) = quake_5_bolt_cast();
        for seat in [P2, P3, P4, P0] {
            setup_pass(&mut runner, seat);
        }
        assert_eq!(runner.state().eliminated_players, vec![P2]);
        setup_cast(&mut runner, quake, 3, None);
        for seat in [P1, P3, P4] {
            setup_pass(&mut runner, seat);
        }
        let target = runner.state().clone();
        let mut table = Table::new(target.clone(), &[P3, P4]);
        table.pass(P0);
        assert_eq!(table.state.eliminated_players, vec![P2, P0, P1]);
        assert!(!matches!(
            table.state.waiting_for,
            WaitingFor::GameOver { .. }
        ));
        (table, target)
    }

    /// The Quake-5 board up to P1's pass on its Lightning Bolt at P2; returns
    /// the runner and P1's Earthquake.
    fn quake_5_bolt_cast() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new_n_player(5, 13);
        scenario.at_phase(Phase::PreCombatMain);
        for seat in [P0, P1, P2] {
            scenario.with_life(seat, 3);
        }
        for _ in 0..5 {
            scenario.add_basic_land(P1, ManaColor::Red);
        }
        let bolt = add_bolt(&mut scenario, P1);
        let quake = add_earthquake(&mut scenario, P1);
        let mut runner = scenario.build();
        hand_turn_to(&mut runner, P1);
        setup_cast(&mut runner, bolt, 0, Some(TargetRef::Player(P2)));
        setup_pass(&mut runner, P1);
        (runner, quake)
    }

    /// Quake-5 played through the `Table` from P2's pass on the Bolt, so P2's
    /// pass is a takeback point taken while P2 was in the game. Returns the
    /// table once the Earthquake has removed P0 and P1.
    fn quake_5_from_the_bolt() -> Table {
        let (runner, quake) = quake_5_bolt_cast();
        let mut table = Table::new(runner.state().clone(), &[P3, P4]);
        for seat in [P2, P3, P4, P0] {
            table.pass(seat);
        }
        assert_eq!(table.state.eliminated_players, vec![P2]);
        table.cast(P1, quake, 3, None);
        for seat in [P1, P3, P4, P0] {
            table.pass(seat);
        }
        assert_eq!(table.state.eliminated_players, vec![P2, P0, P1]);
        table
    }

    /// A duel at precombat main with P0 holding `spell` (from `add`) and
    /// `mountains` Mountains; both players at `life`.
    fn duel(
        life: [i32; 2],
        mountains: usize,
        add: fn(&mut GameScenario, PlayerId) -> ObjectId,
    ) -> (Table, ObjectId) {
        let mut scenario = GameScenario::new_n_player(2, 17);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, life[0]).with_life(P1, life[1]);
        for _ in 0..mountains {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        let spell = add(&mut scenario, P0);
        let runner = scenario.build();
        (Table::new(runner.state().clone(), &[]), spell)
    }

    fn seed_turn_point(table: &mut Table) {
        table.ledger = TakebackLedger::with_turn_rewind(table.state.game_number);
        let events = [GameEvent::TurnStarted {
            player_id: table.state.active_player,
            turn_number: table.state.turn_number,
        }];
        table.ledger.observe_transition(&events, &table.state);
        assert_eq!(table.ledger.turn_history_len(), 1);
    }

    /// L1. Current branch: P2, out of the game, is not asked; P1's approval
    /// alone restores P0's pre-pass state.
    #[test]
    fn a_human_out_of_the_game_is_not_asked_while_another_is_in_it() {
        let (mut table, before_pass, _) = bolt_3h(20);
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.ledger.pending_vote(&table.state, &table.ai),
            Some(TakebackVote {
                requester: P0,
                voters: vec![P1],
            })
        );

        assert_eq!(
            table.respond(P2, true),
            Err("Only the players asked to approve this takeback may respond".to_string())
        );
        assert_eq!(
            table.respond(P3, true),
            Err("Only human players may respond to a takeback request".to_string())
        );
        assert_eq!(
            table.respond(P0, false),
            Err("Only the players asked to approve this takeback may respond".to_string())
        );
        assert!(table.ledger.is_pending());

        assert_eq!(table.respond(P1, true), Ok(APPROVED));
        assert_eq!(table.ledger.pending_vote(&table.state, &table.ai), None);
        assert_eq!(table.state.waiting_for, before_pass.waiting_for);
        assert_eq!(table.state.eliminated_players, vec![P2]);
        assert_eq!(table.state.phase, before_pass.phase);
    }

    /// L2. Target branch: no other human is in the game now, so the humans in
    /// the game at the target decide (P1), and P2, out before it, does not.
    #[test]
    fn with_every_other_human_out_the_humans_in_the_game_at_the_target_decide() {
        let (mut table, target) = quake_5();
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.ledger.pending_vote(&table.state, &table.ai),
            Some(TakebackVote {
                requester: P0,
                voters: vec![P1],
            })
        );
        assert_eq!(
            table.respond(P2, true),
            Err("Only the players asked to approve this takeback may respond".to_string())
        );
        assert_eq!(table.respond(P1, true), Ok(APPROVED));
        assert_eq!(
            table.state.stack.len(),
            1,
            "Earthquake is back on the stack"
        );
        assert_eq!(table.state.stack, target.stack);
        assert_eq!((table.life(P0), table.life(P1)), (3, 3));
        assert_eq!(table.state.eliminated_players, vec![P2]);
    }

    /// L3. No takeback after the game has ended; the identical request before
    /// the Bolt resolves is admitted.
    #[test]
    fn a_takeback_is_refused_once_the_game_is_over() {
        let (mut table, bolt) = duel([20, 3], 1, add_bolt);
        table.cast(P0, bolt, 0, Some(TargetRef::Player(P1)));
        table.pass(P0);

        let mut before_resolution = table.clone();
        assert_eq!(before_resolution.request(P0), Ok(TakebackOutcome::Pending));

        table.pass(P1);
        assert_eq!(
            table.state.waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        );
        assert_eq!(
            table.request(P0),
            Err("A takeback cannot be requested after the game has ended".to_string())
        );
    }

    /// L3 hostile: a draw (CR 104.4a) is `GameOver` too.
    #[test]
    fn a_takeback_is_refused_after_a_drawn_game() {
        let (mut table, quake) = duel([3, 3], 4, add_earthquake);
        table.cast(P0, quake, 3, None);
        table.pass(P0);
        table.pass(P1);
        assert_eq!(
            table.state.waiting_for,
            WaitingFor::GameOver { winner: None }
        );
        assert_eq!(
            table.request(P0),
            Err("A takeback cannot be requested after the game has ended".to_string())
        );
    }

    /// L4. A seat in the game conceding is an actual departure: both rings are
    /// retired, so nothing before the concession can be taken back.
    #[test]
    fn an_actual_departure_retires_both_rings() {
        let (mut table, before_pass, _) = bolt_3h(20);
        let mut seeded = Table::new(before_pass, &[P3]);
        seed_turn_point(&mut seeded);
        seeded.pass(P0);
        assert_eq!(seeded.ledger.history().len(), 1);

        assert_eq!(seeded.concede(P1), None);
        assert_eq!(seeded.ledger.history().len(), 0);
        assert!(seeded.ledger.rewind_options().is_empty());
        assert_eq!(
            seeded.request(P0),
            Err("There is no previous action of yours to take back".to_string())
        );

        // Reach guard: the same request before the concession is admitted.
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
    }

    /// L4 hostile: a two-seat Bo3 concession ends game 1, crossing the game
    /// boundary as it retires.
    #[test]
    fn a_game_ending_concession_in_a_bo3_match_retires_and_crosses_the_boundary() {
        let (mut table, bolt) = duel([20, 20], 1, add_bolt);
        table.state.match_config.match_type = MatchType::Bo3;
        table.state.game_number = 1;
        table.ledger = TakebackLedger::new(1);
        table.cast(P0, bolt, 0, Some(TargetRef::Player(P1)));
        assert_ne!(table.ledger.history().len(), 0);

        assert_eq!(table.concede(P1), None);
        assert_eq!(table.ledger.game_number(), 2);
        assert_eq!(table.ledger.history().len(), 0);
    }

    /// L5. A concession by a seat already out of the game changes nothing:
    /// history is kept (it gains the concession's own point) and P0 can still
    /// ask for its pass back.
    #[test]
    fn a_noop_concession_retires_nothing() {
        let (mut table, _, _) = bolt_3h(20);
        let before: Vec<(PlayerId, GameState)> = table
            .ledger
            .history()
            .map(|(actor, state)| (actor, state.clone()))
            .collect();
        assert_eq!(table.concede(P2), None);
        assert_eq!(table.state.eliminated_players, vec![P2]);
        let after: Vec<(PlayerId, GameState)> = table
            .ledger
            .history()
            .map(|(actor, state)| (actor, state.clone()))
            .collect();
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after[..before.len()], before[..]);
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
    }

    /// L6. A seat in the game conceding during a vote is admitted and withdraws
    /// it; every other action stays frozen.
    #[test]
    fn an_actual_departure_withdraws_an_open_vote() {
        let (mut table, _, _) = bolt_3h(20);
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.ledger.admit_action(&GameAction::PassPriority),
            Err(ActionRejection::new(ActionRejectionCode::ActionNotAllowed))
        );
        assert_eq!(
            table
                .ledger
                .admit_action(&GameAction::Concede { player_id: P1 }),
            Ok(())
        );

        assert_eq!(table.concede(P1), Some(TakebackResolution::Withdrawn));
        assert!(!table.ledger.is_pending());
        assert_eq!(table.ledger.history().len(), 0);
    }

    /// L6 sibling: a no-op concession by a non-voter during a vote withdraws
    /// nothing and records no point.
    #[test]
    fn a_noop_concession_by_a_non_voter_leaves_the_vote_open() {
        let (mut table, _, _) = bolt_3h(20);
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        let depth = table.ledger.history().len();
        assert_eq!(table.concede(P2), None);
        assert!(table.ledger.is_pending());
        assert_eq!(table.ledger.history().len(), depth);
    }

    /// L7. The requester's own concession cancels its request.
    #[test]
    fn the_requesters_concession_cancels_its_request() {
        let (mut table, _, second_bolt) = bolt_3h(3);
        while table.state.waiting_for != (WaitingFor::Priority { player: P1 }) {
            let WaitingFor::Priority { player } = table.state.waiting_for else {
                panic!(
                    "expected a priority window, got {:?}",
                    table.state.waiting_for
                );
            };
            table.pass(player);
        }
        table.cast(P1, second_bolt, 0, Some(TargetRef::Player(P0)));
        table.pass(P1);
        table.pass(P3);
        table.pass(P0);
        assert_eq!(table.state.eliminated_players, vec![P2, P0]);

        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(table.concede(P0), Some(TakebackResolution::Cancelled));
        assert!(!table.ledger.is_pending());
    }

    /// L7 sibling: a requester in the game conceding actually departs, which
    /// withdraws rather than cancels.
    #[test]
    fn a_requester_in_the_game_conceding_withdraws_its_request() {
        let (mut table, _, _) = bolt_3h(20);
        table.pass(P1);
        assert_eq!(table.request(P1), Ok(TakebackOutcome::Pending));
        assert_eq!(table.concede(P1), Some(TakebackResolution::Withdrawn));
        assert!(!table.ledger.is_pending());
    }

    /// L8. In the target branch a voter's no-op concession drops it, and the
    /// vote resolves on the remaining approvals.
    #[test]
    fn a_target_branch_voters_noop_concession_resolves_the_vote() {
        let (mut table, target) = quake_5();
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(table.concede(P1), Some(TakebackResolution::Approved));
        assert!(!table.ledger.is_pending());
        assert_eq!(table.state.stack, target.stack);
        assert_eq!(table.state.eliminated_players, target.eliminated_players);
    }

    /// L9. A seat that conceded since the target is not asked; one that
    /// conceded only before the target is.
    #[test]
    fn a_seat_that_conceded_since_the_target_is_not_asked() {
        let (mut table, _) = quake_5();
        assert_eq!(table.concede(P1), None);
        assert_eq!(table.request(P0), Ok(APPROVED));
        assert_eq!(table.state.eliminated_players, vec![P2]);

        // P0's new pass is a point after P1's concession record.
        table.pass(P0);
        assert_eq!(table.state.eliminated_players, vec![P2, P0, P1]);
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(table.respond(P1, true), Ok(APPROVED));
    }

    /// E1. An approval back past P1's no-op concession forgets it, so a later
    /// request targeting a point before it asks P1 again.
    #[test]
    fn an_approved_rollback_forgets_the_concessions_it_undid() {
        let mut table = quake_5_from_the_bolt();
        assert_eq!(table.concede(P1), None);
        assert_eq!(table.request(P0), Ok(APPROVED));
        assert_eq!(table.state.eliminated_players, vec![P2]);
        table.pass(P0);
        assert_eq!(table.state.eliminated_players, vec![P2, P0, P1]);

        assert_eq!(table.request(P2), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.ledger.pending_vote(&table.state, &table.ai),
            Some(TakebackVote {
                requester: P2,
                voters: vec![P0, P1],
            })
        );
        assert_eq!(table.respond(P1, true), Ok(TakebackOutcome::Pending));
        assert_eq!(table.respond(P0, true), Ok(APPROVED));
        assert!(table.state.eliminated_players.is_empty());
    }

    /// E2. The approval forgets P0's concession, which it undid, and keeps P1's,
    /// which precedes its target and still excludes P1.
    #[test]
    fn an_approval_keeps_the_concessions_before_its_target() {
        let mut table = quake_5_from_the_bolt();
        assert_eq!(table.concede(P1), None);
        assert_eq!(table.concede(P0), None);
        assert_eq!(table.request(P0), Ok(APPROVED));

        assert_eq!(table.request(P2), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.respond(P1, true),
            Err("Only the players asked to approve this takeback may respond".to_string())
        );
        assert_eq!(table.respond(P0, true), Ok(APPROVED));
    }

    /// E2 sibling: P1's second concession, after the target, is undone by the
    /// approval, and its first one still excludes P1.
    #[test]
    fn an_approval_keeps_a_concession_before_its_target_despite_a_later_one() {
        let mut table = quake_5_from_the_bolt();
        assert_eq!(table.concede(P1), None);
        assert_eq!(table.concede(P0), None);
        assert_eq!(table.concede(P1), None);
        assert_eq!(table.request(P0), Ok(APPROVED));

        assert_eq!(table.request(P2), Ok(TakebackOutcome::Pending));
        assert_eq!(
            table.respond(P1, true),
            Err("Only the players asked to approve this takeback may respond".to_string())
        );
        assert_eq!(table.respond(P0, true), Ok(APPROVED));
    }

    /// L10. An accepted match forfeit withdraws an open vote and retires both
    /// rings.
    #[test]
    fn a_match_forfeit_withdraws_and_retires() {
        let (_, before_pass, _) = bolt_3h(20);
        let mut table = Table::new(before_pass, &[P3]);
        seed_turn_point(&mut table);
        table.pass(P0);
        let mut no_vote = table.clone();
        assert_eq!(table.request(P0), Ok(TakebackOutcome::Pending));

        assert_eq!(
            table.ledger.observe_match_forfeit(),
            Some(TakebackResolution::Withdrawn)
        );
        assert!(!table.ledger.is_pending());
        assert_eq!(table.ledger.history().len(), 0);
        assert_eq!(table.ledger.turn_history_len(), 0);

        assert_eq!(no_vote.ledger.observe_match_forfeit(), None);
        assert_eq!(no_vote.ledger.history().len(), 0);
        assert_eq!(no_vote.ledger.turn_history_len(), 0);
    }

    /// A state that has come to rest in `turn_number`, with `active_player`
    /// distinct from seat 0 so a label mix-up is visible.
    fn resting_at(turn_number: u32) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = turn_number;
        state.active_player = PlayerId(1);
        state
    }

    fn turn_started(turn_number: u32) -> GameEvent {
        GameEvent::TurnStarted {
            player_id: PlayerId(1),
            turn_number,
        }
    }

    fn turn_ledger() -> TakebackLedger {
        TakebackLedger::with_turn_rewind(resting_at(1).game_number)
    }

    /// R1. A batch that announces turn 4 and comes to rest in turn 4 is
    /// recorded, labelled from the stored state's own fields.
    #[test]
    fn a_turn_point_stores_the_boundary_it_came_to_rest_in() {
        let mut ledger = turn_ledger();
        ledger.observe_transition(&[turn_started(4)], &resting_at(4));
        assert_eq!(
            ledger.rewind_options(),
            vec![RewindOption {
                turn_number: 4,
                active_player: PlayerId(1),
            }]
        );
    }

    /// R1 hostile A. A batch that both started and ended a turn is refused
    /// rather than mislabelled.
    #[test]
    fn a_batch_that_started_and_ended_a_turn_records_nothing() {
        let mut ledger = turn_ledger();
        ledger.observe_transition(&[turn_started(4)], &resting_at(5));
        assert_eq!(ledger.turn_history_len(), 0);
    }

    /// R1 hostile B. Two boundaries in one batch record only the one it rests
    /// in.
    #[test]
    fn a_batch_carrying_two_boundaries_records_only_the_one_it_rests_in() {
        let mut ledger = turn_ledger();
        ledger.observe_transition(&[turn_started(4), turn_started(5)], &resting_at(5));
        let options = ledger.rewind_options();
        assert_eq!(
            options.len(),
            1,
            "no duplicate entry for the passed-through turn"
        );
        assert_eq!(options[0].turn_number, 5);
    }

    /// R1 hostile C. A batch with no boundary records nothing.
    #[test]
    fn a_batch_with_no_turn_boundary_records_nothing() {
        let mut ledger = turn_ledger();
        ledger.observe_transition(
            &[GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            }],
            &resting_at(4),
        );
        assert_eq!(ledger.turn_history_len(), 0);
    }

    /// R2. A ledger built without turn rewind records no turn point for the
    /// fixture R1 records.
    #[test]
    fn a_ledger_without_turn_rewind_records_no_turn_point() {
        let mut ledger = TakebackLedger::new(resting_at(1).game_number);
        ledger.observe_transition(&[turn_started(4)], &resting_at(4));
        assert_eq!(ledger.turn_history_len(), 0);
        assert!(ledger.rewind_options().is_empty());
    }

    /// R3. The ring is bounded and drops the OLDEST entry.
    #[test]
    fn turn_rewind_ring_is_bounded_and_drops_oldest() {
        let mut ledger = turn_ledger();
        let overflow = MAX_TURN_REWIND_HISTORY as u32 + 2;
        for turn in 1..=overflow {
            ledger.observe_transition(&[turn_started(turn)], &resting_at(turn));
        }
        let options = ledger.rewind_options();
        assert_eq!(options.len(), MAX_TURN_REWIND_HISTORY);
        assert_eq!(options.first().map(|option| option.turn_number), Some(3));
        assert_eq!(
            options.last().map(|option| option.turn_number),
            Some(overflow)
        );
    }
}
