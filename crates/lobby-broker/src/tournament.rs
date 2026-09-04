//! `TournamentManager` — the pure, WASM-safe tournament core.
//!
//! Functional core, no shell: every method here is a pure function of
//! `(&self`/`&mut self, .., env: &impl BrokerEnv)`. No tokio, no axum, no
//! `SystemTime::now()`, no `rand` — wall-clock time and token minting are
//! injected through [`BrokerEnv`] exactly the way [`crate::lobby`] already
//! does, so the identical logic runs in the native `phase-server` shell and a
//! Cloudflare Durable Object (WASM).
//!
//! Scope: this module is PR 1 of the tournament-organizer design
//! (`docs/proposals/tournament-organizer/PLAN.md` §4) — the algorithm and
//! state machine only. Deliberately absent, and deliberately NOT worked
//! around here:
//!
//! * No `Outbound`/`LobbyServerMessage` anywhere. [`TournamentManager::check_expired`]
//!   returns plain domain data ([`TournamentExpiryEvent`]), mirroring
//!   [`crate::lobby::LobbyManager::check_expired`]'s plain `Vec<String>`;
//!   wrapping those into outbounds is `Broker::reap_expired`'s job (PR 2),
//!   the same split that already exists for lobby games.
//! * No `Broker`/`ConnState`/protocol changes — PR 2.
//! * No worker shell or frontend — PR 3/4.
//!
//! Rules provenance. Two documents are cited throughout:
//! * **MTR** — the *Magic Tournament Rules* (Wizards of the Coast), the
//!   official document, cited by section (`MTR §2.1`, `MTR Appendix C/E`).
//! * **MSTR** — the *Multiplayer Addendum to the Magic Tournament Rules*, an
//!   **unofficial, independent-judge-authored** convention this design
//!   deliberately adopts because the official MTR has no
//!   multiplayer-pairing/scoring section at all. It is cited by name only:
//!   the source is continuous prose/tables, not numbered rule text, so there
//!   is no section number to cite (`RESEARCH.md` §13). It is never presented
//!   as Wizards policy.
//!
//! Neither is the Comprehensive Rules, so nothing in this file carries a
//! `CR` annotation: tournament administration (pairings, match points,
//! tiebreakers, byes, drops, retention) is not CR-governed game logic.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::env::BrokerEnv;

// ---------------------------------------------------------------------------
// Lifecycle windows
// ---------------------------------------------------------------------------

/// A `Registration`-status tournament nobody has started is reaped after this
/// many seconds of inactivity — the same 300-second window the existing lobby
/// reaper already uses (`broker.reap_expired(300, &SysEnv)`), because an
/// abandoned registration is the same shape of staleness.
pub const REGISTRATION_TIMEOUT_SECS: u64 = 300;

/// An `InProgress` tournament with no activity for this long is transitioned
/// to [`TournamentStatus::Abandoned`] (record and history preserved — only the
/// "still live" status ends). Seven days comfortably exceeds any real
/// multi-round event's between-round gaps. System default, deliberately NOT
/// organizer-overridable: this is server hygiene, not a tournament rule.
pub const IN_PROGRESS_ABANDON_SECS: u64 = 7 * 24 * 60 * 60;

/// A terminal (`Completed`/`Abandoned`) tournament is deleted outright this
/// long after it reached that state — long enough to look up final standings
/// after the fact, bounded enough that the in-memory map doesn't grow forever.
pub const TERMINAL_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// MatchArity
// ---------------------------------------------------------------------------

/// Number of players seated at one pairing. `HEAD_TO_HEAD` (2) is
/// Standard/Modern/etc. Swiss; `COMMANDER_POD` (4) is the common Commander
/// pod size per MSTR. Chosen once at creation and immutable for the
/// tournament's lifetime — re-sizing pods mid-event would invalidate
/// standings already computed against the old size.
///
/// This is a parameterization axis, not a format enum: pairing, scoring and
/// tiebreak-order selection are all functions of `MatchArity` rather than
/// forked per format.
///
/// The field is private; construction goes through [`MatchArity::new`], never
/// a bare tuple-struct literal or an unvalidated deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct MatchArity(u8);

impl MatchArity {
    /// One-versus-one: the MTR's own (and this engine's) default shape.
    pub const HEAD_TO_HEAD: MatchArity = MatchArity(2);
    /// The standard Commander pod size per MSTR.
    pub const COMMANDER_POD: MatchArity = MatchArity(4);

    /// Validated construction. Rejects `0`/`1` (a pairing needs at least two
    /// seats to pair anyone) and caps at `128`, the largest `n` for which the
    /// MSTR win-point formula `2n - 1` still fits `u8` (`2*128-1 == 255`).
    ///
    /// `#[serde(try_from = "u8")]` routes wire deserialization through this
    /// same constructor, so a malformed payload is rejected at the
    /// deserialization boundary rather than discovered broken later inside
    /// pairing/scoring logic.
    pub fn new(n: u8) -> Result<Self, String> {
        if n < 2 {
            return Err(format!(
                "MatchArity must be at least 2 (a pairing needs >=2 seats), got {n}"
            ));
        }
        if n > 128 {
            return Err(format!(
                "MatchArity {n} exceeds 128 - win_points (2n-1) would overflow u8"
            ));
        }
        Ok(MatchArity(n))
    }

    /// The seat count as a plain `u8`.
    pub fn get(self) -> u8 {
        self.0
    }

    /// Seats in a *short* pod — one fewer than a full pod. At
    /// `HEAD_TO_HEAD` this is `1`, which is a bye rather than a pod; callers
    /// must treat a one-player pairing as [`PairingOutcome::Bye`].
    pub fn short_pod_size(self) -> u8 {
        self.0 - 1
    }
}

impl TryFrom<u8> for MatchArity {
    type Error = String;
    fn try_from(n: u8) -> Result<Self, Self::Error> {
        Self::new(n)
    }
}

impl From<MatchArity> for u8 {
    fn from(arity: MatchArity) -> u8 {
        arity.0
    }
}

// ---------------------------------------------------------------------------
// ScoringPolicy
// ---------------------------------------------------------------------------

/// Tournament match-point scoring. MSTR generalizes MTR §2.1's 3/1/0 to a
/// single formula: `win_points = 2n - 1` for pod size `n` — at `n = 2` that
/// collapses to exactly 3, so this is the same rule as MTR's, not a fork of
/// it. Organizer-overridable at creation for communities running variant
/// conventions.
///
/// Lives in `lobby-broker` and never touches `GameState`: match points and
/// standings are a multi-game event concept the engine crate has no notion of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawScoringPolicy", into = "RawScoringPolicy")]
pub struct ScoringPolicy {
    win_points: u8,
    draw_points: u8,
    loss_points: u8,
}

/// Plain (de)serialization target for [`ScoringPolicy`]'s `try_from`/`into`
/// boundary — the same pattern [`MatchArity`] uses via `try_from = "u8"`,
/// generalized to a three-field struct. It must derive `Serialize` as well as
/// `Deserialize`: `#[serde(into = "RawScoringPolicy")]` generates a
/// `Serialize` impl for `ScoringPolicy` that converts to this type and
/// serializes *that*.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RawScoringPolicy {
    pub win_points: u8,
    pub draw_points: u8,
    pub loss_points: u8,
}

impl ScoringPolicy {
    /// Validated construction. `win_points == 0` is rejected because it is
    /// the tiebreak floor's denominator ([`ScoringPolicy::tiebreak_floor`]);
    /// zero would reach an infinite floor instead of being caught at the
    /// boundary where organizer-supplied configuration enters the broker.
    ///
    /// No ordering is imposed between the three values: organizer overrides
    /// are explicitly supported (some communities score draws as 0), and a
    /// stricter rule would need its own cited requirement rather than being
    /// an implicit side effect of this one.
    pub fn new(win_points: u8, draw_points: u8, loss_points: u8) -> Result<Self, String> {
        if win_points == 0 {
            return Err(
                "win_points must be non-zero - used as the tiebreak floor's denominator"
                    .to_string(),
            );
        }
        Ok(Self {
            win_points,
            draw_points,
            loss_points,
        })
    }

    pub fn win_points(&self) -> u8 {
        self.win_points
    }
    pub fn draw_points(&self) -> u8 {
        self.draw_points
    }
    pub fn loss_points(&self) -> u8 {
        self.loss_points
    }

    /// MSTR-derived default: `2n - 1` match points for a win. At
    /// `HEAD_TO_HEAD` this is exactly MTR §2.1's 3/1/0.
    ///
    /// [`MatchArity::new`] already caps arity at 128, so the *final* value is
    /// always in `3..=255` — but the *intermediate* `2 * n` is not
    /// (`2 * 128 == 256` overflows `u8` before the subtraction runs, even
    /// though the post-subtraction `255` would have fit). Computed in `u16`
    /// and converted down with a checked conversion, never a bare `as` cast
    /// that would silently truncate if the arity bound ever changed.
    pub fn default_for_arity(arity: MatchArity) -> Self {
        let n = u16::from(arity.get());
        let win_points = u8::try_from(2 * n - 1)
            .expect("MatchArity::new caps arity at 128, so 2n-1 always fits u8");
        Self {
            win_points,
            draw_points: 1,
            loss_points: 0,
        }
    }

    /// The shared tiebreak floor, `1 / win_points`. MTR's 1v1 floor of 0.33
    /// and MSTR's 4-player-pod floor of ~0.14 are the same formula with a
    /// different plug-in value (`1/3` and `1/7`), so the floor needs no arity
    /// branch at all — only the tiebreak *order* is arity-selected.
    pub fn tiebreak_floor(&self) -> f64 {
        1.0 / f64::from(self.win_points)
    }
}

impl TryFrom<RawScoringPolicy> for ScoringPolicy {
    type Error = String;
    fn try_from(raw: RawScoringPolicy) -> Result<Self, String> {
        Self::new(raw.win_points, raw.draw_points, raw.loss_points)
    }
}

impl From<ScoringPolicy> for RawScoringPolicy {
    fn from(policy: ScoringPolicy) -> Self {
        Self {
            win_points: policy.win_points,
            draw_points: policy.draw_points,
            loss_points: policy.loss_points,
        }
    }
}

impl Default for ScoringPolicy {
    /// MTR §2.1, equivalently `default_for_arity(MatchArity::HEAD_TO_HEAD)`.
    fn default() -> Self {
        Self::default_for_arity(MatchArity::HEAD_TO_HEAD)
    }
}

// ---------------------------------------------------------------------------
// Status / bracket shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TournamentStatus {
    /// Created, accepting joins, no round generated yet.
    Registration,
    /// At least one round has been generated.
    InProgress,
    /// Organizer-initiated stop via
    /// [`TournamentManager::complete_tournament`], standings frozen — a
    /// trustworthy final result in the one sense the engine can guarantee:
    /// *every pairing that exists has a resolved outcome* (reported,
    /// forfeited, or a bye), so no reported match is missing from the
    /// standings. It does **not** promise that every scheduled round ran —
    /// round count is an overridable recommendation
    /// ([`default_total_rounds`]), and an organizer may stop early once the
    /// current round is settled (see `complete_tournament`'s own rationale).
    /// `current_round < total_rounds()` is therefore a legal shape for a
    /// `Completed` event, and clients must not read this status as "the full
    /// scheduled event was played". The comparison is against a schedule that
    /// no longer moves once the event starts — an unset override resolves
    /// through [`TournamentMeta::resolved_total_rounds`], latched at round 1 —
    /// so a short `Completed` event genuinely ended early rather than having
    /// its target quietly recomputed downward by a drop.
    Completed,
    /// Reached only via [`TournamentManager::check_expired`]'s 7-day
    /// inactivity transition, never organizer-initiated. Distinct from
    /// `Completed`: an abandoned event's final round(s) may still have
    /// `Pending` pairings, so its standings reflect whatever was reported
    /// before activity stopped, not a guaranteed-complete result. Kept as its
    /// own status specifically so clients can display that distinction.
    Abandoned,
}

impl TournamentStatus {
    /// Terminal statuses stop accepting mutations and start the 30-day
    /// retention clock.
    pub fn is_terminal(self) -> bool {
        match self {
            TournamentStatus::Registration | TournamentStatus::InProgress => false,
            TournamentStatus::Completed | TournamentStatus::Abandoned => true,
        }
    }
}

/// Which bracket shape the same [`TournamentManager`] runs for a tournament.
/// `SingleElimination` covers MTR Appendix E's 4-8 player case — the whole
/// contiguous range, byes included, not only the power-of-two counts (see
/// [`SINGLE_ELIMINATION_MIN_PLAYERS`]/[`SINGLE_ELIMINATION_MAX_PLAYERS`]). It
/// is gated to `MatchArity::HEAD_TO_HEAD` at construction time, because
/// pod-based single elimination (advancement semantics for a multi-player
/// bracket match) is an unresolved design question explicitly excluded from
/// v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BracketShape {
    Swiss,
    SingleElimination,
}

// ---------------------------------------------------------------------------
// Pairings and outcomes
// ---------------------------------------------------------------------------

/// Stable, tournament-scoped pairing identity — a monotonic counter, not a
/// re-derivable index. Pairings are never removed or reordered, and this never
/// leaves the tournament's own scope, so a plain `u32` is sufficient: there is
/// no invalid state space for a validated newtype to reject.
pub type PairingId = u32;

/// The reported content of a *played* pairing. Per MSTR, match results (not
/// individual game results) are reported for pods: a pod match has exactly one
/// winner or is a full draw, never per-seat placement. Never represents a bye
/// or a forfeit — see [`PairingOutcome`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PodOutcome {
    /// `game_wins` is validated differently by arity (see
    /// [`validate_match_result`]): at `HEAD_TO_HEAD` it MUST contain exactly
    /// the two participants with a legal completed-Bo3 tally; for a pod
    /// (`arity > 2`) it MUST be empty, because pods are single-game per MSTR
    /// and there is no per-player game-win count to report.
    Decisive {
        winner: String,
        game_wins: HashMap<String, u8>,
    },
    Draw,
}

/// A pairing's resolved outcome. Three distinct variants rather than a
/// `bool is_bye` + `Option<String> forfeit_winner` + `Option<PodOutcome>`
/// triple: the type itself makes "a bye, a forfeit and a reported result are
/// mutually exclusive" a compile-time fact instead of a runtime invariant
/// three loose fields could violate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingOutcome {
    /// Auto-assigned the instant a one-player pairing is generated — never
    /// client-reported, never passes through [`validate_match_result`].
    Bye,
    /// Auto-assigned when every player but one in a pairing has dropped before
    /// the pairing was reported; the remaining player wins by forfeit. Also
    /// server-assigned only, and permanent once assigned.
    Forfeit { winner: String },
    /// A real, client-or-organizer-reported result for a pairing that was
    /// actually played.
    Reported(PodOutcome),
}

/// One generated pairing. `players` holds 2 keys for a head-to-head pairing,
/// up to `arity` for a pod, exactly `arity - 1` for a short pod, or exactly 1
/// for a bye.
///
/// `TournamentMeta::pairings` accumulates every pairing ever generated, across
/// every round — never pruned, never summarized into running totals elsewhere.
/// Match points, opponents faced, `had_bye` and `had_short_pod` are all
/// derived by scanning that list fresh, which is what makes replay-safe result
/// correction well-defined: [`TournamentManager::report_result`] is a single
/// `outcome` write, and every derived view recomputes from the corrected
/// history on its next read. There is no cache or running total a correction
/// could leave stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentPairing {
    pub id: PairingId,
    pub round: u32,
    pub players: Vec<String>,
    /// `None` = pending; `Some(_)` once resolved.
    pub outcome: Option<PairingOutcome>,
}

impl TournamentPairing {
    /// The player credited with the win, if this pairing has one. `None` for a
    /// pending pairing and for a draw.
    pub fn winner(&self) -> Option<&str> {
        match &self.outcome {
            Some(PairingOutcome::Bye) => self.players.first().map(String::as_str),
            Some(PairingOutcome::Forfeit { winner }) => Some(winner.as_str()),
            Some(PairingOutcome::Reported(PodOutcome::Decisive { winner, .. })) => {
                Some(winner.as_str())
            }
            Some(PairingOutcome::Reported(PodOutcome::Draw)) | None => None,
        }
    }

    fn seats(&self, player_key: &str) -> bool {
        self.players.iter().any(|k| k == player_key)
    }
}

/// Has this player already received a bye? Derived fresh from the pairing
/// history on every call — deliberately NOT a stored, incrementally-mutated
/// field, so a corrected result or a re-generated round cannot leave it stale.
pub fn had_bye(player_key: &str, pairings: &[TournamentPairing]) -> bool {
    pairings.iter().any(|p| {
        p.players.len() == 1 && p.players[0] == player_key && p.outcome == Some(PairingOutcome::Bye)
    })
}

/// Has this player already been seated in a short (`arity - 1`) pod? MSTR:
/// "it is desirable that Players only get matched in smaller size pods at most
/// once per event."
///
/// Always `false` at `HEAD_TO_HEAD`, where `arity - 1 == 1` is the bye path,
/// not a short pod — [`had_bye`] is the fairness query there.
pub fn had_short_pod(player_key: &str, arity: MatchArity, pairings: &[TournamentPairing]) -> bool {
    let short = usize::from(arity.short_pod_size());
    if short < 2 {
        return false;
    }
    pairings
        .iter()
        .any(|p| p.players.len() == short && p.seats(player_key))
}

/// Every distinct player this player has already been seated with, across
/// every round. Derived fresh; used for rematch avoidance.
pub fn prior_opponents(player_key: &str, pairings: &[TournamentPairing]) -> HashSet<String> {
    let mut out = HashSet::new();
    for pairing in pairings.iter().filter(|p| p.seats(player_key)) {
        out.extend(
            pairing
                .players
                .iter()
                .filter(|k| k.as_str() != player_key)
                .cloned(),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Players, meta, requests
// ---------------------------------------------------------------------------

/// A registered entrant. `had_bye`/`had_short_pod` are deliberately absent as
/// fields — they are derived queries over the pairing history (see
/// [`had_bye`]), so there is nothing to fall out of sync with a correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentPlayer {
    pub player_key: String,
    /// Minted at join via [`BrokerEnv::new_token`], NOT socket-bound: closing
    /// and reopening a socket must not drop a player's standing.
    pub player_token: String,
    pub display_name: String,
    pub dropped: bool,
}

/// Fields a caller supplies when creating a tournament. A request struct
/// rather than a long positional argument list, matching
/// [`crate::lobby::RegisterGameRequest`]'s precedent in this crate.
#[derive(Debug, Clone)]
pub struct CreateTournamentRequest {
    pub name: String,
    pub arity: MatchArity,
    pub scoring: ScoringPolicy,
    pub bracket: BracketShape,
    /// Organizer override. `None` uses the bracket- and arity-selected
    /// default ([`default_total_rounds`]), resolved against the live player
    /// count rather than frozen at creation time (when nobody has joined
    /// yet) — and then latched into
    /// [`TournamentMeta::resolved_total_rounds`] once round 1 is paired, so
    /// a drop cannot shorten a schedule that is already being played.
    pub total_rounds: Option<u32>,
}

/// One tournament's durable record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentMeta {
    pub code: String,
    pub name: String,
    /// Minted at creation via [`BrokerEnv::new_token`], NOT socket-bound.
    pub organizer_token: String,
    pub arity: MatchArity,
    pub scoring: ScoringPolicy,
    pub bracket: BracketShape,
    /// `None` means "use [`default_total_rounds`]". Stored as an `Option`
    /// rather than an eagerly-resolved `u32` because at creation time the
    /// player count is zero, and the default is keyed on
    /// `(bracket, arity, player_count)` — resolving it against an empty field
    /// would bake in a meaningless value. The default is instead latched into
    /// [`Self::resolved_total_rounds`] at the first moment it *is* meaningful,
    /// when round 1 is paired. Read through [`TournamentMeta::total_rounds`].
    pub total_rounds_override: Option<u32>,
    /// The *engine-computed* round count, latched the first time round 1 is
    /// paired — as opposed to [`Self::total_rounds_override`], which is the
    /// *organizer's* explicit choice and always wins over this one. `None`
    /// until round 1 exists, so a `Registration` event whose field is still
    /// growing keeps resolving [`Self::total_rounds`] live.
    ///
    /// Latched rather than recomputed because [`default_total_rounds`] is
    /// keyed on the *active* player count, and a drop shrinks that count
    /// mid-event: a 3-player single-elimination bracket is two rounds deep,
    /// but one round-1 drop would recompute the default to 1 and the round
    /// ceiling in [`TournamentManager::generate_pairings`] would then refuse
    /// the bracket's own final. The schedule an event started under is the
    /// schedule it finishes under.
    pub resolved_total_rounds: Option<u32>,
    pub current_round: u32,
    pub status: TournamentStatus,
    pub players: Vec<TournamentPlayer>,
    /// Durable pairing history — the single source of truth every derived
    /// view reads fresh.
    pub pairings: Vec<TournamentPairing>,
    /// Unix seconds, matching [`crate::lobby::LobbyManager`]'s own convention.
    pub created_at: u64,
    /// Bumped on every mutation *and* on every status transition, so it
    /// doubles as "time of the most recent state change" for retention
    /// purposes without a second timestamp field.
    pub last_activity_at: u64,
}

impl TournamentMeta {
    /// The scheduled round count, resolved in three tiers:
    ///
    /// 1. [`Self::total_rounds_override`] — the organizer's explicit choice,
    ///    which always wins outright.
    /// 2. [`Self::resolved_total_rounds`] — the computed default, latched
    ///    when round 1 was paired. Recomputation stops there on purpose: the
    ///    default is keyed on the active player count, and a mid-event drop
    ///    must not shorten a schedule that is already being played (see that
    ///    field for the single-elimination case it would otherwise break).
    /// 3. Otherwise the live bracket- and arity-selected default for the
    ///    current active-player count — the right answer for an event that
    ///    has not started yet, whose field is still growing.
    ///
    /// All three inputs to that last tier go through the single
    /// [`default_total_rounds`] authority rather than being branched on here,
    /// so a caller holding a bare `(bracket, arity, player_count)` gets the
    /// same answer this does.
    pub fn total_rounds(&self) -> u32 {
        self.total_rounds_override
            .or(self.resolved_total_rounds)
            .unwrap_or_else(|| {
                default_total_rounds(self.bracket, self.arity, self.active_player_count())
            })
    }

    /// Non-dropped entrants, in join order.
    pub fn active_players(&self) -> impl Iterator<Item = &TournamentPlayer> {
        self.players.iter().filter(|p| !p.dropped)
    }

    pub fn active_player_count(&self) -> u32 {
        self.active_players().count() as u32
    }

    pub fn player(&self, player_key: &str) -> Option<&TournamentPlayer> {
        self.players.iter().find(|p| p.player_key == player_key)
    }

    pub fn pairing(&self, id: PairingId) -> Option<&TournamentPairing> {
        self.pairings.iter().find(|p| p.id == id)
    }

    /// The first pairing in this tournament's history that still has no
    /// outcome, if any — the single authority both round advancement
    /// ([`TournamentManager::generate_pairings`]) and completion
    /// ([`TournamentManager::complete_tournament`]) consult before moving the
    /// tournament forward, rather than each re-implementing the scan.
    ///
    /// Both refuse to move past an unresolved pairing. Leaving one behind is
    /// not recoverable: the missing match points silently distort every
    /// standing derived after it, a later round would seed itself from those
    /// distorted standings, and once the tournament reaches a terminal status
    /// [`TournamentManager::report_result`] refuses the write that would have
    /// fixed it. An organizer whose players never reported has
    /// [`TournamentManager::drop_player`] (auto-forfeit) or an explicit report
    /// as the way out — both leave a stated reason in the history, which
    /// silently pairing around the hole would not.
    ///
    /// Scans every round, not just [`Self::current_round`]. The
    /// `generate_pairings` guard means no earlier round can hold a pending
    /// pairing, so this is the invariant being *checked* rather than assumed —
    /// and it costs one pass over a list that is already scanned fresh for
    /// every derived view.
    pub fn first_unresolved_pairing(&self) -> Option<&TournamentPairing> {
        self.pairings.iter().find(|p| p.outcome.is_none())
    }

    /// Which ranked tiebreak list applies, selected by arity.
    pub fn tiebreak_order(&self) -> TiebreakOrder {
        TiebreakOrder::for_arity(self.arity)
    }
}

/// Recommended round count when the organizer supplies no override.
///
/// **`BracketShape::SingleElimination` — not a table lookup at all.** A
/// single-elimination bracket's length is a property of its own shape, not a
/// recommendation: [`build_single_elimination_round`] rounds the field up to
/// `next_power_of_two()` slots and halves the survivors every round, so a
/// `2^r`-slot bracket is decided in exactly `r` rounds (2 players -> 1,
/// 3-4 -> 2, 5-8 -> 3), and the pairing builder itself refuses a further
/// round once one entrant is left. Consulting the Swiss tables here would
/// report a round the bracket can never pair. A field below
/// [`SINGLE_ELIMINATION_MIN_PLAYERS`] yields 0 — the same "there is no
/// bracket to run" the pairing builder rejects, rather than a fabricated
/// floor.
///
/// **`BracketShape::Swiss`** uses two separate cited tables, selected by
/// arity — MTR Appendix E and MSTR's own table are genuinely different inputs
/// to the same kind of lookup, not one table with an extra column.
///
/// **`HEAD_TO_HEAD` — MTR Appendix E, i.e. the doubling rule.** Its rows are
/// exactly "the smallest `r` with `2^r >= players`", floored at 3 for the 4-8
/// bracket: 4-8 -> 3, 9-16 -> 4, 17-32 -> 5, 33-64 -> 6, 65-128 -> 7, and the
/// same doubling continues unbroken above that — 129-256 -> 8, 257-512 -> 9,
/// 513-1024 -> 10, 1025-2048 -> 11, and so on. There is no plateau and no
/// off-power-of-two row boundary anywhere in the sequence, so nothing here
/// caps or special-cases large fields. The boundaries were confirmed against
/// direct real-world tournament-organizer experience (16 -> 4 and 17 -> 5
/// exactly, with the pattern holding cleanly upward);
/// `default_total_rounds_follows_the_doubling_rule_without_plateau` pins every
/// one of them, including 1025 -> 11 as proof the count keeps climbing.
///
/// This is literally the same arithmetic as the `SingleElimination` arm above:
/// both are `ceil(log2(field))`, because the number of Swiss rounds needed to
/// separate a field is the depth of the bracket that would decide it. The only
/// difference is Swiss's 3-round floor. An organizer who wants a different
/// value — matching some other published chart for an unusual format, or
/// running a very large field to a different length — sets
/// [`TournamentMeta::total_rounds_override`], which wins outright.
///
/// **`arity > HEAD_TO_HEAD` — MSTR's own table** for 4-player pods, quoted in
/// `RESEARCH.md` §13: 6-16 -> 2 rounds, 17-24 -> 3, 25-32 -> 4, 33-40 -> 5,
/// 41-64 -> 5, plus a stated Competitive-REL minimum of 2 rounds for
/// multiplayer events. The table's own 4-5 player row is "single elimination
/// only, no Swiss", which v1 cannot honour (pod single elimination is excluded
/// from v1), so those counts fall back to that 2-round minimum. The table
/// stops at 64 players and is stated for 4-player pods specifically; larger
/// fields and other pod sizes clamp to its last published row rather than
/// extrapolating a row the source never published.
pub fn default_total_rounds(bracket: BracketShape, arity: MatchArity, player_count: u32) -> u32 {
    match bracket {
        // Bracket depth: ceil(log2(slots)) for the same `next_power_of_two()`
        // slot count `build_single_elimination_round` seeds round 1 from.
        // Computed in `u64` so the widening is total for every `u32` field.
        BracketShape::SingleElimination => {
            u64::from(player_count).next_power_of_two().trailing_zeros()
        }
        BracketShape::Swiss => {
            if arity == MatchArity::HEAD_TO_HEAD {
                // Smallest r with 2^r >= max(player_count, 8): the `max` is
                // Appendix E's floor of 3 rounds for the 4-8 bracket.
                let target = u64::from(player_count.max(8));
                let mut rounds = 0u32;
                while (1u64 << rounds) < target {
                    rounds += 1;
                }
                rounds
            } else {
                match player_count {
                    0..=16 => 2,
                    17..=24 => 3,
                    25..=32 => 4,
                    _ => 5,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Standings and tiebreaks
// ---------------------------------------------------------------------------

/// Which ranked tiebreak list applies. MTR and MSTR use genuinely different
/// tiebreak *axes*, not just different constants plugged into the same axes
/// (MSTR has no per-player game-win axis at all, since pods are single-game;
/// it adds an opponents'-average-match-points axis 1v1 does not have), so this
/// is a selected order rather than one parameterized list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TiebreakOrder {
    /// MTR §3.1: match points, opponents' match-win %, game-win %, opponents'
    /// game-win %.
    HeadToHead,
    /// MSTR: match points, match-win % (bye-adjusted), opponents' average
    /// match points, opponents' match-win %.
    Multiplayer,
}

impl TiebreakOrder {
    pub fn for_arity(arity: MatchArity) -> Self {
        if arity == MatchArity::HEAD_TO_HEAD {
            Self::HeadToHead
        } else {
            Self::Multiplayer
        }
    }
}

/// The computed tiebreak axes for one player, in the order they rank. Modelled
/// as an enum rather than a positional `Vec<f64>` so each axis keeps its name
/// and the variant matches the [`TiebreakOrder`] that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Tiebreaks {
    HeadToHead {
        opponents_match_win_pct: f64,
        game_win_pct: f64,
        opponents_game_win_pct: f64,
    },
    Multiplayer {
        match_win_pct: f64,
        opponents_avg_match_points: f64,
        opponents_match_win_pct: f64,
    },
}

impl Tiebreaks {
    /// Ranked comparison keys, most significant first.
    pub fn keys(&self) -> [f64; 3] {
        match *self {
            Tiebreaks::HeadToHead {
                opponents_match_win_pct,
                game_win_pct,
                opponents_game_win_pct,
            } => [
                opponents_match_win_pct,
                game_win_pct,
                opponents_game_win_pct,
            ],
            Tiebreaks::Multiplayer {
                match_win_pct,
                opponents_avg_match_points,
                opponents_match_win_pct,
            } => [
                match_win_pct,
                opponents_avg_match_points,
                opponents_match_win_pct,
            ],
        }
    }
}

/// One row of the computed standings. Every field is derived fresh from the
/// pairing history; nothing here is stored on the tournament.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TournamentStanding {
    pub player_key: String,
    pub display_name: String,
    pub dropped: bool,
    pub match_points: u32,
    /// Resolved pairings with real opponents. Byes are excluded (they are
    /// counted separately) so the MSTR match-win-percentage formula's
    /// denominator is right.
    pub matches_played: u32,
    pub byes: u32,
    pub tiebreaks: Tiebreaks,
}

/// Raw per-player totals scanned out of the pairing history.
#[derive(Debug, Clone, Default)]
struct PlayerRecord {
    match_points: u32,
    matches_played: u32,
    byes: u32,
    game_wins: u32,
    games_played: u32,
    /// One entry per co-seated opponent per resolved pairing — a multiset, so
    /// opponent averages are per-match the way MTR computes them. Byes
    /// contribute nothing (there was no real opponent that round).
    opponents: Vec<String>,
}

fn player_records(meta: &TournamentMeta) -> HashMap<String, PlayerRecord> {
    let scoring = meta.scoring;
    let mut records: HashMap<String, PlayerRecord> = meta
        .players
        .iter()
        .map(|p| (p.player_key.clone(), PlayerRecord::default()))
        .collect();

    for pairing in &meta.pairings {
        let Some(outcome) = &pairing.outcome else {
            continue;
        };
        for key in &pairing.players {
            let Some(record) = records.get_mut(key.as_str()) else {
                continue;
            };
            match outcome {
                PairingOutcome::Bye => {
                    record.match_points += u32::from(scoring.win_points());
                    record.byes += 1;
                    // MTR Appendix C: a bye is recorded as a 2-0 win for the
                    // player's own game-win percentage.
                    record.game_wins += 2;
                    record.games_played += 2;
                }
                PairingOutcome::Forfeit { winner } => {
                    record.matches_played += 1;
                    record.match_points += u32::from(if winner == key {
                        scoring.win_points()
                    } else {
                        scoring.loss_points()
                    });
                }
                PairingOutcome::Reported(PodOutcome::Draw) => {
                    record.matches_played += 1;
                    record.match_points += u32::from(scoring.draw_points());
                }
                PairingOutcome::Reported(PodOutcome::Decisive { winner, game_wins }) => {
                    record.matches_played += 1;
                    record.match_points += u32::from(if winner == key {
                        scoring.win_points()
                    } else {
                        scoring.loss_points()
                    });
                    if !game_wins.is_empty() {
                        record.game_wins += u32::from(game_wins.get(key).copied().unwrap_or(0));
                        record.games_played +=
                            game_wins.values().map(|w| u32::from(*w)).sum::<u32>();
                    }
                }
            }
            if pairing.players.len() > 1 {
                record.opponents.extend(
                    pairing
                        .players
                        .iter()
                        .filter(|other| other.as_str() != key.as_str())
                        .cloned(),
                );
            }
        }
    }
    records
}

/// MSTR: `(match points - byes * points-per-win) / (matches played *
/// points-per-win)`, floored at `1 / points-per-win`. A player with no
/// non-bye matches yet sits exactly on the floor rather than producing a
/// division by zero.
fn match_win_pct(record: &PlayerRecord, scoring: &ScoringPolicy) -> f64 {
    let floor = scoring.tiebreak_floor();
    if record.matches_played == 0 {
        return floor;
    }
    let per_win = f64::from(scoring.win_points());
    let earned = f64::from(record.match_points) - f64::from(record.byes) * per_win;
    let possible = f64::from(record.matches_played) * per_win;
    (earned / possible).max(floor)
}

/// MTR §3.1 game-win percentage, sharing the same `1 / win_points` floor.
fn game_win_pct(record: &PlayerRecord, scoring: &ScoringPolicy) -> f64 {
    let floor = scoring.tiebreak_floor();
    if record.games_played == 0 {
        return floor;
    }
    (f64::from(record.game_wins) / f64::from(record.games_played)).max(floor)
}

/// Average of `value` over this player's opponents, `empty` when they have
/// none yet.
fn opponents_average(
    record: &PlayerRecord,
    records: &HashMap<String, PlayerRecord>,
    empty: f64,
    value: impl Fn(&PlayerRecord) -> f64,
) -> f64 {
    let mut total = 0.0;
    let mut count = 0u32;
    for opponent in record.opponents.iter().filter_map(|key| records.get(key)) {
        total += value(opponent);
        count += 1;
    }
    if count == 0 {
        empty
    } else {
        total / f64::from(count)
    }
}

fn compare_keys(left: &[f64; 3], right: &[f64; 3]) -> Ordering {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| a.partial_cmp(b).unwrap_or(Ordering::Equal))
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

impl TournamentMeta {
    /// Full standings, best first. Sorted by match points, then by the
    /// arity-selected tiebreak axes in order, then by `player_key` so the
    /// result is deterministic (this core injects no randomness).
    pub fn standings(&self) -> Vec<TournamentStanding> {
        let records = player_records(self);
        let order = self.tiebreak_order();
        let scoring = self.scoring;
        let floor = scoring.tiebreak_floor();

        let mut rows: Vec<TournamentStanding> = self
            .players
            .iter()
            .map(|player| {
                let record = &records[&player.player_key];
                let tiebreaks = match order {
                    TiebreakOrder::HeadToHead => Tiebreaks::HeadToHead {
                        opponents_match_win_pct: opponents_average(record, &records, floor, |r| {
                            match_win_pct(r, &scoring)
                        }),
                        game_win_pct: game_win_pct(record, &scoring),
                        opponents_game_win_pct: opponents_average(record, &records, floor, |r| {
                            game_win_pct(r, &scoring)
                        }),
                    },
                    TiebreakOrder::Multiplayer => Tiebreaks::Multiplayer {
                        match_win_pct: match_win_pct(record, &scoring),
                        opponents_avg_match_points: opponents_average(record, &records, 0.0, |r| {
                            f64::from(r.match_points)
                        }),
                        opponents_match_win_pct: opponents_average(record, &records, floor, |r| {
                            match_win_pct(r, &scoring)
                        }),
                    },
                };
                TournamentStanding {
                    player_key: player.player_key.clone(),
                    display_name: player.display_name.clone(),
                    dropped: player.dropped,
                    match_points: record.match_points,
                    matches_played: record.matches_played,
                    byes: record.byes,
                    tiebreaks,
                }
            })
            .collect();

        rows.sort_by(|a, b| {
            b.match_points
                .cmp(&a.match_points)
                .then_with(|| compare_keys(&b.tiebreaks.keys(), &a.tiebreaks.keys()))
                .then_with(|| a.player_key.cmp(&b.player_key))
        });
        rows
    }
}

// ---------------------------------------------------------------------------
// Match result validation
// ---------------------------------------------------------------------------

/// Validates a client-or-organizer-reported result against the pairing it
/// targets. [`PairingOutcome::Bye`] and [`PairingOutcome::Forfeit`] are
/// server-assigned and never reach this function: the reporting path only ever
/// carries a [`PodOutcome`].
pub fn validate_match_result(
    pairing: &TournamentPairing,
    result: &PodOutcome,
    players: &[TournamentPlayer],
) -> Result<(), String> {
    match result {
        // MSTR: all seated players draw together.
        PodOutcome::Draw => Ok(()),
        PodOutcome::Decisive { winner, game_wins } => {
            if !pairing.players.contains(winner) {
                return Err("Winner must be one of the pod's players".to_string());
            }
            // A dropped player can never be credited a win reported after
            // their drop. `pairing.players` keeps its original seat list (a
            // drop does not retroactively rewrite history), so membership
            // alone does not close this gap.
            if players.iter().any(|p| p.player_key == *winner && p.dropped) {
                return Err(format!(
                    "Winner {winner} has dropped and cannot be credited a win"
                ));
            }
            if pairing.players.len() == 2 {
                // HEAD_TO_HEAD: require exactly the two participant keys with
                // a legal completed-Bo3 tally. An empty or single-key map is a
                // hard rejection, not a silently-skipped check.
                let (a, b) = (&pairing.players[0], &pairing.players[1]);
                if game_wins.len() != 2 || !game_wins.contains_key(a) || !game_wins.contains_key(b)
                {
                    return Err(
                        "Head-to-head result must report game wins for exactly both players"
                            .to_string(),
                    );
                }
                let (wa, wb) = (game_wins[a], game_wins[b]);
                // Legal completed best-of-three tallies only: someone reaches
                // 2, the other has 0 or 1. Rejects 0-0/1-0 (unfinished), 2-2,
                // 3-anything.
                if !matches!((wa, wb), (2, 0) | (2, 1) | (0, 2) | (1, 2)) {
                    return Err(format!("Illegal Bo3 game-win tally {wa}-{wb}"));
                }
                let expected = if wa > wb { a } else { b };
                if winner != expected {
                    return Err("Winner must match the player with more game wins".to_string());
                }
            } else if !game_wins.is_empty() {
                // Pod (arity > 2): MSTR pods are single-game, so a client
                // attaching game-win data has no value for it to mean.
                return Err(
                    "Pod results are single-game per MSTR - game_wins must be empty".to_string(),
                );
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Round construction
// ---------------------------------------------------------------------------

/// How one round's active players split into pods, short pods and byes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundPartition {
    /// Pods seating a full `arity` players.
    pub full_pods: usize,
    /// Pods seating `arity - 1` players. Always 0 at `HEAD_TO_HEAD`, where an
    /// `arity - 1` pod is a bye.
    pub short_pods: usize,
    /// One-player pairings, resolved as [`PairingOutcome::Bye`] immediately.
    pub byes: usize,
}

/// Splits `n` active players into pods for one round.
///
/// The general partition: find the smallest `b` such that
/// `n - b * (arity - 1)` is a non-negative multiple of `arity`; `b` is the
/// number of short pods and the quotient is the number of full pods.
/// Minimising `b` first is what makes 9 players at arity 4 resolve to `3+3+3`
/// (`b = 3`, since `b = 0,1,2` all fail divisibility) and 10 resolve to
/// `4+3+3` (`b = 2`) rather than an arbitrarily larger number of short pods.
///
/// `arity` and `arity - 1` are consecutive integers and therefore coprime, so
/// a solution exists for every sufficiently large `n`; the small counts with
/// no all-`{arity-1, arity}` partition (at arity 4: `n` in `{1, 2, 5}`) are
/// resolved by the *outer* search, which hands the minimum number of players a
/// bye and re-runs the partition on the rest. That reproduces the design's
/// explicit degenerate table without hardcoding it: `n = 1` -> one bye,
/// `n = 2` -> two byes (the one accepted multiple-bye exception, since no pod
/// can form), `n = 5` -> one full pod plus one bye.
///
/// At `HEAD_TO_HEAD` the same search degenerates correctly on its own: short
/// pods are one-player pods, i.e. byes, so they are reported as `byes` here.
pub fn partition_round(n: usize, arity: MatchArity) -> RoundPartition {
    let seats = usize::from(arity.get());
    let short_size = usize::from(arity.short_pod_size());

    for byes in 0..=n {
        let seated = n - byes;
        for short_pods in 0..seats {
            let Some(remainder) = seated.checked_sub(short_pods * short_size) else {
                break;
            };
            if remainder % seats == 0 {
                let full_pods = remainder / seats;
                // At arity 2 a "short pod" seats one player, which is a bye.
                return if short_size < 2 {
                    RoundPartition {
                        full_pods,
                        short_pods: 0,
                        byes: byes + short_pods,
                    }
                } else {
                    RoundPartition {
                        full_pods,
                        short_pods,
                        byes,
                    }
                };
            }
        }
    }
    // Unreachable: `byes == n` leaves 0 seated, which is a multiple of every
    // pod size. Kept as data rather than a panic so a future arity change
    // cannot turn a seating edge case into a crash in the broker core.
    RoundPartition {
        full_pods: 0,
        short_pods: 0,
        byes: n,
    }
}

/// Every ordered "has already faced" relation in the pairing history.
fn faced_map(history: &[TournamentPairing]) -> HashMap<&str, HashSet<&str>> {
    let mut faced: HashMap<&str, HashSet<&str>> = HashMap::new();
    for pairing in history.iter().filter(|p| p.players.len() > 1) {
        for seat in &pairing.players {
            let entry = faced.entry(seat.as_str()).or_default();
            entry.extend(
                pairing
                    .players
                    .iter()
                    .filter(|other| other.as_str() != seat.as_str())
                    .map(String::as_str),
            );
        }
    }
    faced
}

fn have_faced(faced: &HashMap<&str, HashSet<&str>>, a: &str, b: &str) -> bool {
    faced.get(a).is_some_and(|seen| seen.contains(b))
}

/// Number of already-played pairs inside one pod — the quantity the
/// swap-repair pass drives down.
fn pod_conflicts(pod: &[&str], faced: &HashMap<&str, HashSet<&str>>) -> usize {
    let mut conflicts = 0;
    for (i, left) in pod.iter().enumerate() {
        for right in pod.iter().skip(i + 1) {
            if have_faced(faced, left, right) {
                conflicts += 1;
            }
        }
    }
    conflicts
}

/// Removes `count` players from `pool`, preferring — from the bottom of the
/// standings upward — those for whom `already` is false.
///
/// This is the shared fairness selector behind both "who takes a bye" and
/// "who sits in a short pod": MTR sends a bye to the lowest-standing eligible
/// player, and MSTR asks that a player be shorted at most once per event. The
/// returned players keep standings order.
fn select_fair(
    pool: &mut Vec<String>,
    count: usize,
    already: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut chosen: Vec<usize> = Vec::with_capacity(count);
    for (index, key) in pool.iter().enumerate().rev() {
        if chosen.len() == count {
            break;
        }
        if !already(key) {
            chosen.push(index);
        }
    }
    // Not enough never-shorted players: fill the rest from the bottom.
    if chosen.len() < count {
        for index in (0..pool.len()).rev() {
            if chosen.len() == count {
                break;
            }
            if !chosen.contains(&index) {
                chosen.push(index);
            }
        }
    }
    chosen.sort_unstable();
    let mut picked: Vec<String> = chosen
        .iter()
        .rev()
        .map(|index| pool.remove(*index))
        .collect();
    picked.reverse();
    picked
}

/// Greedy top-to-bottom assignment into pods of the given sizes, skipping any
/// candidate that would create a repeated *pair* (not merely a repeated full
/// pod — two players who have met before still count as a rematch even if the
/// rest of the pod is new).
fn greedy_assign<'a>(
    order: &[&'a str],
    sizes: &[usize],
    faced: &HashMap<&str, HashSet<&str>>,
) -> Vec<Vec<&'a str>> {
    let mut remaining: Vec<&'a str> = order.to_vec();
    let mut pods: Vec<Vec<&'a str>> = Vec::with_capacity(sizes.len());
    for &size in sizes {
        let mut pod: Vec<&'a str> = Vec::with_capacity(size);
        while pod.len() < size && !remaining.is_empty() {
            let index = if pod.is_empty() {
                0
            } else {
                remaining
                    .iter()
                    .position(|candidate| {
                        pod.iter()
                            .all(|seated| !have_faced(faced, seated, candidate))
                    })
                    // Every remaining candidate is a rematch for this pod;
                    // seat the highest-standing one and let the repair pass
                    // below try to trade it away.
                    .unwrap_or(0)
            };
            pod.push(remaining.remove(index));
        }
        pods.push(pod);
    }
    pods
}

/// MSTR's repair step: iteratively swap a player out of a pod carrying a
/// rematch with a player from another pod, keeping any swap that strictly
/// lowers the total number of repeated pairs.
///
/// Pod sizes are preserved (a swap is a straight exchange), and every accepted
/// swap strictly decreases a non-negative integer total, so this terminates.
/// Swaps against pods both above and below are considered: a rematch forced
/// into the *last* pod by the greedy pass can only be repaired upward.
fn repair_rematches(pods: &mut [Vec<&str>], faced: &HashMap<&str, HashSet<&str>>) {
    loop {
        let mut improved = false;
        'search: for i in 0..pods.len() {
            if pod_conflicts(&pods[i], faced) == 0 {
                continue;
            }
            for j in 0..pods.len() {
                if i == j {
                    continue;
                }
                let before = pod_conflicts(&pods[i], faced) + pod_conflicts(&pods[j], faced);
                for x in 0..pods[i].len() {
                    for y in 0..pods[j].len() {
                        let moved_out = pods[i][x];
                        let moved_in = pods[j][y];
                        pods[i][x] = moved_in;
                        pods[j][y] = moved_out;
                        let after = pod_conflicts(&pods[i], faced) + pod_conflicts(&pods[j], faced);
                        if after < before {
                            improved = true;
                            break 'search;
                        }
                        pods[i][x] = moved_out;
                        pods[j][y] = moved_in;
                    }
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// Builds one Swiss round from a standings-ordered active player list.
///
/// A free function, deliberately: it is the whole pairing algorithm and is
/// unit-testable in isolation from any [`TournamentManager`] mutation. Steps,
/// in order:
///
/// 1. Partition the field into full pods, short pods and byes
///    ([`partition_round`]).
/// 2. Choose the bye recipients, preferring players who have not had one
///    ([`had_bye`]), from the bottom of the standings.
/// 3. Choose the short-pod occupants, preferring players who have not been
///    shorted ([`had_short_pod`]), from the bottom of what is left — spread
///    across however many short pods the round actually needs, not just one.
/// 4. Greedily assign the remainder top-to-bottom into pods, rejecting any
///    repeated pair.
/// 5. Repair any rematch the greedy pass could not avoid by swapping with
///    another pod.
///
/// Byes are emitted already resolved as [`PairingOutcome::Bye`] — there is
/// nothing to report for them.
pub fn build_swiss_round(
    standings_order: &[String],
    arity: MatchArity,
    history: &[TournamentPairing],
    round: u32,
    first_id: PairingId,
) -> Vec<TournamentPairing> {
    let partition = partition_round(standings_order.len(), arity);
    let seats = usize::from(arity.get());
    let short_size = usize::from(arity.short_pod_size());

    let mut pool: Vec<String> = standings_order.to_vec();
    let bye_players = select_fair(&mut pool, partition.byes, |key| had_bye(key, history));
    let short_players = select_fair(&mut pool, partition.short_pods * short_size, |key| {
        had_short_pod(key, arity, history)
    });

    // Full pods take the top of the standings; the short pods sit at the
    // bottom, which is where the fairness selector just placed their
    // occupants.
    let mut order: Vec<&str> = pool.iter().map(String::as_str).collect();
    order.extend(short_players.iter().map(String::as_str));
    let mut sizes = vec![seats; partition.full_pods];
    sizes.resize(partition.full_pods + partition.short_pods, short_size);

    let faced = faced_map(history);
    let mut pods = greedy_assign(&order, &sizes, &faced);
    repair_rematches(&mut pods, &faced);

    let mut next_id = first_id;
    let mut pairings: Vec<TournamentPairing> = pods
        .into_iter()
        .map(|pod| {
            let pairing = TournamentPairing {
                id: next_id,
                round,
                players: pod.into_iter().map(str::to_string).collect(),
                outcome: None,
            };
            next_id += 1;
            pairing
        })
        .collect();

    for player in bye_players {
        pairings.push(TournamentPairing {
            id: next_id,
            round,
            players: vec![player],
            outcome: Some(PairingOutcome::Bye),
        });
        next_id += 1;
    }
    pairings
}

/// Smallest field [`BracketShape::SingleElimination`] accepts. MTR Appendix E
/// documents the 4-8 player single-elimination cut; 2 and 3 are the degenerate
/// finals and semifinal-plus-bye fields the same seeded formula already covers
/// with no extra machinery, and [`MatchArity::HEAD_TO_HEAD`] is itself 2, so a
/// two-player "bracket" is simply a finals match with nothing to reject.
pub const SINGLE_ELIMINATION_MIN_PLAYERS: usize = 2;

/// Largest field [`BracketShape::SingleElimination`] accepts — the top of MTR
/// Appendix E's single-elimination cut. Larger fields run Swiss, whose
/// round-count default (`default_total_rounds`) covers 9 players upward.
pub const SINGLE_ELIMINATION_MAX_PLAYERS: usize = 8;

/// Builds one single-elimination round. Head-to-head only (enforced at
/// construction), for any field from [`SINGLE_ELIMINATION_MIN_PLAYERS`] to
/// [`SINGLE_ELIMINATION_MAX_PLAYERS`] — the whole contiguous MTR Appendix E
/// range, not only the power-of-two counts.
///
/// Round 1 rounds the field up to the next power of two and applies the same
/// standard `i` versus `slots - 1 - i` seeding the power-of-two case already
/// used; the slots past the end of the field are empty, so a seat drawn
/// against one is emitted as an already-resolved [`PairingOutcome::Bye`] —
/// the same bye representation the Swiss path uses, not a second one. Because
/// the empty slots sit at the *bottom* of the seeding, the byes land on the
/// *top* seeds. That is the defining property of a seeded bracket (the best
/// finishers get the shortest path) and is deliberately the opposite of the
/// Swiss bye rule in [`build_swiss_round`], where a bye is a free win handed
/// to the bottom of the standings precisely to keep it away from the leader.
///
/// Later rounds pair the winners of adjacent prior-round matches. A bye's
/// winner is its single occupant ([`TournamentPairing::winner`]), so a bye
/// recipient advances through exactly the same path as a match winner with no
/// special case. A prior round that is unreported or drawn has no winner to
/// advance, which is an error rather than a silently dropped seat.
pub fn build_single_elimination_round(
    standings_order: &[String],
    history: &[TournamentPairing],
    round: u32,
    first_id: PairingId,
) -> Result<Vec<TournamentPairing>, String> {
    let pods: Vec<Vec<String>> = if round <= 1 {
        let n = standings_order.len();
        if !(SINGLE_ELIMINATION_MIN_PLAYERS..=SINGLE_ELIMINATION_MAX_PLAYERS).contains(&n) {
            return Err(format!(
                "Single-elimination brackets support {SINGLE_ELIMINATION_MIN_PLAYERS}-{SINGLE_ELIMINATION_MAX_PLAYERS} players (MTR Appendix E's 4-8 cut, plus the degenerate 2- and 3-player finals); got {n}"
            ));
        }
        let slots = n.next_power_of_two();
        (0..slots / 2)
            .map(|i| match standings_order.get(slots - 1 - i) {
                Some(lower_seed) => vec![standings_order[i].clone(), lower_seed.clone()],
                // The opposing slot is past the end of the field: this seat
                // draws a bye rather than an opponent.
                None => vec![standings_order[i].clone()],
            })
            .collect()
    } else {
        let previous: Vec<&TournamentPairing> =
            history.iter().filter(|p| p.round == round - 1).collect();
        if previous.len() < 2 {
            return Err(
                "Single-elimination bracket is already decided - no further round to pair"
                    .to_string(),
            );
        }
        let mut winners = Vec::with_capacity(previous.len());
        for pairing in previous {
            let winner = pairing.winner().ok_or_else(|| {
                format!(
                    "Pairing {} has no winner yet - single elimination cannot advance",
                    pairing.id
                )
            })?;
            winners.push(winner.to_string());
        }
        winners.chunks(2).map(<[String]>::to_vec).collect()
    };

    Ok(pods
        .into_iter()
        .enumerate()
        .map(|(offset, players)| {
            // A one-seat pairing is a bye and nothing else — the same
            // predicate `had_bye` derives from, so there is no second
            // representation to keep in sync. Only round 1 can produce one:
            // every later round pairs a power-of-two count of winners.
            let outcome = (players.len() == 1).then_some(PairingOutcome::Bye);
            TournamentPairing {
                id: first_id + offset as PairingId,
                round,
                players,
                outcome,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// TournamentManager
// ---------------------------------------------------------------------------

/// What one [`TournamentManager::check_expired`] sweep did to a tournament.
///
/// Plain domain data, deliberately: `Outbound`/`LobbyServerMessage` live in
/// `broker.rs`/`protocol.rs`, and wrapping these into them is
/// `Broker::reap_expired`'s job (PR 2) — exactly the split that already exists
/// between [`crate::lobby::LobbyManager::check_expired`]'s plain `Vec<String>`
/// and the broker's `LobbyGameRemoved` fan-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TournamentExpiryEvent {
    /// The record was removed: a stale `Registration`, or a terminal
    /// tournament past its retention window.
    Deleted(String),
    /// An `InProgress` tournament went inactive and is now
    /// [`TournamentStatus::Abandoned`]. The record and its full pairing and
    /// standings history are preserved.
    Abandoned(String),
}

/// The pure registry of tournaments, mirroring
/// [`crate::lobby::LobbyManager`]'s manager-owns-state shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TournamentManager {
    tournaments: HashMap<String, TournamentMeta>,
}

impl TournamentManager {
    pub fn new() -> Self {
        Self {
            tournaments: HashMap::new(),
        }
    }

    pub fn get(&self, code: &str) -> Option<&TournamentMeta> {
        self.tournaments.get(code)
    }

    pub fn len(&self) -> usize {
        self.tournaments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tournaments.is_empty()
    }

    /// Every tournament this manager holds, in unspecified order.
    ///
    /// Deliberately unfiltered: it yields terminal and abandoned events
    /// alongside live ones, because "which tournaments are interesting" is a
    /// caller's question and differs per call site. Callers that need a stable
    /// order (e.g. building a `TournamentListUpdate`) sort the result
    /// themselves — this makes no ordering promise, matching
    /// [`std::collections::HashMap::values`]'s own contract, which it wraps
    /// directly.
    ///
    /// Returns an opaque iterator rather than a concrete `Values` or a
    /// materialized `Vec` so the storage behind it stays an implementation
    /// detail, the same way [`Self::get`]/[`Self::len`] keep the map itself
    /// private.
    pub fn iter(&self) -> impl Iterator<Item = &TournamentMeta> {
        self.tournaments.values()
    }

    fn meta_mut(&mut self, code: &str) -> Result<&mut TournamentMeta, String> {
        self.tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))
    }

    /// Creates a tournament and returns the minted `organizer_token`.
    ///
    /// The `code` is caller-supplied, mirroring
    /// [`crate::lobby::LobbyManager::register_game`] exactly rather than
    /// minting one internally. Unlike `register_game`, a duplicate code is
    /// rejected instead of overwriting: silently replacing a tournament would
    /// discard its pairing history.
    pub fn create_tournament(
        &mut self,
        code: &str,
        req: CreateTournamentRequest,
        env: &impl BrokerEnv,
    ) -> Result<String, String> {
        if self.tournaments.contains_key(code) {
            return Err(format!("Tournament code already in use: {code}"));
        }
        // v1 ships single elimination for head-to-head only: bracket and
        // advancement semantics for a multi-player pod bracket are an
        // unresolved design question, and rejecting explicitly is better than
        // silently dropping the request's intent.
        if req.bracket == BracketShape::SingleElimination && req.arity != MatchArity::HEAD_TO_HEAD {
            return Err(format!(
                "Single-elimination brackets are head-to-head only in v1; got arity {}",
                req.arity.get()
            ));
        }
        if req.total_rounds == Some(0) {
            return Err("total_rounds override must be at least 1".to_string());
        }
        let now = env.now_ms() / 1000;
        let organizer_token = env.new_token();
        self.tournaments.insert(
            code.to_string(),
            TournamentMeta {
                code: code.to_string(),
                name: req.name,
                organizer_token: organizer_token.clone(),
                arity: req.arity,
                scoring: req.scoring,
                bracket: req.bracket,
                total_rounds_override: req.total_rounds,
                // Nothing is latched until round 1 is paired: no round has
                // been scheduled yet, and the field is still empty.
                resolved_total_rounds: None,
                current_round: 0,
                status: TournamentStatus::Registration,
                players: Vec::new(),
                pairings: Vec::new(),
                created_at: now,
                last_activity_at: now,
            },
        );
        Ok(organizer_token)
    }

    /// Registers a player and returns the minted `player_token`. Joins are
    /// accepted only while the tournament is still in `Registration`: a field
    /// that grows after pairings exist would invalidate the round count and
    /// the standings already computed against it.
    pub fn join_tournament(
        &mut self,
        code: &str,
        player_key: &str,
        display_name: &str,
        env: &impl BrokerEnv,
    ) -> Result<String, String> {
        let now = env.now_ms() / 1000;
        let token = env.new_token();
        let meta = self.meta_mut(code)?;
        if meta.status != TournamentStatus::Registration {
            return Err(format!(
                "Tournament {code} is no longer accepting entries (status {:?})",
                meta.status
            ));
        }
        if meta.player(player_key).is_some() {
            return Err(format!("Player {player_key} has already joined {code}"));
        }
        meta.players.push(TournamentPlayer {
            player_key: player_key.to_string(),
            player_token: token.clone(),
            display_name: display_name.to_string(),
            dropped: false,
        });
        meta.last_activity_at = now;
        Ok(token)
    }

    /// Generates the next round's pairings and returns their ids.
    ///
    /// Standings order is recomputed fresh from the pairing history on every
    /// call, so a correction to an earlier round is reflected in the next
    /// round's seeding without any cache to invalidate.
    ///
    /// Refuses to pair a new round while any pairing is still unresolved (see
    /// [`TournamentMeta::first_unresolved_pairing`]) — the "a round is
    /// finished before the next one is paired" invariant is enforced here
    /// rather than merely assumed by the seeding that depends on it.
    ///
    /// Also refuses to pair past [`TournamentMeta::total_rounds`]. The
    /// asymmetry with [`Self::complete_tournament`] — which is deliberately
    /// *not* gated on the round count — is intentional and runs in one
    /// direction only: an organizer may end an event *early*, but the
    /// scheduled length is a ceiling, not a suggestion. Without this guard an
    /// event created with `total_rounds: Some(1)` could settle round 1 and
    /// then pair round 2, 3, ... indefinitely, which would make both the
    /// override and the advertised default ([`default_total_rounds`]) purely
    /// decorative.
    ///
    /// Because that ceiling binds, this is also where an unset default stops
    /// being live: pairing round 1 latches the computed count into
    /// [`TournamentMeta::resolved_total_rounds`]. The default is keyed on the
    /// *active* player count, so without the latch a mid-event drop would
    /// recompute it downward and this very guard would refuse a round the
    /// bracket still owes — a 3-player single-elimination event losing its
    /// final being the sharp case.
    ///
    /// The three guards are ordered most- to least-permanent, so the first
    /// error a caller sees is the one that actually describes their
    /// situation: terminal status (the tournament is over), then the round
    /// ceiling (no further round is scheduled, and settling pairings will
    /// never change that), then an unresolved pairing (transient — report it
    /// and retry).
    pub fn generate_pairings(
        &mut self,
        code: &str,
        env: &impl BrokerEnv,
    ) -> Result<Vec<PairingId>, String> {
        let now = env.now_ms() / 1000;
        let meta = self.meta_mut(code)?;
        if meta.status.is_terminal() {
            return Err(format!(
                "Tournament {code} is no longer running (status {:?})",
                meta.status
            ));
        }
        // The scheduled length is a ceiling. `total_rounds()` is the single
        // authority for it — the organizer's override if set, else the
        // computed default latched when round 1 was paired, else (before
        // round 1 exists) that default resolved against the live field — so
        // this guard cannot drift from the count clients are shown or from
        // the one `complete_tournament`'s rationale refers to.
        //
        // A `total_rounds()` of 0 is a single-elimination field below
        // `SINGLE_ELIMINATION_MIN_PLAYERS`: "there is no bracket to run", per
        // `default_total_rounds`. Refusing it here agrees with
        // `build_single_elimination_round`, which rejects the same field a few
        // lines below; this guard simply reaches it first.
        let total_rounds = meta.total_rounds();
        if meta.current_round >= total_rounds {
            return Err(format!(
                "Tournament {code} cannot pair round {} - it is scheduled for {total_rounds} round(s) and is already at round {}",
                meta.current_round + 1,
                meta.current_round
            ));
        }
        if let Some(pending) = meta.first_unresolved_pairing() {
            return Err(format!(
                "Tournament {code} cannot pair round {} - pairing {} in round {} has no result yet",
                meta.current_round + 1,
                pending.id,
                pending.round
            ));
        }
        let round = meta.current_round + 1;
        let first_id = meta
            .pairings
            .iter()
            .map(|p| p.id)
            .max()
            .map_or(0, |m| m + 1);
        let order: Vec<String> = meta
            .standings()
            .into_iter()
            .filter(|row| !row.dropped)
            .map(|row| row.player_key)
            .collect();

        let generated = match meta.bracket {
            BracketShape::Swiss => {
                build_swiss_round(&order, meta.arity, &meta.pairings, round, first_id)
            }
            BracketShape::SingleElimination => {
                build_single_elimination_round(&order, &meta.pairings, round, first_id)?
            }
        };

        let ids: Vec<PairingId> = generated.iter().map(|p| p.id).collect();
        meta.pairings.extend(generated);
        meta.current_round = round;
        // Latch the computed default the moment round 1 exists, so a later
        // drop cannot shrink the schedule out from under the ceiling above
        // (see [`TournamentMeta::resolved_total_rounds`]). `total_rounds` is
        // the very value this call was authorized against, so the latch and
        // the guard can never disagree about what round 1 was scheduled for.
        //
        // The `is_none()` test is what makes this idempotent, not `round == 1`
        // on its own: the round test states *when* the schedule is fixed, and
        // the `is_none()` test guarantees an already-latched one is never
        // overwritten by a later call. With an override in play there is
        // nothing to latch — the override already wins ahead of this tier and
        // is fixed by the organizer, not derived from the field.
        if round == 1
            && meta.total_rounds_override.is_none()
            && meta.resolved_total_rounds.is_none()
        {
            meta.resolved_total_rounds = Some(total_rounds);
        }
        meta.status = TournamentStatus::InProgress;
        meta.last_activity_at = now;
        Ok(ids)
    }

    /// Records a reported result for one pairing.
    ///
    /// Replay-safe by construction: this is a single `outcome` write, so a
    /// correction simply overwrites the prior value and every derived view
    /// (standings, opponent history) recomputes from the corrected history on
    /// its next read. Reporting the identical outcome twice is therefore a
    /// no-op rather than an error.
    ///
    /// A [`PairingOutcome::Bye`] or [`PairingOutcome::Forfeit`] is
    /// server-assigned and permanent: a client report cannot overwrite one.
    pub fn report_result(
        &mut self,
        code: &str,
        pairing_id: PairingId,
        outcome: PodOutcome,
        env: &impl BrokerEnv,
    ) -> Result<(), String> {
        let now = env.now_ms() / 1000;
        let meta = self.meta_mut(code)?;
        if meta.status.is_terminal() {
            return Err(format!(
                "Tournament {code} is no longer running (status {:?})",
                meta.status
            ));
        }
        let index = meta
            .pairings
            .iter()
            .position(|p| p.id == pairing_id)
            .ok_or_else(|| format!("Pairing {pairing_id} not found in {code}"))?;
        match meta.pairings[index].outcome {
            Some(PairingOutcome::Bye) => {
                return Err(format!(
                    "Pairing {pairing_id} is a bye and has no result to report"
                ))
            }
            Some(PairingOutcome::Forfeit { .. }) => {
                return Err(format!(
                    "Pairing {pairing_id} was resolved by forfeit and cannot be reported"
                ))
            }
            Some(PairingOutcome::Reported(_)) | None => {}
        }
        validate_match_result(&meta.pairings[index], &outcome, &meta.players)?;
        meta.pairings[index].outcome = Some(PairingOutcome::Reported(outcome));
        meta.last_activity_at = now;
        Ok(())
    }

    /// Marks a player dropped and auto-settles any pending pairing the drop
    /// leaves with exactly one active player.
    ///
    /// * Head-to-head: the remaining player is awarded
    ///   [`PairingOutcome::Forfeit`] immediately.
    /// * Pod with two or more active players left: unaffected — the remaining
    ///   players still play it out and report a real result.
    /// * Pod reduced to exactly one active player: the same forfeit, the
    ///   head-to-head rule generalized.
    /// * A pairing that already has an outcome is never retroactively altered.
    ///
    /// A pairing every one of whose players has dropped is left pending: there
    /// is no active player to credit, and a dropped player must never be
    /// credited a win.
    ///
    /// The scan covers every *unresolved* pairing this player is seated in,
    /// not only [`TournamentMeta::current_round`]'s. Round advancement now
    /// refuses to leave an unresolved pairing behind
    /// ([`TournamentMeta::first_unresolved_pairing`]), so in practice only the
    /// current round can hold one — but the rule "a drop settles any pairing
    /// it reduces to a single active player" is unconditionally correct and
    /// does not need that invariant to hold in order to be right.
    ///
    /// A drop shrinks the active field but never the schedule: once round 1
    /// has been paired, [`TournamentMeta::total_rounds`] reads the count
    /// latched then ([`TournamentMeta::resolved_total_rounds`]), so the rounds
    /// the remaining players are still owed stay generatable.
    pub fn drop_player(
        &mut self,
        code: &str,
        player_key: &str,
        env: &impl BrokerEnv,
    ) -> Result<(), String> {
        let now = env.now_ms() / 1000;
        let meta = self.meta_mut(code)?;
        if meta.status.is_terminal() {
            return Err(format!(
                "Tournament {code} is no longer running (status {:?})",
                meta.status
            ));
        }
        let player = meta
            .players
            .iter_mut()
            .find(|p| p.player_key == player_key)
            .ok_or_else(|| format!("Player {player_key} is not in {code}"))?;
        player.dropped = true;

        let dropped: HashSet<&str> = meta
            .players
            .iter()
            .filter(|p| p.dropped)
            .map(|p| p.player_key.as_str())
            .collect();
        let mut forfeits: Vec<(usize, String)> = Vec::new();
        for (index, pairing) in meta.pairings.iter().enumerate() {
            if pairing.outcome.is_some() || !pairing.seats(player_key) {
                continue;
            }
            let mut active = pairing
                .players
                .iter()
                .filter(|key| !dropped.contains(key.as_str()));
            if let (Some(survivor), None) = (active.next(), active.next()) {
                forfeits.push((index, survivor.clone()));
            }
        }
        for (index, winner) in forfeits {
            meta.pairings[index].outcome = Some(PairingOutcome::Forfeit { winner });
        }
        meta.last_activity_at = now;
        Ok(())
    }

    /// Freezes a tournament as [`TournamentStatus::Completed`] — every
    /// pairing resolved, and the organizer choosing to stop here. Terminal:
    /// no further mutation is accepted, and the 30-day retention clock starts
    /// now.
    ///
    /// Refused while any pairing is still unresolved, through the same
    /// [`TournamentMeta::first_unresolved_pairing`] authority
    /// [`Self::generate_pairings`] uses: completing is terminal, and a
    /// tournament frozen with a pending pairing could never report it
    /// afterwards, leaving a permanently unfinished record whose final
    /// standings are wrong by exactly that match.
    ///
    /// Deliberately *not* gated on `current_round >= total_rounds()`. The
    /// design leaves round count an organizer-overridable default
    /// ([`default_total_rounds`], MTR Appendix E), so an organizer cutting an
    /// event short once the current round is settled — a concession, a
    /// venue closing, a top-cut decided early — is a legitimate call the
    /// organizer already owns, not an invariant violation. "Every pairing has
    /// a result" is the property that actually keeps the record honest.
    pub fn complete_tournament(&mut self, code: &str, env: &impl BrokerEnv) -> Result<(), String> {
        let now = env.now_ms() / 1000;
        let meta = self.meta_mut(code)?;
        if meta.status.is_terminal() {
            return Err(format!(
                "Tournament {code} is already finished (status {:?})",
                meta.status
            ));
        }
        if let Some(pending) = meta.first_unresolved_pairing() {
            return Err(format!(
                "Tournament {code} cannot be completed - pairing {} in round {} has no result yet",
                pending.id, pending.round
            ));
        }
        meta.status = TournamentStatus::Completed;
        meta.last_activity_at = now;
        Ok(())
    }

    /// One sweep implementing all three lifecycle rules, keyed off the same
    /// `last_activity_at` every mutation and status transition bumps:
    ///
    /// 1. `Registration` idle past [`REGISTRATION_TIMEOUT_SECS`] — deleted.
    /// 2. `InProgress` idle past [`IN_PROGRESS_ABANDON_SECS`] — transitioned
    ///    to `Abandoned`, record preserved.
    /// 3. `Completed`/`Abandoned` past [`TERMINAL_RETENTION_SECS`] — deleted.
    ///
    /// Returns plain domain data; see [`TournamentExpiryEvent`].
    pub fn check_expired(&mut self, env: &impl BrokerEnv) -> Vec<TournamentExpiryEvent> {
        let now = env.now_ms() / 1000;
        let mut events = Vec::new();
        self.tournaments.retain(|code, meta| {
            let idle = now.saturating_sub(meta.last_activity_at);
            match meta.status {
                TournamentStatus::Registration => {
                    if idle > REGISTRATION_TIMEOUT_SECS {
                        events.push(TournamentExpiryEvent::Deleted(code.clone()));
                        return false;
                    }
                }
                TournamentStatus::InProgress => {
                    if idle > IN_PROGRESS_ABANDON_SECS {
                        meta.status = TournamentStatus::Abandoned;
                        // The transition is itself a state change, so it bumps
                        // `last_activity_at` — which is what starts the 30-day
                        // retention clock from the moment of abandonment.
                        meta.last_activity_at = now;
                        events.push(TournamentExpiryEvent::Abandoned(code.clone()));
                    }
                }
                TournamentStatus::Completed | TournamentStatus::Abandoned => {
                    if idle > TERMINAL_RETENTION_SECS {
                        events.push(TournamentExpiryEvent::Deleted(code.clone()));
                        return false;
                    }
                }
            }
            true
        });
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Deterministic `BrokerEnv` for tests. `now_ms` is settable; tokens and
    /// codes are monotonic counters so assertions are stable. Same shape as
    /// `lobby.rs`'s and `broker.rs`'s own test fakes — this crate re-declares
    /// it per test module rather than sharing one.
    struct FakeEnv {
        now: Cell<u64>,
        token_counter: Cell<u64>,
        code_counter: Cell<u64>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                now: Cell::new(1_000_000),
                token_counter: Cell::new(0),
                code_counter: Cell::new(0),
            }
        }
        fn set_now_ms(&self, ms: u64) {
            self.now.set(ms);
        }
        fn advance_secs(&self, secs: u64) {
            self.now.set(self.now.get() + secs * 1000);
        }
    }

    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn new_token(&self) -> String {
            let n = self.token_counter.get();
            self.token_counter.set(n + 1);
            format!("token-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code_counter.get();
            self.code_counter.set(n + 1);
            format!("CODE{n:02}")
        }
    }

    // -- helpers ------------------------------------------------------------

    fn arity(n: u8) -> MatchArity {
        MatchArity::new(n).expect("test arity must be valid")
    }

    fn key(i: usize) -> String {
        format!("p{i:02}")
    }

    fn create(
        mgr: &mut TournamentManager,
        code: &str,
        a: MatchArity,
        bracket: BracketShape,
        env: &FakeEnv,
    ) {
        mgr.create_tournament(
            code,
            CreateTournamentRequest {
                name: "Test Event".to_string(),
                arity: a,
                scoring: ScoringPolicy::default_for_arity(a),
                bracket,
                total_rounds: None,
            },
            env,
        )
        .expect("create_tournament");
    }

    fn join_n(mgr: &mut TournamentManager, code: &str, n: usize, env: &FakeEnv) {
        for i in 0..n {
            mgr.join_tournament(code, &key(i), &format!("Player {i}"), env)
                .expect("join_tournament");
        }
    }

    /// Builds a Swiss tournament of `n` players at `a` seats per pairing.
    fn swiss(n: usize, a: u8, env: &FakeEnv) -> TournamentManager {
        let mut mgr = TournamentManager::new();
        create(&mut mgr, "T", arity(a), BracketShape::Swiss, env);
        join_n(&mut mgr, "T", n, env);
        mgr
    }

    /// A Swiss tournament of `n` players at `a` seats per pairing whose
    /// organizer set an explicit round-count override, so `total_rounds()`
    /// resolves through [`TournamentMeta::total_rounds_override`] instead of
    /// [`default_total_rounds`].
    fn swiss_capped(n: usize, a: u8, total_rounds: u32, env: &FakeEnv) -> TournamentManager {
        let mut mgr = TournamentManager::new();
        let a = arity(a);
        mgr.create_tournament(
            "T",
            CreateTournamentRequest {
                name: "Test Event".to_string(),
                arity: a,
                scoring: ScoringPolicy::default_for_arity(a),
                bracket: BracketShape::Swiss,
                total_rounds: Some(total_rounds),
            },
            env,
        )
        .expect("create_tournament");
        join_n(&mut mgr, "T", n, env);
        mgr
    }

    /// Pod sizes generated for `round`, largest first.
    fn round_sizes(mgr: &TournamentManager, code: &str, round: u32) -> Vec<usize> {
        let mut sizes: Vec<usize> = mgr
            .get(code)
            .expect("tournament")
            .pairings
            .iter()
            .filter(|p| p.round == round)
            .map(|p| p.players.len())
            .collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        sizes
    }

    /// Reports a decisive win for the first seat of every pending pairing.
    fn report_all_pending(mgr: &mut TournamentManager, code: &str, env: &FakeEnv) {
        let meta = mgr.get(code).expect("tournament");
        let head_to_head = meta.arity == MatchArity::HEAD_TO_HEAD;
        let pending: Vec<(PairingId, Vec<String>)> = meta
            .pairings
            .iter()
            .filter(|p| p.outcome.is_none())
            .map(|p| (p.id, p.players.clone()))
            .collect();
        for (id, players) in pending {
            let mut game_wins = HashMap::new();
            if head_to_head {
                game_wins.insert(players[0].clone(), 2u8);
                game_wins.insert(players[1].clone(), 0u8);
            }
            mgr.report_result(
                code,
                id,
                PodOutcome::Decisive {
                    winner: players[0].clone(),
                    game_wins,
                },
                env,
            )
            .expect("report_result");
        }
    }

    /// Fails if any two players are seated together twice across the whole
    /// pairing history.
    fn assert_no_rematches(meta: &TournamentMeta, context: &str) {
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for pairing in &meta.pairings {
            for (i, left) in pairing.players.iter().enumerate() {
                for right in pairing.players.iter().skip(i + 1) {
                    let mut pair = [left.clone(), right.clone()];
                    pair.sort();
                    assert!(
                        seen.insert((pair[0].clone(), pair[1].clone())),
                        "{context}: rematch {pair:?} in round {}",
                        pairing.round
                    );
                }
            }
        }
    }

    fn standing_of<'a>(rows: &'a [TournamentStanding], player_key: &str) -> &'a TournamentStanding {
        rows.iter()
            .find(|row| row.player_key == player_key)
            .expect("standing row")
    }

    fn head_to_head_pairing(a: &str, b: &str) -> TournamentPairing {
        TournamentPairing {
            id: 0,
            round: 1,
            players: vec![a.to_string(), b.to_string()],
            outcome: None,
        }
    }

    fn undropped(keys: &[&str]) -> Vec<TournamentPlayer> {
        keys.iter()
            .map(|k| TournamentPlayer {
                player_key: (*k).to_string(),
                player_token: format!("token-{k}"),
                display_name: (*k).to_string(),
                dropped: false,
            })
            .collect()
    }

    fn bo3(a: &str, wa: u8, b: &str, wb: u8) -> HashMap<String, u8> {
        HashMap::from([(a.to_string(), wa), (b.to_string(), wb)])
    }

    // -- unit 1: MatchArity -------------------------------------------------

    #[test]
    fn match_arity_rejects_zero_one_and_above_128() {
        assert!(MatchArity::new(0).is_err());
        assert!(MatchArity::new(1).is_err());
        assert!(MatchArity::new(129).is_err());
        assert!(MatchArity::new(255).is_err());

        assert_eq!(MatchArity::new(2).expect("arity 2").get(), 2);
        assert_eq!(MatchArity::new(128).expect("arity 128").get(), 128);
        assert_eq!(MatchArity::HEAD_TO_HEAD.get(), 2);
        assert_eq!(MatchArity::COMMANDER_POD.get(), 4);

        // Wire deserialization routes through the same constructor.
        assert!(serde_json::from_str::<MatchArity>("0").is_err());
        assert!(serde_json::from_str::<MatchArity>("1").is_err());
        assert!(serde_json::from_str::<MatchArity>("129").is_err());
        assert_eq!(
            serde_json::from_str::<MatchArity>("4").expect("wire arity 4"),
            MatchArity::COMMANDER_POD
        );
        assert_eq!(
            serde_json::to_string(&MatchArity::COMMANDER_POD).expect("serialize arity"),
            "4"
        );
    }

    // -- unit 2: ScoringPolicy ----------------------------------------------

    #[test]
    fn default_for_arity_at_arity_128_is_255_not_panic() {
        let policy = ScoringPolicy::default_for_arity(arity(128));
        assert_eq!(policy.win_points(), 255);
        assert_eq!(policy.draw_points(), 1);
        assert_eq!(policy.loss_points(), 0);

        // MTR 2.1 falls straight out of the MSTR 2n-1 formula at arity 2.
        let head_to_head = ScoringPolicy::default_for_arity(MatchArity::HEAD_TO_HEAD);
        assert_eq!(
            (
                head_to_head.win_points(),
                head_to_head.draw_points(),
                head_to_head.loss_points()
            ),
            (3, 1, 0)
        );
        assert_eq!(
            ScoringPolicy::default_for_arity(MatchArity::COMMANDER_POD).win_points(),
            7
        );
        assert_eq!(ScoringPolicy::default(), head_to_head);
    }

    #[test]
    fn scoring_policy_rejects_zero_win_points() {
        assert!(ScoringPolicy::new(0, 1, 0).is_err());
        // No ordering is imposed on the three values.
        assert!(ScoringPolicy::new(3, 1, 0).is_ok());
        assert!(ScoringPolicy::new(3, 0, 0).is_ok());
        assert!(ScoringPolicy::new(1, 5, 9).is_ok());

        assert!(serde_json::from_str::<ScoringPolicy>(
            r#"{"win_points":0,"draw_points":1,"loss_points":0}"#
        )
        .is_err());
        let parsed: ScoringPolicy =
            serde_json::from_str(r#"{"win_points":5,"draw_points":1,"loss_points":0}"#)
                .expect("valid wire policy");
        assert_eq!(parsed.win_points(), 5);
        assert_eq!(
            serde_json::to_string(&parsed).expect("serialize policy"),
            r#"{"win_points":5,"draw_points":1,"loss_points":0}"#
        );
    }

    // -- unit 4: derived pairing-history queries ----------------------------

    #[test]
    fn derived_queries_from_multi_round_history() {
        let pairings = vec![
            TournamentPairing {
                id: 0,
                round: 1,
                players: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                outcome: Some(PairingOutcome::Reported(PodOutcome::Draw)),
            },
            TournamentPairing {
                id: 1,
                round: 1,
                players: vec!["e".into()],
                outcome: Some(PairingOutcome::Bye),
            },
            TournamentPairing {
                id: 2,
                round: 2,
                players: vec!["a".into(), "e".into(), "f".into()],
                outcome: None,
            },
        ];
        let pod = MatchArity::COMMANDER_POD;

        assert!(had_bye("e", &pairings));
        assert!(!had_bye("a", &pairings));
        // A short pod is not a bye: everyone seated had real opponents.
        assert!(had_short_pod("a", pod, &pairings));
        assert!(had_short_pod("f", pod, &pairings));
        assert!(!had_short_pod("b", pod, &pairings));
        // At head-to-head an `arity - 1` pairing is a bye, never a short pod.
        assert!(!had_short_pod("e", MatchArity::HEAD_TO_HEAD, &pairings));

        assert_eq!(
            prior_opponents("a", &pairings),
            HashSet::from([
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
                "f".to_string()
            ])
        );
        // A bye contributes no opponents.
        assert_eq!(
            prior_opponents("e", &pairings),
            HashSet::from(["a".to_string(), "f".to_string()])
        );
        assert!(prior_opponents("zz", &pairings).is_empty());
    }

    // -- unit 5: construction ------------------------------------------------

    #[test]
    fn construction_round_trips_every_field() {
        let env = FakeEnv::new();
        env.set_now_ms(5_000_000);
        let mut mgr = TournamentManager::new();
        assert!(mgr.is_empty());

        let scoring = ScoringPolicy::new(5, 1, 0).expect("policy");
        let token = mgr
            .create_tournament(
                "ABCD",
                CreateTournamentRequest {
                    name: "Friday Pods".to_string(),
                    arity: MatchArity::COMMANDER_POD,
                    scoring,
                    bracket: BracketShape::Swiss,
                    total_rounds: Some(4),
                },
                &env,
            )
            .expect("create");

        let meta = mgr.get("ABCD").expect("tournament");
        assert_eq!(meta.code, "ABCD");
        assert_eq!(meta.name, "Friday Pods");
        assert_eq!(meta.organizer_token, token);
        assert_eq!(meta.arity, MatchArity::COMMANDER_POD);
        assert_eq!(meta.scoring, scoring);
        assert_eq!(meta.bracket, BracketShape::Swiss);
        assert_eq!(meta.total_rounds_override, Some(4));
        assert_eq!(meta.total_rounds(), 4);
        assert_eq!(meta.current_round, 0);
        assert_eq!(meta.status, TournamentStatus::Registration);
        assert!(meta.players.is_empty());
        assert!(meta.pairings.is_empty());
        assert_eq!(meta.created_at, 5000);
        assert_eq!(meta.last_activity_at, 5000);
        assert_eq!(mgr.len(), 1);

        // A duplicate code never silently replaces a tournament's history.
        assert!(mgr
            .create_tournament(
                "ABCD",
                CreateTournamentRequest {
                    name: "Clash".to_string(),
                    arity: MatchArity::HEAD_TO_HEAD,
                    scoring: ScoringPolicy::default(),
                    bracket: BracketShape::Swiss,
                    total_rounds: None,
                },
                &env,
            )
            .is_err());

        let player_token = mgr
            .join_tournament("ABCD", "p00", "Ada", &env)
            .expect("join");
        let player = mgr.get("ABCD").expect("t").player("p00").expect("player");
        assert_eq!(player.player_token, player_token);
        assert_eq!(player.display_name, "Ada");
        assert!(!player.dropped);
        assert!(mgr
            .join_tournament("ABCD", "p00", "Ada Again", &env)
            .is_err());
    }

    #[test]
    fn single_elimination_rejects_non_head_to_head_arity() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        for seats in [3u8, 4, 6, 128] {
            let request = CreateTournamentRequest {
                name: "Pod SE".to_string(),
                arity: arity(seats),
                scoring: ScoringPolicy::default_for_arity(arity(seats)),
                bracket: BracketShape::SingleElimination,
                total_rounds: None,
            };
            assert!(
                mgr.create_tournament(&format!("SE{seats}"), request, &env)
                    .is_err(),
                "single elimination must be rejected at arity {seats}"
            );
        }
        // The two accepted siblings.
        assert!(mgr
            .create_tournament(
                "SE2",
                CreateTournamentRequest {
                    name: "1v1 SE".to_string(),
                    arity: MatchArity::HEAD_TO_HEAD,
                    scoring: ScoringPolicy::default(),
                    bracket: BracketShape::SingleElimination,
                    total_rounds: None,
                },
                &env,
            )
            .is_ok());
        assert!(mgr
            .create_tournament(
                "SW4",
                CreateTournamentRequest {
                    name: "Pod Swiss".to_string(),
                    arity: MatchArity::COMMANDER_POD,
                    scoring: ScoringPolicy::default_for_arity(MatchArity::COMMANDER_POD),
                    bracket: BracketShape::Swiss,
                    total_rounds: None,
                },
                &env,
            )
            .is_ok());
    }

    #[test]
    fn default_total_rounds_uses_the_arity_selected_table() {
        // MTR Appendix E's published rows.
        for (players, rounds) in [(4, 3), (8, 3), (9, 4), (16, 4), (17, 5), (32, 5), (64, 6)] {
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, players),
                rounds,
                "head-to-head default for {players} players"
            );
        }
        // MSTR's own, genuinely different table.
        for (players, rounds) in [(4, 2), (16, 2), (17, 3), (25, 4), (33, 5), (64, 5)] {
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::COMMANDER_POD, players),
                rounds,
                "pod default for {players} players"
            );
        }
        // The override wins, and is what a `None` falls back from.
        let env = FakeEnv::new();
        let mut mgr = swiss(9, 2, &env);
        assert_eq!(mgr.get("T").expect("t").total_rounds(), 4);
        mgr.meta_mut("T").expect("t").total_rounds_override = Some(11);
        assert_eq!(mgr.get("T").expect("t").total_rounds(), 11);
    }

    /// The head-to-head Swiss default is the doubling rule "smallest `r` with
    /// `2^r >= players`" the whole way up: no plateau, no off-power-of-two row
    /// boundary. Both sides of every boundary are pinned, because the counts
    /// above 128 players are exactly where a mis-transcribed chart would land
    /// a fabricated cap or an invented row.
    #[test]
    fn default_total_rounds_follows_the_doubling_rule_without_plateau() {
        for (players, rounds) in [
            (16, 4),
            (17, 5),
            (32, 5),
            (33, 6),
            (64, 6),
            (65, 7),
            (128, 7),
            (129, 8),
            (256, 8),
            (257, 9),
            (512, 9),
            (513, 10),
            (1024, 10),
            // Still climbing: a cap or plateau at 10 rounds fails here.
            (1025, 11),
        ] {
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, players),
                rounds,
                "head-to-head Swiss default for {players} players"
            );
        }

        // Unbounded and monotonic: every doubling adds exactly one round, as
        // far as a `u32` field can go. Nothing plateaus, and no row boundary
        // sits anywhere but a power of two.
        for r in 4..32u32 {
            let full = 1u32 << r;
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, full),
                r,
                "exactly 2^{r} players"
            );
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, full + 1),
                r + 1,
                "one player past 2^{r}"
            );
        }

        // Above the 3-round floor the Swiss table and the single-elimination
        // bracket depth are the same `ceil(log2(field))` arithmetic — the doc
        // comment's internal-consistency claim, asserted here rather than left
        // to prose.
        for players in [9u32, 16, 17, 100, 1025] {
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, players),
                default_total_rounds(
                    BracketShape::SingleElimination,
                    MatchArity::HEAD_TO_HEAD,
                    players
                ),
                "doubling rule and bracket depth agree for {players} players"
            );
        }
    }

    /// A single-elimination event's round count is its bracket depth, not the
    /// Swiss recommendation table: the same field size answers differently
    /// under the two bracket shapes, and the small brackets the Swiss table
    /// floors at 3 are decided in 1 or 2.
    #[test]
    fn default_total_rounds_uses_bracket_depth_for_single_elimination() {
        for (players, depth) in [(2, 1), (3, 2), (4, 2), (5, 3), (6, 3), (7, 3), (8, 3)] {
            assert_eq!(
                default_total_rounds(
                    BracketShape::SingleElimination,
                    MatchArity::HEAD_TO_HEAD,
                    players
                ),
                depth,
                "single-elimination depth for {players} players"
            );
            // The same field under Swiss still gets Appendix E's floor of 3 —
            // proof the bracket arm is what moved, not the table.
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, players),
                3,
                "Swiss default is unchanged for {players} players"
            );
        }
        // A field too small to seat a bracket reports no rounds rather than a
        // fabricated floor, matching `build_single_elimination_round`'s own
        // refusal below `SINGLE_ELIMINATION_MIN_PLAYERS`.
        for players in [0, 1] {
            assert_eq!(
                default_total_rounds(
                    BracketShape::SingleElimination,
                    MatchArity::HEAD_TO_HEAD,
                    players
                ),
                0,
                "no bracket to run for {players} players"
            );
        }

        // End to end through a real tournament: the accessor reads
        // `self.bracket`, so a 2-player final is one round and a 3- or
        // 4-player bracket is two.
        let env = FakeEnv::new();
        for (players, depth) in [(2, 1), (3, 2), (4, 2)] {
            let mut mgr = TournamentManager::new();
            create(
                &mut mgr,
                "SE",
                MatchArity::HEAD_TO_HEAD,
                BracketShape::SingleElimination,
                &env,
            );
            join_n(&mut mgr, "SE", players, &env);
            assert_eq!(
                mgr.get("SE").expect("t").total_rounds(),
                depth,
                "{players}-player single-elimination bracket"
            );
            // The override still wins over the bracket-derived depth.
            mgr.meta_mut("SE").expect("t").total_rounds_override = Some(7);
            assert_eq!(mgr.get("SE").expect("t").total_rounds(), 7);
        }
    }

    // -- unit 6: pairing ------------------------------------------------------

    #[test]
    fn partition_9_players_is_3_3_3() {
        let env = FakeEnv::new();
        let mut mgr = swiss(9, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        assert_eq!(round_sizes(&mgr, "T", 1), vec![3, 3, 3]);
        // No bye is issued when a short-pod-only partition exists.
        assert!(mgr
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .all(|p| p.outcome.is_none()));
    }

    #[test]
    fn partition_10_players_is_4_3_3() {
        let env = FakeEnv::new();
        let mut mgr = swiss(10, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        assert_eq!(round_sizes(&mgr, "T", 1), vec![4, 3, 3]);

        // Sibling: an evenly-divisible field needs no short pod at all.
        let mut even = swiss(8, 4, &env);
        even.generate_pairings("T", &env).expect("round 1");
        assert_eq!(round_sizes(&even, "T", 1), vec![4, 4]);
    }

    #[test]
    fn partition_generalizes_across_arities_2_3_4_6() {
        // Nothing here is hardcoded to 4 seats.
        let cases: [(usize, u8, Vec<usize>); 8] = [
            (7, 2, vec![2, 2, 2, 1]),
            (8, 2, vec![2, 2, 2, 2]),
            (4, 3, vec![2, 2]),
            (5, 3, vec![3, 2]),
            (7, 3, vec![3, 2, 2]),
            (9, 4, vec![3, 3, 3]),
            (12, 6, vec![6, 6]),
            // At arity 6 there is no {5,6} partition of 9, so the minimum
            // number of byes (3) is issued alongside the one full pod.
            (9, 6, vec![6, 1, 1, 1]),
        ];
        for (players, seats, expected) in cases {
            let env = FakeEnv::new();
            let mut mgr = swiss(players, seats, &env);
            mgr.generate_pairings("T", &env).expect("round 1");
            assert_eq!(
                round_sizes(&mgr, "T", 1),
                expected,
                "{players} players at arity {seats}"
            );
        }
    }

    #[test]
    fn degenerate_counts_n0_n1_n2_n5() {
        let env = FakeEnv::new();
        for (players, expected) in [
            (0usize, Vec::new()),
            (1, vec![1]),
            // The one accepted multiple-bye exception: no pod of 3 or 4 can
            // form from two players.
            (2, vec![1, 1]),
            // A valid 4-pod IS available here, so only one bye is issued.
            (5, vec![4, 1]),
            // Sibling: 6 is not degenerate and must not take the bye path.
            (6, vec![3, 3]),
        ] {
            let mut mgr = swiss(players, 4, &env);
            mgr.generate_pairings("T", &env).expect("round 1");
            assert_eq!(
                round_sizes(&mgr, "T", 1),
                expected,
                "{players} active players at arity 4"
            );
            // Every one-player pairing is resolved as a bye at generation.
            for pairing in &mgr.get("T").expect("t").pairings {
                if pairing.players.len() == 1 {
                    assert_eq!(pairing.outcome, Some(PairingOutcome::Bye));
                }
            }
        }
    }

    #[test]
    fn drop_reduced_field_pairs_identically_to_starting_at_that_count() {
        let env = FakeEnv::new();
        // 6 players, one drops mid-event, leaving 5 for the next round.
        let mut shrunk = swiss(6, 4, &env);
        shrunk.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut shrunk, "T", &env);
        shrunk.drop_player("T", &key(5), &env).expect("drop");
        shrunk.generate_pairings("T", &env).expect("round 2");
        assert_eq!(round_sizes(&shrunk, "T", 2), vec![4, 1]);

        // Starting a round at 5 resolves the same way — drops are not a
        // separate code path.
        let mut fresh = swiss(5, 4, &env);
        fresh.generate_pairings("T", &env).expect("round 1");
        assert_eq!(round_sizes(&fresh, "T", 1), vec![4, 1]);
    }

    #[test]
    fn swiss_pairing_avoids_rematch_5_7_9_players() {
        for players in [5usize, 7, 9] {
            let env = FakeEnv::new();
            let mut mgr = swiss(players, 2, &env);
            for round in 1..=3 {
                mgr.generate_pairings("T", &env)
                    .unwrap_or_else(|e| panic!("{players} players, round {round}: {e}"));
                assert_no_rematches(
                    mgr.get("T").expect("t"),
                    &format!("{players}-player odd bracket"),
                );
                report_all_pending(&mut mgr, "T", &env);
            }
            // Byes spread rather than repeating on one player.
            let meta = mgr.get("T").expect("t");
            for player in &meta.players {
                let byes = meta
                    .pairings
                    .iter()
                    .filter(|p| p.players.len() == 1 && p.players[0] == player.player_key)
                    .count();
                assert!(
                    byes <= 1,
                    "{players} players: {} took {byes} byes",
                    player.player_key
                );
            }
        }
    }

    #[test]
    fn swap_repair_fixes_a_rematch_the_greedy_pass_cannot_avoid() {
        // Standings order a..f. Only e and f have met. A greedy top-to-bottom
        // pass pairs [a,b], [c,d], [e,f] — and the rematch lands in the LAST
        // pod, so only a swap against a pod ABOVE can repair it.
        let order: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|k| (*k).to_string())
            .collect();
        let history = vec![TournamentPairing {
            id: 0,
            round: 1,
            players: vec!["e".into(), "f".into()],
            outcome: Some(PairingOutcome::Reported(PodOutcome::Draw)),
        }];

        let round = build_swiss_round(&order, MatchArity::HEAD_TO_HEAD, &history, 2, 1);
        assert_eq!(round.len(), 3);
        for pairing in &round {
            assert_eq!(pairing.players.len(), 2);
            assert!(
                !(pairing.players.contains(&"e".to_string())
                    && pairing.players.contains(&"f".to_string())),
                "swap repair must break the e/f rematch, got {:?}",
                pairing.players
            );
        }
        // Ids continue the tournament's monotonic counter.
        assert_eq!(
            round.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn short_pod_fairness_prefers_players_who_have_not_been_shorted() {
        let env = FakeEnv::new();
        let mut mgr = swiss(9, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let round_one_short: HashSet<String> = mgr
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .filter(|p| p.round == 1 && p.players.len() == 3)
            .flat_map(|p| p.players.clone())
            .collect();
        // 9 players at arity 4 shorts everyone, so round 2 cannot improve on
        // it; 10 players shorts only six, and the round-2 short pods must
        // prefer the four who were not shorted.
        assert_eq!(round_one_short.len(), 9);

        let mut ten = swiss(10, 4, &env);
        ten.generate_pairings("T", &env).expect("round 1");
        let shorted: HashSet<String> = ten
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .filter(|p| p.players.len() == 3)
            .flat_map(|p| p.players.clone())
            .collect();
        assert_eq!(shorted.len(), 6);
        let full_seat = ten
            .get("T")
            .expect("t")
            .players
            .iter()
            .map(|p| p.player_key.clone())
            .find(|k| !shorted.contains(k))
            .expect("someone sat in the full pod");
        assert!(!had_short_pod(
            &full_seat,
            MatchArity::COMMANDER_POD,
            &ten.get("T").expect("t").pairings
        ));
    }

    /// The write path's own guards, exercised through `report_result` rather
    /// than through `validate_match_result` alone: a server-assigned outcome
    /// is permanent, a dropped player cannot be credited a win by an actual
    /// report, and a terminal tournament accepts no reports at all.
    #[test]
    fn report_result_rejects_server_assigned_dropped_and_terminal_writes() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let id = mgr.get("T").expect("t").pairings[0].id;

        // A dropped player reported as the winner is rejected on the real
        // write path, not merely by the pure validator.
        mgr.drop_player("T", &key(0), &env).expect("drop");
        assert!(mgr
            .report_result(
                "T",
                id,
                PodOutcome::Decisive {
                    winner: key(0),
                    game_wins: HashMap::new(),
                },
                &env,
            )
            .is_err());
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            None
        );
        assert!(mgr
            .report_result("T", 9_999, PodOutcome::Draw, &env)
            .is_err());

        // A forfeit is server-assigned and permanent: a later report cannot
        // overwrite it.
        mgr.drop_player("T", &key(1), &env).expect("drop");
        mgr.drop_player("T", &key(2), &env).expect("drop");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            Some(PairingOutcome::Forfeit { winner: key(3) })
        );
        assert!(mgr
            .report_result(
                "T",
                id,
                PodOutcome::Decisive {
                    winner: key(3),
                    game_wins: HashMap::new(),
                },
                &env,
            )
            .is_err());
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            Some(PairingOutcome::Forfeit { winner: key(3) })
        );

        // A terminal tournament accepts nothing further.
        mgr.complete_tournament("T", &env).expect("complete");
        assert!(mgr.report_result("T", id, PodOutcome::Draw, &env).is_err());
        assert!(mgr.complete_tournament("T", &env).is_err());
        assert!(mgr.join_tournament("T", "late", "Late", &env).is_err());
        // A draw and a pending pairing both report no winner.
        assert_eq!(
            TournamentPairing {
                id: 0,
                round: 1,
                players: vec!["a".into(), "b".into()],
                outcome: Some(PairingOutcome::Reported(PodOutcome::Draw)),
            }
            .winner(),
            None
        );
        assert_eq!(head_to_head_pairing("a", "b").winner(), None);
    }

    // -- unit 7: drops --------------------------------------------------------

    #[test]
    fn head_to_head_drop_forfeits() {
        let env = FakeEnv::new();
        let mut mgr = swiss(2, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let id = mgr.get("T").expect("t").pairings[0].id;
        assert_eq!(mgr.get("T").expect("t").pairings[0].outcome, None);

        mgr.drop_player("T", &key(0), &env).expect("drop");
        assert_eq!(
            mgr.get("T")
                .expect("t")
                .pairing(id)
                .expect("pairing")
                .outcome,
            Some(PairingOutcome::Forfeit { winner: key(1) })
        );
        // Scored identically to a normal win.
        let rows = mgr.get("T").expect("t").standings();
        assert_eq!(standing_of(&rows, &key(1)).match_points, 3);
        assert_eq!(standing_of(&rows, &key(0)).match_points, 0);
    }

    #[test]
    fn pod_drop_does_not_auto_resolve() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let id = mgr.get("T").expect("t").pairings[0].id;

        mgr.drop_player("T", &key(0), &env).expect("drop");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            None,
            "three active players still play the pod out"
        );
        // A real result from the remaining players is accepted normally.
        mgr.report_result(
            "T",
            id,
            PodOutcome::Decisive {
                winner: key(1),
                game_wins: HashMap::new(),
            },
            &env,
        )
        .expect("report");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").winner(),
            Some(key(1).as_str())
        );
    }

    #[test]
    fn pod_drop_to_last_player_forfeits() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 4, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let id = mgr.get("T").expect("t").pairings[0].id;

        // Three separate drops: only the third one auto-settles.
        mgr.drop_player("T", &key(0), &env).expect("drop 1");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            None
        );
        mgr.drop_player("T", &key(1), &env).expect("drop 2");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            None
        );
        mgr.drop_player("T", &key(2), &env).expect("drop 3");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").outcome,
            Some(PairingOutcome::Forfeit { winner: key(3) })
        );
    }

    #[test]
    fn resolved_pairing_is_unaffected_by_a_later_drop() {
        let env = FakeEnv::new();
        let mut mgr = swiss(2, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let id = mgr.get("T").expect("t").pairings[0].id;
        mgr.report_result(
            "T",
            id,
            PodOutcome::Decisive {
                winner: key(0),
                game_wins: bo3(&key(0), 2, &key(1), 1),
            },
            &env,
        )
        .expect("report");

        mgr.drop_player("T", &key(1), &env).expect("drop");
        assert_eq!(
            mgr.get("T").expect("t").pairing(id).expect("p").winner(),
            Some(key(0).as_str()),
            "a later drop must never rewrite a finished pairing"
        );
    }

    // -- unit 9: result validation --------------------------------------------

    #[test]
    fn bo3_validation_matrix() {
        let pairing = head_to_head_pairing("a", "b");
        let players = undropped(&["a", "b"]);

        let decisive = |winner: &str, wins: HashMap<String, u8>| PodOutcome::Decisive {
            winner: winner.to_string(),
            game_wins: wins,
        };

        // Illegal shapes.
        assert!(
            validate_match_result(&pairing, &decisive("a", HashMap::new()), &players).is_err(),
            "empty game_wins must be rejected"
        );
        assert!(validate_match_result(
            &pairing,
            &decisive("a", HashMap::from([("a".to_string(), 2u8)])),
            &players
        )
        .is_err());
        assert!(
            validate_match_result(&pairing, &decisive("a", bo3("a", 1, "b", 0)), &players).is_err()
        );
        assert!(
            validate_match_result(&pairing, &decisive("a", bo3("a", 0, "b", 0)), &players).is_err()
        );
        assert!(
            validate_match_result(&pairing, &decisive("a", bo3("a", 2, "b", 2)), &players).is_err()
        );
        assert!(
            validate_match_result(&pairing, &decisive("a", bo3("a", 3, "b", 0)), &players).is_err()
        );
        // Right tally, wrong winner named.
        assert!(
            validate_match_result(&pairing, &decisive("b", bo3("a", 2, "b", 1)), &players).is_err()
        );
        // A key that is not in the pairing at all.
        assert!(
            validate_match_result(&pairing, &decisive("c", bo3("a", 2, "b", 0)), &players).is_err()
        );
        assert!(
            validate_match_result(&pairing, &decisive("a", bo3("a", 2, "c", 0)), &players).is_err()
        );

        // The four legal completed tallies, with the correct winner.
        for (wa, wb, winner) in [(2u8, 0u8, "a"), (2, 1, "a"), (0, 2, "b"), (1, 2, "b")] {
            validate_match_result(&pairing, &decisive(winner, bo3("a", wa, "b", wb)), &players)
                .unwrap_or_else(|e| panic!("{wa}-{wb} to {winner} must validate: {e}"));
        }
        // A draw never carries game wins and is always legal.
        validate_match_result(&pairing, &PodOutcome::Draw, &players).expect("draw");
    }

    #[test]
    fn pod_result_validation_requires_empty_game_wins() {
        let pairing = TournamentPairing {
            id: 0,
            round: 1,
            players: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            outcome: None,
        };
        let players = undropped(&["a", "b", "c", "d"]);

        // Winner membership alone is enough at pod arity.
        validate_match_result(
            &pairing,
            &PodOutcome::Decisive {
                winner: "c".to_string(),
                game_wins: HashMap::new(),
            },
            &players,
        )
        .expect("pod decisive with no game wins");

        assert!(validate_match_result(
            &pairing,
            &PodOutcome::Decisive {
                winner: "c".to_string(),
                game_wins: HashMap::from([("c".to_string(), 1u8)]),
            },
            &players,
        )
        .is_err());
    }

    #[test]
    fn dropped_player_cannot_be_credited_a_win() {
        let pairing = head_to_head_pairing("a", "b");
        let mut players = undropped(&["a", "b"]);
        players[0].dropped = true;

        // `a` is still a legitimate member of `pairing.players` — membership
        // alone does not close this gap.
        assert!(pairing.players.contains(&"a".to_string()));
        assert!(validate_match_result(
            &pairing,
            &PodOutcome::Decisive {
                winner: "a".to_string(),
                game_wins: bo3("a", 2, "b", 0),
            },
            &players,
        )
        .is_err());
        // The player who did not drop can still be credited.
        validate_match_result(
            &pairing,
            &PodOutcome::Decisive {
                winner: "b".to_string(),
                game_wins: bo3("a", 0, "b", 2),
            },
            &players,
        )
        .expect("undropped winner");
    }

    // -- unit 10: replay-safe reporting ---------------------------------------

    #[test]
    fn correction_overwrites_not_accumulates() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let round_one: Vec<(PairingId, Vec<String>)> = mgr
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .map(|p| (p.id, p.players.clone()))
            .collect();
        let (id, seats) = round_one[0].clone();
        let (winner, loser) = (seats[0].clone(), seats[1].clone());

        report_all_pending(&mut mgr, "T", &env);
        // A later round is generated from the pre-correction standings.
        mgr.generate_pairings("T", &env).expect("round 2");
        let pairing_count = mgr.get("T").expect("t").pairings.len();
        let opponents_before = prior_opponents(&winner, &mgr.get("T").expect("t").pairings);

        // Correct round 1: the other player actually won, 2-1.
        mgr.report_result(
            "T",
            id,
            PodOutcome::Decisive {
                winner: loser.clone(),
                game_wins: bo3(&loser, 2, &winner, 1),
            },
            &env,
        )
        .expect("correction");

        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.pairings.len(), pairing_count, "no pairing was added");
        assert_eq!(meta.pairing(id).expect("p").winner(), Some(loser.as_str()));

        let rows = meta.standings();
        // Exactly one win's worth of points for the corrected winner from
        // round 1, and zero residue for the original winner.
        assert_eq!(standing_of(&rows, &loser).match_points, 3);
        assert_eq!(standing_of(&rows, &winner).match_points, 0);
        assert_eq!(standing_of(&rows, &loser).matches_played, 1);
        assert_eq!(standing_of(&rows, &winner).matches_played, 1);
        // Opponent history is derived, so it is unchanged by a correction.
        assert_eq!(prior_opponents(&winner, &meta.pairings), opponents_before);

        // Reporting the identical outcome again is a no-op, not an error.
        let before = meta.pairing(id).expect("p").clone();
        mgr.report_result(
            "T",
            id,
            PodOutcome::Decisive {
                winner: loser.clone(),
                game_wins: bo3(&loser, 2, &winner, 1),
            },
            &env,
        )
        .expect("idempotent re-report");
        assert_eq!(mgr.get("T").expect("t").pairing(id).expect("p"), &before);

        // A bye is server-assigned and cannot be overwritten by a report.
        let mut odd = swiss(3, 2, &env);
        odd.generate_pairings("T", &env).expect("round 1");
        let bye_id = odd
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .find(|p| p.outcome == Some(PairingOutcome::Bye))
            .expect("a bye")
            .id;
        assert!(odd
            .report_result(
                "T",
                bye_id,
                PodOutcome::Decisive {
                    winner: key(0),
                    game_wins: HashMap::new()
                },
                &env
            )
            .is_err());
    }

    // -- unit 8: standings / tiebreaks ----------------------------------------

    #[test]
    fn tiebreak_order_and_floor_are_arity_selected() {
        assert_eq!(
            TiebreakOrder::for_arity(MatchArity::HEAD_TO_HEAD),
            TiebreakOrder::HeadToHead
        );
        for seats in [3u8, 4, 6, 128] {
            assert_eq!(
                TiebreakOrder::for_arity(arity(seats)),
                TiebreakOrder::Multiplayer,
                "arity {seats}"
            );
        }
        // One shared formula, not a hardcoded 0.33 / 0.14 pair.
        let head_to_head = ScoringPolicy::default_for_arity(MatchArity::HEAD_TO_HEAD);
        let pod = ScoringPolicy::default_for_arity(MatchArity::COMMANDER_POD);
        assert!((head_to_head.tiebreak_floor() - 1.0 / 3.0).abs() < 1e-12);
        assert!((pod.tiebreak_floor() - 1.0 / 7.0).abs() < 1e-12);
        // And it follows an organizer override rather than a constant.
        let custom = ScoringPolicy::new(5, 1, 0).expect("policy");
        assert!((custom.tiebreak_floor() - 0.2).abs() < 1e-12);
    }

    #[test]
    fn standings_compute_the_selected_axes() {
        let env = FakeEnv::new();

        // Head-to-head: a bye scores as a win at win_points and as 2-0 for the
        // player's own game-win percentage, but contributes no opponents.
        let mut duel = swiss(3, 2, &env);
        duel.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut duel, "T", &env);
        let meta = duel.get("T").expect("t");
        let rows = meta.standings();
        let bye_taker = meta
            .pairings
            .iter()
            .find(|p| p.players.len() == 1)
            .expect("a bye")
            .players[0]
            .clone();
        let bye_row = standing_of(&rows, &bye_taker);
        assert_eq!(bye_row.byes, 1);
        assert_eq!(bye_row.matches_played, 0, "a bye is not a match played");
        assert_eq!(bye_row.match_points, 3);
        match bye_row.tiebreaks {
            Tiebreaks::HeadToHead {
                game_win_pct,
                opponents_match_win_pct,
                ..
            } => {
                assert!((game_win_pct - 1.0).abs() < 1e-12, "bye counts 2-0");
                // No real opponents that round: the average falls back to the
                // shared floor rather than zero-filling.
                assert!((opponents_match_win_pct - 1.0 / 3.0).abs() < 1e-12);
            }
            Tiebreaks::Multiplayer { .. } => panic!("arity 2 must select the MTR order"),
        }

        // Pods select MSTR's axes, including one 1v1 has no analogue for.
        let mut pods = swiss(4, 4, &env);
        pods.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut pods, "T", &env);
        let rows = pods.get("T").expect("t").standings();
        assert_eq!(rows[0].match_points, 7, "MSTR 2n-1 at a 4-player pod");
        match rows[0].tiebreaks {
            Tiebreaks::Multiplayer {
                match_win_pct,
                opponents_avg_match_points,
                ..
            } => {
                assert!((match_win_pct - 1.0).abs() < 1e-12);
                assert!(opponents_avg_match_points.abs() < 1e-12, "three losers");
            }
            Tiebreaks::HeadToHead { .. } => panic!("arity 4 must select the MSTR order"),
        }
        // Best first.
        assert!(rows[0].match_points >= rows[3].match_points);
    }

    // -- single elimination ----------------------------------------------------

    #[test]
    fn single_elimination_pairs_by_seed_then_advances_winners() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        create(
            &mut mgr,
            "SE",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::SingleElimination,
            &env,
        );
        join_n(&mut mgr, "SE", 8, &env);
        mgr.generate_pairings("SE", &env).expect("round 1");

        let round_one: Vec<Vec<String>> = mgr
            .get("SE")
            .expect("t")
            .pairings
            .iter()
            .map(|p| p.players.clone())
            .collect();
        assert_eq!(round_one.len(), 4);
        assert_eq!(round_one[0], vec![key(0), key(7)]);
        assert_eq!(round_one[3], vec![key(3), key(4)]);

        report_all_pending(&mut mgr, "SE", &env);
        mgr.generate_pairings("SE", &env).expect("round 2");
        let round_two: Vec<Vec<String>> = mgr
            .get("SE")
            .expect("t")
            .pairings
            .iter()
            .filter(|p| p.round == 2)
            .map(|p| p.players.clone())
            .collect();
        assert_eq!(round_two, vec![vec![key(0), key(1)], vec![key(2), key(3)]]);

        // A field outside the supported range is rejected outright — above
        // MTR Appendix E's cut, and below a pairable field. Counts *inside*
        // the range that are not powers of two are not rejected; they take
        // byes (see `single_elimination_byes_seed_the_top_of_the_bracket`).
        for n in [SINGLE_ELIMINATION_MAX_PLAYERS + 1, 12] {
            let mut oversized = TournamentManager::new();
            let code = format!("SE{n}");
            create(
                &mut oversized,
                &code,
                MatchArity::HEAD_TO_HEAD,
                BracketShape::SingleElimination,
                &env,
            );
            join_n(&mut oversized, &code, n, &env);
            let err = oversized
                .generate_pairings(&code, &env)
                .expect_err("field is larger than the Appendix E cut");
            assert!(err.contains(&n.to_string()), "{err}");
        }

        let mut solo = TournamentManager::new();
        create(
            &mut solo,
            "SE1",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::SingleElimination,
            &env,
        );
        join_n(&mut solo, "SE1", SINGLE_ELIMINATION_MIN_PLAYERS - 1, &env);
        assert!(solo.generate_pairings("SE1", &env).is_err());
    }

    /// A 5/6/7-player single-elimination field — squarely inside MTR
    /// Appendix E's 4-8 cut, and previously rejected outright — pairs by
    /// rounding up to the next power of two and giving the surplus slots to
    /// the *top* seeds as byes, then advances bye recipients and real match
    /// winners through the identical path.
    #[test]
    fn single_elimination_byes_seed_the_top_of_the_bracket() {
        let env = FakeEnv::new();

        // The whole accepted range pairs; the byes always land on the top
        // seeds, and the count is exactly the shortfall to the next power of
        // two.
        for n in SINGLE_ELIMINATION_MIN_PLAYERS..=SINGLE_ELIMINATION_MAX_PLAYERS {
            let mut mgr = TournamentManager::new();
            create(
                &mut mgr,
                "SE",
                MatchArity::HEAD_TO_HEAD,
                BracketShape::SingleElimination,
                &env,
            );
            join_n(&mut mgr, "SE", n, &env);
            mgr.generate_pairings("SE", &env)
                .unwrap_or_else(|e| panic!("{n}-player bracket must pair: {e}"));

            let meta = mgr.get("SE").expect("t");
            let slots = n.next_power_of_two();
            assert_eq!(meta.pairings.len(), slots / 2, "{n} players");
            // The reported round count is this bracket's own depth — the
            // number of halvings from `slots` to one survivor — not the Swiss
            // table's flat 3 for the whole 2-8 range.
            assert_eq!(
                meta.total_rounds(),
                slots.trailing_zeros(),
                "{n} players: round count is the bracket's depth"
            );

            let byes: Vec<String> = meta
                .pairings
                .iter()
                .filter(|p| p.outcome == Some(PairingOutcome::Bye))
                .map(|p| p.players[0].clone())
                .collect();
            let expected_byes: Vec<String> = (0..slots - n).map(key).collect();
            assert_eq!(
                byes, expected_byes,
                "{n} players: byes belong to the top seeds, in seed order"
            );
            // Every non-bye pairing seats exactly two players, and nobody is
            // seated twice or dropped from the bracket.
            let mut seated: Vec<String> = meta
                .pairings
                .iter()
                .flat_map(|p| p.players.clone())
                .collect();
            seated.sort();
            let mut everyone: Vec<String> = (0..n).map(key).collect();
            everyone.sort();
            assert_eq!(seated, everyone, "{n} players: every entrant is seated");
            for pairing in &meta.pairings {
                assert!(
                    (1..=2).contains(&pairing.players.len()),
                    "{n} players: head-to-head seats or a single-seat bye"
                );
                assert_eq!(
                    pairing.outcome.is_some(),
                    pairing.players.len() == 1,
                    "{n} players: exactly the one-seat pairings are pre-resolved byes"
                );
            }
        }

        // Six players in detail: seeds 1-2 bye, 3v6 and 4v5 played, and both
        // kinds of round-1 result advance together into round 2.
        let mut six = TournamentManager::new();
        create(
            &mut six,
            "SE6",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::SingleElimination,
            &env,
        );
        join_n(&mut six, "SE6", 6, &env);
        six.generate_pairings("SE6", &env).expect("round 1");
        let round_one: Vec<Vec<String>> = six
            .get("SE6")
            .expect("t")
            .pairings
            .iter()
            .map(|p| p.players.clone())
            .collect();
        assert_eq!(
            round_one,
            vec![
                vec![key(0)],
                vec![key(1)],
                vec![key(2), key(5)],
                vec![key(3), key(4)],
            ]
        );
        // A bye scores as a win, exactly as it does in Swiss.
        let rows = six.get("SE6").expect("t").standings();
        assert_eq!(standing_of(&rows, &key(0)).match_points, 3);
        assert_eq!(standing_of(&rows, &key(5)).match_points, 0);

        report_all_pending(&mut six, "SE6", &env);
        six.generate_pairings("SE6", &env).expect("round 2");
        let round_two: Vec<Vec<String>> = six
            .get("SE6")
            .expect("t")
            .pairings
            .iter()
            .filter(|p| p.round == 2)
            .map(|p| p.players.clone())
            .collect();
        assert_eq!(
            round_two,
            vec![vec![key(0), key(1)], vec![key(2), key(3)]],
            "both bye recipients and both match winners advance"
        );

        // ...and on to a single final, in the 3 rounds a 6-player bracket is
        // deep (8 slots -> 4 -> 2 -> 1).
        assert_eq!(six.get("SE6").expect("t").total_rounds(), 3);
        report_all_pending(&mut six, "SE6", &env);
        six.generate_pairings("SE6", &env).expect("round 3");
        let final_round: Vec<Vec<String>> = six
            .get("SE6")
            .expect("t")
            .pairings
            .iter()
            .filter(|p| p.round == 3)
            .map(|p| p.players.clone())
            .collect();
        assert_eq!(final_round, vec![vec![key(0), key(2)]]);
        report_all_pending(&mut six, "SE6", &env);
        assert!(
            six.generate_pairings("SE6", &env).is_err(),
            "the bracket is decided"
        );

        // Five players: three byes, one match, and a bye recipient meets a
        // match winner in round 2.
        let mut five = TournamentManager::new();
        create(
            &mut five,
            "SE5",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::SingleElimination,
            &env,
        );
        join_n(&mut five, "SE5", 5, &env);
        five.generate_pairings("SE5", &env).expect("round 1");
        assert_eq!(
            five.get("SE5")
                .expect("t")
                .pairings
                .iter()
                .map(|p| p.players.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![key(0)],
                vec![key(1)],
                vec![key(2)],
                vec![key(3), key(4)],
            ]
        );
        report_all_pending(&mut five, "SE5", &env);
        five.generate_pairings("SE5", &env).expect("round 2");
        assert_eq!(
            five.get("SE5")
                .expect("t")
                .pairings
                .iter()
                .filter(|p| p.round == 2)
                .map(|p| p.players.clone())
                .collect::<Vec<_>>(),
            vec![vec![key(0), key(1)], vec![key(2), key(3)]]
        );
    }

    // -- round-advancement and completion guards --------------------------------

    /// A round is finished before the next one is paired. Pairing around an
    /// unreported match would seed the new round from standings that are wrong
    /// by exactly that match, and strand a pairing that a later
    /// `complete_tournament` would make permanently unreportable.
    #[test]
    fn generate_pairings_rejects_an_unfinished_round() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let round_one: Vec<(PairingId, Vec<String>)> = mgr
            .get("T")
            .expect("t")
            .pairings
            .iter()
            .map(|p| (p.id, p.players.clone()))
            .collect();
        assert_eq!(round_one.len(), 2);

        // Report one of the two; the other is still pending.
        let (reported, seats) = round_one[0].clone();
        mgr.report_result(
            "T",
            reported,
            PodOutcome::Decisive {
                winner: seats[0].clone(),
                game_wins: bo3(&seats[0], 2, &seats[1], 0),
            },
            &env,
        )
        .expect("report");

        let straggler = round_one[1].0;
        let err = mgr
            .generate_pairings("T", &env)
            .expect_err("round 2 must wait for round 1");
        assert!(err.contains(&straggler.to_string()), "{err}");
        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.current_round, 1, "the rejected call advanced nothing");
        assert_eq!(meta.pairings.len(), 2, "no pairing was generated");

        // Settling the straggler unblocks the next round.
        report_all_pending(&mut mgr, "T", &env);
        mgr.generate_pairings("T", &env).expect("round 2");
        assert_eq!(mgr.get("T").expect("t").current_round, 2);

        // A drop that auto-forfeits is the other way to settle one.
        let env2 = FakeEnv::new();
        let mut dropped = swiss(4, 2, &env2);
        dropped.generate_pairings("T", &env2).expect("round 1");
        assert!(dropped.generate_pairings("T", &env2).is_err());
        for i in 0..4 {
            dropped.drop_player("T", &key(i), &env2).expect("drop");
        }
        // Every pairing lost a player, so every pairing forfeited.
        assert!(dropped
            .get("T")
            .expect("t")
            .first_unresolved_pairing()
            .is_none());
    }

    /// The configured round count is a ceiling on round *generation*. Without
    /// this an event created with `total_rounds: Some(1)` could settle round 1
    /// and pair round 2, then repeat indefinitely, leaving the override — and
    /// the schedule every client is shown — with no authority at all.
    #[test]
    fn generate_pairings_refuses_to_pair_past_the_override_round_total() {
        let env = FakeEnv::new();
        let mut mgr = swiss_capped(4, 2, 1, &env);
        assert_eq!(
            mgr.get("T").expect("t").total_rounds(),
            1,
            "the override is the authority"
        );

        mgr.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut mgr, "T", &env);

        // Snapshotting the whole record, not just `current_round`: the
        // rejection has to be a pure no-op, and a `Debug` comparison catches a
        // field this test never thought to name (a bumped `last_activity_at`,
        // a half-extended pairing list, a status flip).
        let before = format!("{:?}", mgr.get("T").expect("t"));
        let err = mgr
            .generate_pairings("T", &env)
            .expect_err("round 2 is past the configured total");
        assert!(err.contains("scheduled for 1 round(s)"), "{err}");
        assert!(err.contains("already at round 1"), "{err}");
        assert_eq!(
            format!("{:?}", mgr.get("T").expect("t")),
            before,
            "the rejected call mutated the tournament"
        );

        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.current_round, 1, "no round was advanced");
        assert_eq!(meta.pairings.len(), 2, "no pairing was generated");
        assert_eq!(meta.status, TournamentStatus::InProgress);

        // Guard ordering: a tournament that is both terminal *and* at its
        // ceiling reports that it is over, not that it is full.
        mgr.complete_tournament("T", &env).expect("complete");
        let terminal = mgr
            .generate_pairings("T", &env)
            .expect_err("a completed tournament pairs nothing");
        assert!(terminal.contains("no longer running"), "{terminal}");
    }

    /// The paired positive case for the ceiling: a tournament below its total
    /// still advances normally, so the guard costs no legitimate round.
    #[test]
    fn generate_pairings_still_advances_below_the_round_total() {
        let env = FakeEnv::new();
        let mut mgr = swiss_capped(4, 2, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut mgr, "T", &env);

        // 1 < 2 — the round the ceiling still allows.
        mgr.generate_pairings("T", &env).expect("round 2");
        assert_eq!(mgr.get("T").expect("t").current_round, 2);
        report_all_pending(&mut mgr, "T", &env);

        // 2 >= 2 — the first one it does not.
        let err = mgr
            .generate_pairings("T", &env)
            .expect_err("round 3 is past the configured total");
        assert!(err.contains("scheduled for 2 round(s)"), "{err}");
        assert_eq!(mgr.get("T").expect("t").current_round, 2);
    }

    /// The same ceiling, with no override in play: `total_rounds()` resolves
    /// through `default_total_rounds`, and the computed count binds exactly as
    /// hard as an organizer-set one.
    #[test]
    fn generate_pairings_refuses_to_pair_past_the_default_round_total() {
        let env = FakeEnv::new();
        // 8 players in 4-player pods: MSTR's table gives 2 rounds — the
        // smallest count the Swiss default produces, so the ceiling is two
        // reported rounds away rather than three.
        let mut mgr = swiss(8, 4, &env);
        {
            let meta = mgr.get("T").expect("t");
            assert!(
                meta.total_rounds_override.is_none(),
                "this is the computed-default path"
            );
            assert_eq!(meta.total_rounds(), 2);
        }

        mgr.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut mgr, "T", &env);
        mgr.generate_pairings("T", &env).expect("round 2");
        report_all_pending(&mut mgr, "T", &env);

        let before = format!("{:?}", mgr.get("T").expect("t"));
        let err = mgr
            .generate_pairings("T", &env)
            .expect_err("round 3 is past the computed total");
        assert!(err.contains("scheduled for 2 round(s)"), "{err}");
        assert!(err.contains("already at round 2"), "{err}");
        assert_eq!(
            format!("{:?}", mgr.get("T").expect("t")),
            before,
            "the rejected call mutated the tournament"
        );

        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.current_round, 2, "no round was advanced");
        assert_eq!(meta.status, TournamentStatus::InProgress);
    }

    /// The ceiling binds against a *latched* schedule, not a live one. Three
    /// players is the sharp case: the bracket is two rounds deep (4 slots ->
    /// 2 -> 1), but a single round-1 drop leaves two active players, whose
    /// live `default_total_rounds` is one round — so an unlatched recompute
    /// would have the ceiling refuse the bracket's own final.
    #[test]
    fn single_elimination_drop_does_not_shorten_the_latched_final() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        create(
            &mut mgr,
            "SE",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::SingleElimination,
            &env,
        );
        join_n(&mut mgr, "SE", 3, &env);

        mgr.generate_pairings("SE", &env).expect("round 1");
        {
            let meta = mgr.get("SE").expect("t");
            assert!(
                meta.total_rounds_override.is_none(),
                "this is the computed-default path"
            );
            assert_eq!(
                meta.resolved_total_rounds,
                Some(2),
                "round 1 latches the bracket's depth"
            );
            assert_eq!(meta.total_rounds(), 2);
            // Seed 1 takes the bye; seeds 2 and 3 play the semifinal.
            let round_one: Vec<Vec<String>> =
                meta.pairings.iter().map(|p| p.players.clone()).collect();
            assert_eq!(round_one, vec![vec![key(0)], vec![key(1), key(2)]]);
        }

        // A drop mid-round-1: it forfeits the match it leaves one-sided and
        // takes the active field down to two.
        mgr.drop_player("SE", &key(2), &env).expect("drop");
        {
            let meta = mgr.get("SE").expect("t");
            assert_eq!(meta.active_player_count(), 2);
            assert_eq!(
                meta.pairing(1).expect("the semifinal").outcome,
                Some(PairingOutcome::Forfeit { winner: key(1) }),
                "the drop settles the pairing it emptied"
            );
            assert_eq!(
                default_total_rounds(BracketShape::SingleElimination, MatchArity::HEAD_TO_HEAD, 2),
                1,
                "the count an unlatched recompute would now produce"
            );
            assert_eq!(
                meta.total_rounds(),
                2,
                "the drop must not shorten a schedule already being played"
            );
            assert!(meta.first_unresolved_pairing().is_none());
        }

        // ...so the final the bracket still owes actually pairs: the bye
        // recipient against the forfeit winner.
        mgr.generate_pairings("SE", &env)
            .expect("round 2 is the bracket's final");
        {
            let meta = mgr.get("SE").expect("t");
            assert_eq!(meta.current_round, 2);
            let final_round: Vec<Vec<String>> = meta
                .pairings
                .iter()
                .filter(|p| p.round == 2)
                .map(|p| p.players.clone())
                .collect();
            assert_eq!(final_round, vec![vec![key(0), key(1)]]);
        }

        // The ceiling still binds at the latched value: the latch buys back
        // exactly the rounds the bracket was scheduled for, and no more.
        report_all_pending(&mut mgr, "SE", &env);
        let err = mgr
            .generate_pairings("SE", &env)
            .expect_err("round 3 is past a two-round bracket");
        assert!(err.contains("scheduled for 2 round(s)"), "{err}");
    }

    /// The same latch on the Swiss path, where the shrink is a table row
    /// rather than a bracket depth: 9 head-to-head players schedule 4 rounds
    /// (2^4 >= 9) and 8 schedule 3, so one drop would otherwise cancel the
    /// last round of an event already under way. Running all four rounds also
    /// pins the latch's idempotence — a re-latch at round 2 would drop the
    /// count to 3 and make round 4 unpairable.
    #[test]
    fn swiss_drop_does_not_shorten_the_latched_round_count() {
        let env = FakeEnv::new();
        let mut mgr = swiss(9, 2, &env);
        assert_eq!(
            mgr.get("T").expect("t").total_rounds(),
            4,
            "9 players: the smallest r with 2^r >= 9"
        );

        mgr.generate_pairings("T", &env).expect("round 1");
        assert_eq!(
            mgr.get("T").expect("t").resolved_total_rounds,
            Some(4),
            "round 1 latches the computed default"
        );

        mgr.drop_player("T", &key(8), &env).expect("drop");
        {
            let meta = mgr.get("T").expect("t");
            assert_eq!(meta.active_player_count(), 8);
            assert_eq!(
                default_total_rounds(BracketShape::Swiss, MatchArity::HEAD_TO_HEAD, 8),
                3,
                "the count an unlatched recompute would now produce"
            );
            assert_eq!(
                meta.total_rounds(),
                4,
                "the drop must not shorten a schedule already being played"
            );
        }

        // Every scheduled round runs, including the fourth an unlatched
        // recompute would have refused.
        for next in 2..=4u32 {
            report_all_pending(&mut mgr, "T", &env);
            mgr.generate_pairings("T", &env)
                .unwrap_or_else(|e| panic!("round {next}: {e}"));
            let meta = mgr.get("T").expect("t");
            assert_eq!(meta.current_round, next);
            assert_eq!(
                meta.total_rounds(),
                4,
                "round {next} did not re-latch a shorter schedule"
            );
        }

        // ...and the ceiling still binds at the latched value.
        report_all_pending(&mut mgr, "T", &env);
        let err = mgr
            .generate_pairings("T", &env)
            .expect_err("round 5 is past the scheduled total");
        assert!(err.contains("scheduled for 4 round(s)"), "{err}");
        assert!(err.contains("already at round 4"), "{err}");
    }

    /// The three resolution tiers, at the one point where they can disagree.
    /// The organizer's override outranks a latched default exactly as it
    /// outranks a live one, and an event created with an override latches
    /// nothing: there is no computed default in play to freeze.
    #[test]
    fn total_rounds_resolves_override_then_latched_then_live() {
        let env = FakeEnv::new();

        let mut mgr = swiss(9, 2, &env);
        {
            let meta = mgr.get("T").expect("t");
            assert_eq!(
                meta.resolved_total_rounds, None,
                "nothing is scheduled before round 1"
            );
            assert_eq!(meta.total_rounds(), 4, "so the default resolves live");
        }
        mgr.generate_pairings("T", &env).expect("round 1");
        assert_eq!(mgr.get("T").expect("t").resolved_total_rounds, Some(4));

        mgr.meta_mut("T").expect("t").total_rounds_override = Some(11);
        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.total_rounds(), 11, "the override wins outright");
        assert_eq!(
            meta.resolved_total_rounds,
            Some(4),
            "and leaves the latched default untouched underneath it"
        );

        let mut capped = swiss_capped(9, 2, 2, &env);
        capped.generate_pairings("T", &env).expect("round 1");
        let meta = capped.get("T").expect("t");
        assert_eq!(
            meta.resolved_total_rounds, None,
            "an overridden event has no computed default to latch"
        );
        assert_eq!(meta.total_rounds(), 2);
    }

    /// Completing is terminal, so it may not freeze a tournament around a
    /// pairing that could then never be reported.
    #[test]
    fn complete_tournament_rejects_an_unfinished_round() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let pending = mgr.get("T").expect("t").pairings[0].id;

        let err = mgr
            .complete_tournament("T", &env)
            .expect_err("a pending pairing blocks completion");
        assert!(err.contains(&pending.to_string()), "{err}");
        assert_eq!(
            mgr.get("T").expect("t").status,
            TournamentStatus::InProgress,
            "the rejected call left the tournament running"
        );
        // Still running means the pairing is still reportable — the exact
        // property a premature `Completed` would have destroyed.
        report_all_pending(&mut mgr, "T", &env);
        mgr.complete_tournament("T", &env).expect("complete");
        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.status, TournamentStatus::Completed);
        // Deliberately no `current_round >= total_rounds()` gate: ending an
        // event early, once the current round is settled, is the organizer's
        // call (see `complete_tournament`).
        assert!(meta.current_round < meta.total_rounds());
    }

    // -- unit 11: expiry --------------------------------------------------------

    #[test]
    fn expiry_registration_300s() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        create(
            &mut mgr,
            "T",
            MatchArity::HEAD_TO_HEAD,
            BracketShape::Swiss,
            &env,
        );

        env.advance_secs(REGISTRATION_TIMEOUT_SECS);
        assert!(mgr.check_expired(&env).is_empty(), "exactly at the window");
        assert_eq!(mgr.len(), 1);

        env.advance_secs(1);
        assert_eq!(
            mgr.check_expired(&env),
            vec![TournamentExpiryEvent::Deleted("T".to_string())]
        );
        assert!(mgr.is_empty());
    }

    #[test]
    fn expiry_inprogress_untouched_under_7d() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");

        // A real multi-day gap between rounds must not be reaped.
        env.advance_secs(IN_PROGRESS_ABANDON_SECS - 60);
        assert!(mgr.check_expired(&env).is_empty());
        assert_eq!(
            mgr.get("T").expect("t").status,
            TournamentStatus::InProgress
        );
        // The 300-second registration window does not apply once started.
        assert!(mgr.get("T").expect("t").pairings.len() == 2);
    }

    #[test]
    fn expiry_inprogress_abandoned_over_7d() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        let pairings_before = mgr.get("T").expect("t").pairings.clone();

        env.advance_secs(IN_PROGRESS_ABANDON_SECS + 1);
        assert_eq!(
            mgr.check_expired(&env),
            vec![TournamentExpiryEvent::Abandoned("T".to_string())]
        );
        let meta = mgr.get("T").expect("t");
        assert_eq!(meta.status, TournamentStatus::Abandoned);
        assert_eq!(meta.pairings, pairings_before, "history is preserved");
        assert_eq!(meta.last_activity_at, env.now_ms() / 1000);
        // Terminal: mutations are refused.
        assert!(mgr.generate_pairings("T", &env).is_err());
        assert!(mgr.drop_player("T", &key(0), &env).is_err());
    }

    #[test]
    fn expiry_terminal_retention_30d() {
        let env = FakeEnv::new();
        let mut mgr = swiss(4, 2, &env);
        mgr.generate_pairings("T", &env).expect("round 1");
        report_all_pending(&mut mgr, "T", &env);
        mgr.complete_tournament("T", &env).expect("complete");
        assert_eq!(mgr.get("T").expect("t").status, TournamentStatus::Completed);

        // Exactly at the boundary is retained (off-by-one guard).
        env.advance_secs(TERMINAL_RETENTION_SECS);
        assert!(mgr.check_expired(&env).is_empty());
        assert_eq!(mgr.len(), 1);

        env.advance_secs(1);
        assert_eq!(
            mgr.check_expired(&env),
            vec![TournamentExpiryEvent::Deleted("T".to_string())]
        );
        assert!(mgr.is_empty());

        // An abandoned tournament is retained on the same clock, measured
        // from the moment it was abandoned rather than from its last round.
        let env2 = FakeEnv::new();
        let mut abandoned = swiss(4, 2, &env2);
        abandoned.generate_pairings("T", &env2).expect("round 1");
        env2.advance_secs(IN_PROGRESS_ABANDON_SECS + 1);
        assert_eq!(abandoned.check_expired(&env2).len(), 1);
        env2.advance_secs(TERMINAL_RETENTION_SECS);
        assert!(abandoned.check_expired(&env2).is_empty());
        env2.advance_secs(1);
        assert_eq!(
            abandoned.check_expired(&env2),
            vec![TournamentExpiryEvent::Deleted("T".to_string())]
        );
    }

    /// Verification Matrix row 11's hostile fixture: one tournament in EACH
    /// of the four [`TournamentStatus`] variants. `iter()` must yield all
    /// four — proving no status filtering leaked into the accessor, since
    /// filtering (if any) is a caller's job.
    #[test]
    fn iter_yields_every_tournament_regardless_of_status() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();

        // Registration: created, nobody paired.
        create(&mut mgr, "REG", arity(2), BracketShape::Swiss, &env);

        // InProgress: round 1 paired.
        create(&mut mgr, "RUN", arity(2), BracketShape::Swiss, &env);
        join_n(&mut mgr, "RUN", 4, &env);
        mgr.generate_pairings("RUN", &env).expect("round 1");

        // Completed: every pairing resolved, then frozen by the organizer.
        create(&mut mgr, "DONE", arity(2), BracketShape::Swiss, &env);
        join_n(&mut mgr, "DONE", 2, &env);
        mgr.generate_pairings("DONE", &env).expect("round 1");
        let pairing_id = mgr.get("DONE").expect("done").pairings[0].id;
        mgr.report_result(
            "DONE",
            pairing_id,
            PodOutcome::Decisive {
                winner: key(0),
                game_wins: [(key(0), 2u8), (key(1), 0u8)].into_iter().collect(),
            },
            &env,
        )
        .expect("report");
        mgr.complete_tournament("DONE", &env).expect("complete");

        // Abandoned: reached only through the 7-day inactivity transition.
        create(&mut mgr, "GONE", arity(2), BracketShape::Swiss, &env);
        join_n(&mut mgr, "GONE", 4, &env);
        mgr.generate_pairings("GONE", &env).expect("round 1");

        let statuses: Vec<TournamentStatus> = ["REG", "RUN", "DONE", "GONE"]
            .iter()
            .map(|c| mgr.get(c).expect("present").status)
            .collect();
        assert_eq!(
            statuses,
            vec![
                TournamentStatus::Registration,
                TournamentStatus::InProgress,
                TournamentStatus::Completed,
                TournamentStatus::InProgress,
            ],
            "fixture precondition: the four records are in the intended states"
        );

        // Push "GONE" (and only it) into Abandoned. "RUN" is kept alive by a
        // fresh activity stamp so the sweep cannot abandon it too, and "REG"
        // would be deleted outright, so it is re-created after the sweep.
        // "DONE" survives untouched: 7 days is well inside the 30-day terminal
        // retention window, which is what leaves all FOUR statuses present at
        // once for the assertion below.
        env.advance_secs(IN_PROGRESS_ABANDON_SECS + 1);
        // A `const` block: both operands are constants, so the fixture's
        // premise is decidable at compile time and a retention window that
        // shrank below the abandonment window should fail the BUILD rather
        // than wait for someone to run this test. Same idiom as
        // `protocol.rs`'s floor-versus-version guard.
        const {
            assert!(
                IN_PROGRESS_ABANDON_SECS + 1 < TERMINAL_RETENTION_SECS,
                "this fixture needs the terminal window to outlast the abandonment one, \
                 so the Completed record survives the sweep that abandons GONE"
            )
        };
        mgr.report_result(
            "RUN",
            mgr.get("RUN").expect("run").pairings[0].id,
            PodOutcome::Draw,
            &env,
        )
        .expect("keep RUN active");
        let events = mgr.check_expired(&env);
        assert!(events.contains(&TournamentExpiryEvent::Abandoned("GONE".to_string())));
        create(&mut mgr, "REG", arity(2), BracketShape::Swiss, &env);

        let mut seen: Vec<(&str, TournamentStatus)> =
            mgr.iter().map(|m| (m.code.as_str(), m.status)).collect();
        seen.sort_by_key(|(code, _)| *code);

        assert_eq!(
            seen,
            vec![
                ("DONE", TournamentStatus::Completed),
                ("GONE", TournamentStatus::Abandoned),
                ("REG", TournamentStatus::Registration),
                ("RUN", TournamentStatus::InProgress),
            ],
            "iter() must yield every held tournament, in every status"
        );
        assert_eq!(
            mgr.iter().count(),
            mgr.len(),
            "iter() yields each tournament exactly once"
        );
    }

    /// The empty case: a fresh manager's `iter()` yields nothing rather than
    /// panicking or special-casing.
    #[test]
    fn iter_on_an_empty_manager_yields_nothing() {
        let mgr = TournamentManager::new();
        assert_eq!(mgr.iter().count(), 0);
        assert!(mgr.iter().next().is_none());
    }
}
