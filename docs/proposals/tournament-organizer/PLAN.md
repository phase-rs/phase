# Phase 61 — Swiss-Pairing Tournament Organizer — PLAN

This plan captures the rollout sequencing from discussion #5314, refined with
this session's verification findings (CONTEXT.md, RESEARCH.md). No code is
written here — this is the design a future `engine-implementer` pass should
execute against.

## Mandatory architectural sections

### Pattern Coverage — build for the class, not one tournament format

Swiss pairing, tiebreaker math, and bye handling must be built as general
algorithms parameterized by player count and `ScoringPolicy`, not hardcoded
to a specific round count or point values. The MTR round-count table
(Appendix E) is a *default lookup*, not a hardcoded round count — an
organizer overriding total rounds must work identically to the default path.
Single-elimination for 4-8 players (also in Appendix E) is a second bracket
*shape* the same `TournamentManager` must support, not a special case bolted
onto Swiss — see §2 below for how that's scoped.

**Revised this pass — player-count-per-match is a parameterization axis, not
a fork.** The original design implicitly assumed 2 players per match
(head-to-head Standard/Modern/etc. Swiss). Per direct maintainer instruction,
this must also cover Commander/multiplayer pod tournaments (4-player pods
being the common case), and the sibling-cluster smell this repo's CLAUDE.md
warns about applies here exactly: a `TournamentFormat::HeadToHead` +
`TournamentFormat::Commander` split would duplicate pairing, scoring, and
tiebreaker logic across two near-identical code paths that differ only in
one number. Instead, `MatchArity` (§2) is threaded through pairing,
`ScoringPolicy`, and tiebreak order as a single parameter — `arity = 2`
*is* today's design, not a special case of a new one. Full grounding in the
Multiplayer Addendum to the Magic Tournament Rules ("MSTR" — an unofficial,
independent-judge-authored convention this proposal deliberately adopts,
NOT a Wizards of the Coast document; see RESEARCH.md §13) and TopDeck.gg's
production Commander-pairing practice is in RESEARCH.md §13.

### Building Blocks — reused, not reinvented

| Need | Existing block reused | Location |
|------|----------------------|----------|
| Dispatch pattern (`handle(conn, msg, env) -> Vec<Outbound>`) | `Broker::handle` | `crates/lobby-broker/src/broker.rs:136` |
| Per-connection state extension pattern | `ConnState` | `crates/lobby-broker/src/broker.rs:46` |
| Fan-out to subscribers | `Outbound::{AddSubscriber,RemoveSubscriber,ToSubscribers}` | `crates/lobby-broker/src/broker.rs:63-76` |
| Deterministic time/rng for WASM-safe pure core | `BrokerEnv` | `crates/lobby-broker/src/env.rs` |
| Token-based authority surviving a socket bounce | `player_tokens`/`seat_for_token`/`generate_player_token` | `crates/server-core/src/draft_session.rs:27-47`, `session.rs` |
| Native reaper wiring pattern | `broker.reap_expired(300, &SysEnv)` call site | `crates/phase-server/src/main.rs:1000` |
| Deterministic test harness for pure broker logic | `FakeEnv` | used throughout `lobby.rs`/`broker.rs` tests |

New building blocks introduced (all general, none single-card/single-format):
`TournamentManager`, `MatchArity` (2-player through N-player pods, §2),
`ScoringPolicy` (MSTR-formula-derived, arity-aware), `SwissPairing`
(top-to-bottom score-order assignment with swap-based repair, §2 — supersedes
this doc's earlier backtracking-based sketch), `TiebreakOrder` (arity-selected
MTR vs. MSTR computation), organizer/player token minting reusing the same
primitive `draft_session.rs` already uses.

### Logic Placement

All pairing, scoring, tiebreaker, and expiry logic lives in
`crates/lobby-broker/src/tournament.rs` — the pure core. `phase-server` and
the Cloudflare Worker shell are thin dispatch/serialization boundaries, per
CLAUDE.md's "engine [here: broker core] owns all logic" principle applied to
this crate. The frontend renders `TournamentView`/`TournamentStanding`
exactly as computed server-side; it must not recompute standings, tiebreaks,
or pairing itself.

### Extension vs Creation

This extends `lobby-broker`, it does not fork it: `Broker` gains one
additive field (`tournaments: TournamentManager`), `ConnState` gains two
additive fields (token-shaped, not bare identifiers — see §3), and
`LobbyClientMessage`/`LobbyServerMessage` gain additive variants. No existing
lobby behavior changes.

### Analogous Trace

Traced `LobbyManager`/`Broker::handle`/`ConnState` end-to-end (RESEARCH.md
§5) before designing `TournamentManager`'s shape, and traced
`draft_session.rs`'s token model end-to-end (RESEARCH.md §7) before designing
the organizer/player-token authority fix. Both traces are cited with
file:line evidence in RESEARCH.md, not assumed from the discussion's prose.

## 1. `MatchArity` and `ScoringPolicy` — first-class, TO-configurable, MTR/MSTR defaults

```rust
/// Number of players seated at one pairing. `HEAD_TO_HEAD` (2) is today's
/// Standard/Modern/etc. Swiss. `COMMANDER_POD` (4) is the common Commander
/// multiplayer pod size per the Multiplayer Addendum to the MTR ("MSTR" —
/// RESEARCH.md §13). Chosen once at `CreateTournament` time and stored on
/// `TournamentMeta`, immutable for the tournament's lifetime — same
/// rationale as `ScoringPolicy` below (re-sizing pods mid-event would
/// invalidate standings already computed against the old size).
///
/// This is a parameterization axis, not a format enum: pairing (§2),
/// scoring, and tiebreak-order selection are all functions of `MatchArity`
/// rather than forked per format, per CLAUDE.md's "parameterize, don't
/// proliferate" — a `TournamentFormat::HeadToHead` /
/// `TournamentFormat::Commander` split would duplicate three algorithms
/// that differ only in this one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct MatchArity(u8); // REVISED, maintainer review: field is now private —
                           // construction goes through `new()`, never a bare
                           // tuple-struct literal or an unvalidated deserialize.

impl MatchArity {
    pub const HEAD_TO_HEAD: MatchArity = MatchArity(2);
    pub const COMMANDER_POD: MatchArity = MatchArity(4);

    /// NEW, maintainer review — validated construction. Rejects `0`/`1`
    /// (not a real "players per pairing" count — a pairing needs at least
    /// two seats to pair anyone) and caps at `128`, the largest `n` for
    /// which `win_points = 2n - 1` still fits `u8` (`2*128-1 = 255`).
    /// `#[serde(try_from = "u8")]` above routes wire deserialization through
    /// this same constructor — a malformed `CreateTournament` payload is
    /// rejected at the deserialization boundary, not accepted and only
    /// discovered broken later inside pairing/scoring logic.
    pub fn new(n: u8) -> Result<Self, String> {
        if n < 2 {
            return Err(format!(
                "MatchArity must be at least 2 (a pairing needs ≥2 seats), got {n}"
            ));
        }
        if n > 128 {
            return Err(format!(
                "MatchArity {n} exceeds 128 — win_points (2n-1) would overflow u8"
            ));
        }
        Ok(MatchArity(n))
    }

    pub fn get(self) -> u8 {
        self.0
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

/// Tournament match-point scoring. MSTR generalizes MTR §2.1's 3/1/0 to a
/// single formula: `win_points = 2n - 1` for pod size `n` (RESEARCH.md
/// §13) — `n = 2` collapses to exactly 3, so this is the same rule as
/// before, not a fork of it. TO-overridable at tournament creation for
/// communities running variant conventions (e.g. some Old School/93-94
/// groups score draws as 0 to discourage intentional draws; TopDeck.gg's
/// own Commander default is 5/1/0, confirmed distinct from the MSTR-derived
/// 7/1/0 for 4-player pods — see RESEARCH.md §12). Lives in `lobby-broker`,
/// never touches `GameState` — see CONTEXT.md's "Relationship to adjacent
/// work" for why this is NOT a field of the engine crate's
/// `CustomFormatDef`/`LegacyRuleSet` (phase 58).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawScoringPolicy", into = "RawScoringPolicy")]
pub struct ScoringPolicy {
    win_points: u8,  // REVISED, maintainer review: private — see `new()`
    draw_points: u8, // below. `ScoringPolicy` is organizer-overridable at
    loss_points: u8, // `CreateTournament` (this doc's own §1 text, above),
                     // exactly like `MatchArity` — it needed the identical
                     // validated-construction treatment `MatchArity` got in
                     // an earlier round, not a narrower exemption.
}

/// Plain (de)serialization target for the `#[serde(try_from = ...,
/// into = ...)]` boundary — same pattern `MatchArity` uses via
/// `try_from = "u8"`, generalized to a 3-field struct instead of a single
/// scalar. REVISED, maintainer review: must derive `Serialize` too, not
/// only `Deserialize` — `#[serde(into = "RawScoringPolicy")]` on
/// `ScoringPolicy` generates a `Serialize` impl that converts to this type
/// and serializes THAT, so this type not implementing `Serialize` itself
/// means `ScoringPolicy` silently fails to compile, not merely fails to
/// round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawScoringPolicy {
    pub win_points: u8,
    pub draw_points: u8,
    pub loss_points: u8,
}

impl ScoringPolicy {
    /// NEW, maintainer review — validated construction. `win_points == 0`
    /// is rejected here: it is the shared tiebreak floor's denominator
    /// (`1.0 / scoring.win_points as f64`, below) — zero reaches an
    /// invalid (infinite) floor instead of being caught at the boundary
    /// where organizer-supplied configuration actually enters the broker.
    /// Per review scope: this does NOT impose any ordering between
    /// `win_points`/`draw_points`/`loss_points` — organizer overrides are
    /// explicitly supported (some communities score draws as 0, TopDeck.gg
    /// uses a different win value entirely) and a stricter ordering rule
    /// would need its own separately-cited requirement, not an
    /// implicit side effect of this fix.
    pub fn new(win_points: u8, draw_points: u8, loss_points: u8) -> Result<Self, String> {
        if win_points == 0 {
            return Err(
                "win_points must be non-zero — used as the tiebreak floor's denominator".to_string(),
            );
        }
        Ok(Self { win_points, draw_points, loss_points })
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

    /// MSTR-derived default: `2n - 1` match points for a win, `n` being
    /// `MatchArity`. At `arity = HEAD_TO_HEAD` this is exactly MTR §2.1's
    /// 3/1/0 — the same formula, not a special case of it. `arity` is
    /// already validated to `2..=128` by `MatchArity::new` (§1 above), so
    /// the FINAL value `2n - 1` is always in `3..=255` — but REVISED,
    /// maintainer review: the INTERMEDIATE `2 * n` is not (`2 * 128 =
    /// 256`, which overflows `u8` before the subtraction ever runs, even
    /// though the post-subtraction result of `255` would have fit). Fixed
    /// by computing in `u16` and converting down with a checked
    /// conversion, not a bare `as` cast that would silently truncate on a
    /// future arity bound change instead of panicking loudly in a debug
    /// build:
    pub fn default_for_arity(arity: MatchArity) -> Self {
        let n = u16::from(arity.0);
        let win_points = u8::try_from(2 * n - 1)
            .expect("MatchArity::new caps arity at 128, so 2n-1 always fits u8");
        Self { win_points, draw_points: 1, loss_points: 0 }
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
    // MTR §2.1 (equivalently, `default_for_arity(MatchArity::HEAD_TO_HEAD)`).
    fn default() -> Self {
        Self::default_for_arity(MatchArity::HEAD_TO_HEAD)
    }
}
```

Threaded through match-point accumulation (`points_for(&PodOutcome,
ScoringPolicy) -> u32` style helper) and the tiebreaker calculations
identically — a tiebreaker's "total possible points" denominator must use
`scoring.win_points`, not a hardcoded `3`, so a non-default policy's
percentages stay internally consistent. This generalization pays off
immediately for the tiebreaker floor too: MTR's 1v1 floor is a hardcoded
0.33, MSTR's 4-player-pod floor is a hardcoded ~0.14 — both are the *same*
formula, `1.0 / scoring.win_points as f64` (1/3 ≈ 0.33, 1/7 ≈ 0.14), so the
floor needs no arity branch at all. Tiebreaker *order itself* is
arity-selected, not TO-configurable (§2 below) — MTR (official) and MSTR
(the unofficial convention this proposal adopts for multiplayer, RESEARCH.md
§13) use genuinely different tiebreak axes, not just different numbers
plugged into the same axes, so this can't be unified the way the floor was;
ship both fixed to their respective cited values, revisit only if a real
request surfaces.

`MatchArity` and `ScoringPolicy` are supplied once at `CreateTournament`
time and stored on the `TournamentMeta`, immutable for the tournament's
lifetime (changing either mid-event would retroactively corrupt
already-reported standings).

## 2. `TournamentManager` shape

```rust
pub struct TournamentManager {
    tournaments: HashMap<String, TournamentMeta>,
}

pub struct TournamentMeta {
    pub code: String,
    pub name: String,
    pub organizer_token: String,          // minted at creation, NOT socket-bound
    pub arity: MatchArity,                // players per pairing — 2 for head-to-head, 4 for Commander pods
    pub scoring: ScoringPolicy,
    pub bracket: BracketShape,            // Swiss | SingleElimination
    pub total_rounds: u32,                // organizer override, else MTR/MSTR Appendix E default (arity-selected)
    pub current_round: u32,
    pub status: TournamentStatus,         // Registration | InProgress | Completed | Abandoned — see "Expiry" below
    pub players: Vec<TournamentPlayer>,
    pub pairings: Vec<TournamentPairing>,
    pub created_at: u64,
    pub last_activity_at: u64,            // bumped on every mutation — fixes #4615 finding #3
}

pub struct TournamentPlayer {
    pub player_key: String,
    pub player_token: String,             // minted at join, NOT socket-bound — fixes #4615 finding #1
    pub display_name: String,
    pub dropped: bool,
    // NOTE, maintainer review: `had_bye`/`had_short_pod` are NOT stored
    // fields here anymore — see the "Durable pairing history" note below
    // `TournamentPairing` for why they moved to derived queries over
    // `pairings` instead.
}

/// REVISED, maintainer review — `TournamentPairing` gains a stable identity
/// and an explicit, durable outcome slot. The previous sketch (`round` +
/// `players` only) could describe WHO is paired but had nowhere to durably
/// record WHAT happened, even though rematch avoidance (§2 point 2 above)
/// and standings/tiebreak computation (below) both require querying prior
/// results and prior opponents — there was no data for those queries to
/// read. Fixed by making `pairings` (on `TournamentMeta`) the single
/// source of truth every derived view reads fresh, not an incrementally-
/// mutated side structure that a corrected/re-reported result could drift
/// out of sync with:
///
/// - **Durable pairing history.** `pairings: Vec<TournamentPairing>`
///   accumulates every pairing ever generated, across every round — never
///   pruned, never summarized into running totals elsewhere. Match points,
///   opponents faced (rematch avoidance and tiebreak averaging alike),
///   `had_bye`, and `had_short_pod` are ALL derived by scanning this list
///   fresh, not stored as separately-mutated fields anywhere:
///   `fn had_bye(key, pairings) -> bool` (any pairing where
///   `players == [key]` and `outcome == Some(Bye)`),
///   `fn had_short_pod(key, arity, pairings) -> bool` (any pairing sized
///   `arity.get() - 1` containing `key`), `fn prior_opponents(key,
///   pairings) -> HashSet<String>` (every co-`players` entry across every
///   pairing containing `key`). This is what makes "replay-safe result
///   update" well-defined: `report_result(pairing_id, outcome)` is a single
///   `pairings[i].outcome = Some(outcome)` write (a correction simply
///   overwrites the prior value), and every derived view recomputes from
///   the corrected history on its next read — there is no separate cache
///   or running total that a correction could leave stale.
///
/// `players` holds 2 keys for a head-to-head pairing, up to `arity` for a
/// pod pairing; it may be exactly `arity - 1` for a *short pod* (§2 point
/// 4) or exactly 1 for a bye (§2 point 5).
pub struct TournamentPairing {
    pub id: PairingId,                    // stable identity — see below
    pub round: u32,
    pub players: Vec<String>,             // player_key, len in 1..=arity.0
    pub outcome: Option<PairingOutcome>,   // None = pending; Some(_) once resolved
}

/// Stable, tournament-scoped pairing identity — a monotonic counter, not a
/// re-derivable index (pairings are never removed or reordered, so a plain
/// `u32` counter minted at generation time is sufficient; no UUID needed
/// since this never leaves the tournament's own scope).
pub type PairingId = u32;

/// A pairing's resolved outcome, once it has one. REVISED, maintainer
/// review: this now has THREE variants, not the two `PodOutcome` had
/// before — `Bye` and `Forfeit` are pulled OUT of `PodOutcome` entirely
/// rather than smuggled through it, because both are server-assigned
/// facts, never client-reported results, and conflating them with a real
/// reported `PodOutcome` was exactly the ambiguity maintainer review
/// flagged ("does not distinguish a one-player bye from a reported
/// draw/decisive match").
pub enum PairingOutcome {
    /// Auto-assigned the instant a 1-player pairing is generated (§2 point
    /// 5) — never client-reported, never passes through
    /// `validate_match_result`'s reported-outcome path at all.
    Bye,
    /// Auto-assigned when every player but one in a pairing has dropped
    /// before the pairing was reported (§2's drop-timing note below) — the
    /// remaining player wins by forfeit. Also never client-reported.
    Forfeit { winner: String },
    /// A real, client-or-organizer-reported result for a pairing that was
    /// actually played.
    Reported(PodOutcome),
}

/// The reported content of a PLAYED pairing. MSTR: "Match results, not
/// individual Game results, are reported" for pods — a pod match has
/// exactly one winner or is a full draw, never per-seat placement
/// (RESEARCH.md §13). Never represents a bye or a forfeit — see
/// `PairingOutcome` above.
pub enum PodOutcome {
    /// `game_wins` is validated differently by arity (see
    /// `validate_match_result` below): for `HEAD_TO_HEAD`, it MUST contain
    /// exactly the two participants with a legal completed-Bo3 tally; for
    /// a pod (`arity > 2`), it MUST be empty — pods are single-game per
    /// MSTR default, so there is no per-player game-win count to report.
    Decisive { winner: String, game_wins: std::collections::HashMap<String, u8> },
    Draw,
}
```

`BracketShape::SingleElimination` covers the MTR Appendix E 4-8 player case
as a sibling bracket shape on the *same* manager, not a separate feature —
per "build for the class," a tournament tool that only does Swiss is
incomplete relative to the MTR table it cites as its own round-count source.
This applies at `arity = HEAD_TO_HEAD` only, gated to the existing fixed
8-seat case (RESEARCH.md §11).

**Pod-based single elimination (`arity > HEAD_TO_HEAD`) is excluded from v1
— DECIDED, not a live question.** Maintainer review correctly rejected
this section's earlier "in-scope, not designed further" framing: bracket/
advancement semantics for a pod (does a 4-player bracket match have one
winner and three eliminated, or does it need its own seeding/advancement
rules distinct from adjacent-pair 1v1 SE?) were genuinely undesigned, which
is exactly why exclusion — not silent scope creep — is the resolution.
**The scope decision itself (excluded from v1) is final; only the
*underlying design question* (what pod-SE would look like if someone
builds it later) remains open, tracked as future work, not as a blocker on
this proposal.** `v1` ships `BracketShape::SingleElimination` for
`arity = HEAD_TO_HEAD` only; `CreateTournament` must reject
`SingleElimination` paired with `arity != HEAD_TO_HEAD` at construction
time (the same "reject explicitly rather than silently drop data" posture
the sibling custom-format-engine proposal uses for its own unsupported
combinations). Commander/multiplayer pods get Swiss only in v1 — see §5.

**Pairing algorithm — top-to-bottom pod assignment with swap-based repair,
generalized over `arity` (supersedes this doc's earlier backtracking
design).** This session's earlier draft proposed fixing #4615's backtracking
bug in place (a one-line base-case fix). Generalizing to N-player pods
makes that moot: the MSTR convention's own algorithm (an unofficial,
community-authored document this proposal adopts, RESEARCH.md §13) is
itself non-backtracking, and it's the same *shape* CONTEXT.md finding #8
already
recommended adopting from `draft-core`'s existing 1v1 pairing (greedy
top-down assignment + carry/swap repair, no recursive search) — one
generalized algorithm now satisfies both the odd-bracket-bug fix and the
Commander-pod requirement, rather than needing two:
1. Sort active (non-dropped) players descending by current standing (match
   points, then tiebreaks-so-far).
2. Walk top-to-bottom, greedily assigning players into pods of `arity.0`
   in standing order, skipping an assignment that would create *any*
   repeated pair within the pod (not just a repeated full pod — two
   players who've faced each other in a prior pod, even if the rest of the
   pod is new, still count as a rematch, per MSTR). At `arity = 2` this
   is exactly the existing 1v1 score-group pairing.
3. **If the top-to-bottom pass leaves players unassignable** (every
   remaining candidate would create a rematch), iteratively swap an
   unmatched player with one already placed in a pod further down the
   standings — MSTR's own repair step, not a novel design. A player moved
   into a pod above their standing is "paired up"; moved below is "paired
   down" (recorded for organizer visibility only, not scored differently).
4. **Pod-size fallback, `arity > 2` only — REVISED, maintainer review: the
   original "form one short pod" text was a real math error, not a
   simplification.** A single `arity - 1` short pod cannot seat every
   non-divisible player count: at `arity = 4`, 9 players need THREE short
   pods (`3+3+3` — a single 4-pod-plus-one-short-pod only accounts for 7),
   and 10 players need TWO (`4+3+3`, not `4+4+`-with-one-short). Capping at
   "at most one short pod" was simply wrong, not a deliberate scope
   decision.

   **General partition algorithm** (works for any `arity`, not just 4):
   given `n` active players and pod size `arity.0`, find the smallest
   `b ≥ 0` such that `n - b * (arity.0 - 1)` is a non-negative multiple of
   `arity.0`; that `b` is the number of short (`arity.0 - 1`-player) pods,
   and `(n - b * (arity.0 - 1)) / arity.0` is the number of full pods. This
   always finds a solution with `b` in `0..arity.0` when one exists, because
   `arity.0` and `arity.0 - 1` are always coprime (consecutive integers) —
   by the same reasoning the Chicken McNugget/Frobenius bound gives for any
   two coprime pod sizes, the only counts with NO all-`{arity-1,arity}`
   partition are the small ones below `(arity.0 - 1) * (arity.0 - 2)` that
   the formula fails to solve (for `arity = 4`: `n ∈ {1, 2, 5}` — see the
   explicit per-count resolution below, not left as an undefined
   fall-through). Minimizing `b` first (trying `b = 0, 1, 2, ...`
   in order and taking the first fit) is what makes 9 players resolve to
   `3+3+3` (`b=3`, the minimum that works — `b=0,1,2` all fail divisibility)
   and 10 players resolve to `4+3+3` (`b=2`) rather than an arbitrarily
   larger number of short pods.

   Fairness now spreads across however many short pods a round actually
   needs (potentially more than one), not just one: when selecting which
   `b * (arity.0 - 1)` players go into short pods this round, prefer players
   who haven't already had one (`had_short_pod(key, arity, pairings)`, the
   same derived-over-pairing-history query as `had_bye`, not a stored
   field) across the full selection, generalized from "pick one player"
   to "pick the `b`-pod-worth of players." At `arity = 2` there is no
   short-pod case (`arity.0 - 1 == 1`, which is just the existing bye
   path) — this fallback is `arity > 2` only, unchanged.

   **Explicit resolution for `n ∈ {0, 1, 2, 5}` at `arity = 4` — CodeRabbit
   flagged this as undefined; it is not new mechanism, but it does need
   stating explicitly rather than left as an implicit fall-through:**
   - `n = 0`: no active players remain to pair. This isn't a pairing
     outcome at all — it means the tournament has no players left to run a
     round for, which is a tournament-completion condition (§2's broader
     lifecycle), not something this pairing step needs to handle.
   - `n = 1`: the sole active player gets a bye — same single-bye path §2
     point 5 already specifies (scores as a win, excluded from
     opponents'-percentage averaging per point 6 below). No pod forms.
   - `n = 2`: no pod of size 3 or 4 can be formed from 2 players, and no
     single short pod covers it either — this is the one genuine exception
     to "avoid multiple byes": **both players receive a bye this round**,
     each scored via the same existing bye rule (point 6). This is an
     accepted, explicit exception to the "prefer one short pod over
     multiple byes" preference (§13's MSTR citation), not a violation of
     it — that preference exists to avoid multiple byes when a valid pod
     partition is available; at `n = 2` none is.
   - `n = 5`: resolves to one 4-pod plus a single bye for the fifth
     player (the partition algorithm's own `b` search correctly finds no
     `{3,4}`-only solution at `n=5`, so this case's bye is assigned via
     point 5 exactly as for `n=1` — only one bye, not the `n=2` exception,
     since a valid 4-player pod IS available here).
   - **Drops that reduce the active pool BEFORE the next round's pairings
     are generated** (a tournament that started larger can shrink to
     `n ∈ {0,1,2,5}` active players through drops between rounds) use this
     exact same resolution — there is no separate mechanism for "reached
     this count via drops" versus "started at this count." A dropped
     player is simply excluded from the active-player count `n` this
     pairing step operates on, per the existing `TournamentPlayer.dropped`
     field (§2 above); they never appear in that round's pairings or
     standings-affecting bye assignment.
   - **Drops AFTER a round's pairings are already generated, before that
     pairing is reported — a genuinely different timing, REVISED per
     maintainer/CodeRabbit review: the prior text only covered the
     before-generation case and left this one unspecified.** When a drop is
     recorded for a player who is a member of a `Pending` pairing for the
     CURRENT round:
     - **`HEAD_TO_HEAD` (2-player) pairing, one player drops:** the pairing
       auto-settles immediately — the remaining player is awarded
       `PairingOutcome::Forfeit { winner: remaining_player }` (§2's
       `PairingOutcome` enum above), scored identically to a normal win at
       `scoring.win_points`. This is a server-assigned fact, never a client
       report, so it does not pass through `validate_match_result`'s
       reported-outcome path (below) at all — mirroring exactly how a bye
       is assigned, not reported.
     - **Pod (`arity > 2`) pairing, one or more (but not all) players
       drop:** the pod's `Pending` pairing is unaffected — the remaining
       active players still play it out and report a real `PodOutcome`
       when finished, same as if nobody had dropped. The ONE constraint:
       `validate_match_result` (below) rejects a reported `winner` who has
       since dropped — a dropped player cannot be credited a pod win after
       the fact, even if the game was in their favor at the moment they
       left.
     - **Pod pairing where drops leave exactly one active player:** that
       remaining player is awarded `PairingOutcome::Forfeit`, the same
       mechanism as the head-to-head case, generalized — one auto-settled
       forfeit-win for whoever is left, regardless of original pod size.
     - **A pairing already `Some(outcome)` (already reported) before the
       drop is recorded** is never retroactively altered — a later drop
       cannot un-report a finished pairing; this is the same "corrections
       overwrite, history doesn't get silently rewritten by unrelated
       events" property the durable pairing-history design above already
       establishes.
5. Bye assignment (any `arity`, and the only path at `arity = 2`) prefers a
   player who hasn't already had one (`had_bye(key, pairings)`, a derived
   query over the pairing history — see the "Durable pairing history" note
   above `TournamentPairing`, not a stored field) among any players still
   unassignable after pod-size fallback. The generated pairing is a
   1-player `TournamentPairing` whose `outcome` is set to
   `Some(PairingOutcome::Bye)` immediately at generation time — a bye is
   never left `Pending` waiting for a report, since there is nothing to
   report.
6. A bye scores as a win at `scoring.win_points` (2-0/6 game points at
   `arity = 2` per MTR Appendix C; `2n - 1` match points with no game-win
   count at `arity > 2` per MSTR), and the bye round is **excluded** (not
   zero-filled) from that player's opponents'-percentage stats for later
   rounds, since there was no real opponent that round. A short pod is
   **not** a bye — every seated player has real opponents that round and
   is scored normally; only the literal unseated case is a bye.

**Standings / tiebreaker computation — arity-selected order, not a single
shared list.** MTR (2-player) and MSTR (pod) use genuinely different
tiebreak axes, not just different constants plugged into the same axes
(MSTR has no per-player game-win axis at all, since pods are single-game;
it adds an "opponents' average match points" axis 1v1 doesn't have) — so
this is modeled as a `TiebreakOrder` selected by `MatchArity`, not a single
parameterized list:

```rust
pub enum TiebreakOrder {
    /// MTR §3.1 (arity = HEAD_TO_HEAD): match points, opponents'
    /// match-win %, game-win % (floored at `1 / win_points`), opponents'
    /// game-win %.
    HeadToHead,
    /// MSTR (arity > HEAD_TO_HEAD): match points, match-win % (own
    /// bye-adjusted formula, same `1 / win_points` floor), opponents'
    /// average match points, opponents' match-win %.
    Multiplayer,
}

impl TiebreakOrder {
    pub fn for_arity(arity: MatchArity) -> Self {
        if arity == MatchArity::HEAD_TO_HEAD { Self::HeadToHead } else { Self::Multiplayer }
    }
}
```

Both orders share the same generalized floor (`1.0 / scoring.win_points as
f64`, per §1) and both exclude bye rounds (never short-pod rounds) from
opponent-average calculations — those two pieces of logic are genuinely
arity-independent and stay unforked; only the ranked list of *which*
percentages to compute, and in what order, differs.

**Match result validation** — fixes #4615 finding #2 directly, and REWRITTEN
per maintainer review to close two further gaps: (a) a head-to-head report
could previously carry an empty or one-sided `game_wins` map and still
pass, since the Bo3 check only ran `if !game_wins.is_empty()`; (b) nothing
distinguished a client attempting to report a bye/forfeit from a real
played result, since both flowed through the same `PodOutcome` type. Fixed
by operating on `PairingOutcome` (§2) directly — `Bye`/`Forfeit` are
server-assigned and never reach this validator via a client message at
all (the message type for reporting a result only carries a `PodOutcome`
payload, never a bare `PairingOutcome` — see PR 2's protocol variants):
```rust
fn validate_match_result(pairing: &TournamentPairing, result: &PodOutcome) -> Result<(), String> {
    match result {
        PodOutcome::Draw => Ok(()), // MSTR: all seated players draw together
        PodOutcome::Decisive { winner, game_wins } => {
            if !pairing.players.contains(winner) {
                return Err("Winner must be one of the pod's players".to_string());
            }
            if pairing.players.len() == 2 {
                // HEAD_TO_HEAD — REVISED: require EXACTLY the two
                // participant keys with a legal completed-Bo3 tally, not
                // "if present, must be consistent." An empty or
                // single-key game_wins map is now a hard rejection, not a
                // silently-skipped check.
                let (a, b) = (&pairing.players[0], &pairing.players[1]);
                if game_wins.len() != 2 || !game_wins.contains_key(a) || !game_wins.contains_key(b) {
                    return Err(
                        "Head-to-head result must report game wins for exactly both players".to_string(),
                    );
                }
                let (wa, wb) = (game_wins[a], game_wins[b]);
                // Legal completed best-of-three tallies only: someone
                // reaches 2, the other has 0 or 1. Rejects 1-0/0-0 (an
                // unfinished match), 2-2, 3-anything, etc.
                if !matches!((wa, wb), (2, 0) | (2, 1) | (0, 2) | (1, 2)) {
                    return Err(format!("Illegal Bo3 game-win tally {wa}-{wb}"));
                }
                let expected = if wa > wb { a } else { b };
                if winner != expected {
                    return Err("Winner must match the player with more game wins".to_string());
                }
            } else {
                // Pod (arity > 2) — REVISED: game_wins must be EMPTY, not
                // merely unchecked. MSTR pods are single-game; a client
                // attaching game-win data to a pod result has no value for
                // it to mean, so reject rather than silently ignore.
                if !game_wins.is_empty() {
                    return Err(
                        "Pod results are single-game per MSTR — game_wins must be empty".to_string(),
                    );
                }
            }
            Ok(())
        }
    }
}
```
A dropped player can never be the `winner` of a pod result reported after
their drop — `validate_match_result` checks `pairing.players.contains(winner)`
against the pairing's ORIGINAL seat list (drops don't remove a player from
`pairing.players` retroactively), so this needs one more check specific to
the drop-timing case above: reject `winner` if `TournamentPlayer.dropped`
is true for that player at validation time, closing the "credited a win
after leaving" gap the drop-timing note above calls out.

**Expiry / retention — REVISED, maintainer review: the prior text named a
policy ("genuine inactivity" for `InProgress`) without ever defining it, the
owning transition, or a `Completed` retention rule, so the in-memory
`TournamentManager` had no bound on how long it retains ANY tournament that
ever leaves `Registration`.** Fixes #4615 finding #3 and completes the
lifecycle with four explicit rules, one per status, all keyed off the same
`last_activity_at` (bumped on every mutating call — `join_tournament`,
`start_round`, `report_result`, `drop_player`, AND, new here, every status
transition itself, so `last_activity_at` doubles as "time of the most
recent state change" for retention purposes without a second timestamp
field):

1. **`Registration`** — unchanged from the original design.
   `check_expired` reaps (deletes) a `Registration`-status tournament once
   `last_activity_at` exceeds the same 300-second window the existing
   lobby reaper already uses (`broker.reap_expired(300, &SysEnv)`,
   `crates/phase-server/src/main.rs:1000`) — an abandoned registration
   (organizer created it, nobody ever joined or started it) is exactly the
   same shape of staleness the lobby's own 300s window already handles.
   **REVISED, maintainer review: this is a deletion path exactly like the
   30-day `Completed`/`Abandoned` retention deletion below, and needs the
   identical delivery contract — the omission wasn't a deliberate
   distinction, it was this rule simply predating the delivery-contract
   section entirely (it was already in the doc before that section was
   added) and never getting swept into it.** `check_expired` emits
   `Outbound::ToSubscribers(LobbyServerMessage::TournamentRemoved { code
   })` plus `TournamentListUpdate` for a reaped `Registration` tournament
   too, same as any other deletion — see "Expiry event delivery" below,
   which now covers all three deletion/transition outcomes uniformly, not
   two of three.
2. **`InProgress` — NEW, the previously-undefined case.** "Genuine
   inactivity" is defined as `last_activity_at` exceeding **7 days**
   (chosen to comfortably exceed any real multi-round Swiss event's
   natural between-round gaps — human coordination, players returning
   later the same day or the next — while still eventually reclaiming a
   tournament nobody is actually running anymore). The owning transition:
   `check_expired` moves the tournament to `TournamentStatus::Abandoned`
   (a NEW status, not a deletion — the record and its full pairing/
   standings history are preserved, only the "still live" status ends).
   This threshold is a system default, NOT organizer-overridable (unlike
   `total_rounds`) — it's server hygiene, not a tournament rule a TO would
   reasonably want to tune.
3. **`Completed` / `Abandoned` — NEW, the retention rule maintainer review
   flagged as entirely missing.** Both are terminal states whose standings
   are frozen (a genuinely finished event and an abandoned one are kept
   distinct — see `Abandoned`'s own doc comment below — but both stop
   accepting mutations the same way). `check_expired` deletes a tournament
   in either state once `last_activity_at` (i.e., time of completion or
   abandonment) exceeds a **30-day retention period** — long enough for
   players/organizers to look up final standings after the fact, bounded
   enough that the in-memory `HashMap` doesn't grow forever.

```rust
pub enum TournamentStatus {
    Registration,
    InProgress,
    /// All rounds finished normally, standings frozen — a trustworthy
    /// final result.
    Completed,
    /// NEW — reached only via `check_expired`'s 7-day `InProgress`
    /// inactivity transition (point 2 above), never organizer-initiated.
    /// Distinct from `Completed`: an abandoned tournament's final round(s)
    /// may still have `Pending` pairings, so its "standings" reflect
    /// whatever was actually reported before activity stopped, not a
    /// guaranteed-complete result — kept as its own status specifically so
    /// clients can display that distinction rather than presenting an
    /// abandoned event's partial standings as equivalent to a real
    /// `Completed` one.
    Abandoned,
}
```

Both new rules are exercised by `check_expired` alone — no new operation
or background job beyond the reaper this design already calls for
`Registration` (`main.rs:1000`'s existing reap site just needs to widen its
scope to also cover `InProgress`/`Completed`/`Abandoned` on every sweep,
not add a second timer).

**Expiry event delivery — NEW, maintainer review: the lifecycle rules above
had no typed outbound/fan-out contract, so a reaper sweep could mutate
durable tournament state while every connected client's view silently went
stale.** The existing lobby reaper is the concrete precedent to mirror, not
a novel mechanism: `Broker::reap_expired` (`crates/lobby-broker/src/
broker.rs:306-314`) already maps each expired lobby game to
`Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved{game_code})`,
and the native shell's reaper timer (`main.rs`'s periodic sweep, ~line
2095) already recovers those codes from the returned outbounds and fans
them out to `bg_lobby_subs` — the shared subscriber list — via the same
generic `Outbound::ToSubscribers` path every other broker message uses.
Tournament expiry needs the identical shape, generalized to two outcomes:

1. **`InProgress` → `Abandoned` (record persists, status changes)** emits
   `Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { code,
   view })` — `TournamentUpdate` and `TournamentView` are already named in
   this proposal's own protocol surface (RESEARCH.md §1, from #4612's
   original design) but were never wired to expiry specifically; this is
   the same authoritative full-state push a manual `ReportMatchResult`
   would already trigger, not a new message shape invented for this case.
2. **Any deletion path — REVISED, maintainer review: this must cover all
   three, not just the two newest ones** (`Registration` expiry at 300s,
   same as always; `Completed`/`Abandoned` past the 30-day retention
   window) — emits `Outbound::ToSubscribers(LobbyServerMessage::
   TournamentRemoved { code })` — a new variant, but named identically to
   the existing `LobbyGameRemoved` precedent it mirrors, for the same
   reason: a client holding a stale view of a now-gone tournament needs an
   explicit "this no longer exists" signal, not silence, regardless of
   which status it was deleted from.
3. **All three outcomes (the transition and both deletion cases)
   additionally emit `Outbound::ToSubscribers(
   LobbyServerMessage::TournamentListUpdate { .. })`** (also already named
   in RESEARCH.md §1) so any tournament-browser/list view reflects the
   status change or removal, not just a client already viewing that one
   tournament's detail page.

`Broker::reap_expired` widens to call `self.tournaments.check_expired(env)`
in the same sweep as the existing `self.lobby.check_expired(...)` call,
appending these new outbound kinds to the same returned `Vec<Outbound>` —
one reaper, one call site, both lobby games and tournaments fanned out
through it, exactly matching this section's "no second timer" framing
above. PR 2's native wiring (below) extends the SAME `main.rs` reap block
that today only recovers `LobbyGameRemoved`-shaped codes to also recover
and dispatch these new variants through `bg_lobby_subs`. PR 3's Cloudflare
Worker shell needs the identical widening on the Durable Object's alarm
path (the WASM-side equivalent of the native timer) — see PR 3 below.

## 3. Authority model — organizer/player tokens, not socket identity

Fixes #4615 finding #1 directly, using the `draft_session.rs` precedent
(RESEARCH.md §7) as the concrete pattern to mirror:
- `CreateTournament` mints an `organizer_token` (reuse
  `generate_player_token()` from `server-core::session`, the same primitive
  draft pods already use) and returns it to the creating client. The
  *tournament's* record of who the organizer is keys off this token, not
  `ConnState`/the socket.
- `JoinTournament` mints a `player_token` per joining player, same mechanism.
- `ConnState` gains `organized_tournaments: Vec<String>` /
  `joined_tournaments: Vec<(String, String)>` (tournament code, player_token)
  purely as **local reconnect convenience** (so a client that already has a
  token doesn't need to re-type it) — never as the source of authority.
  Organizer-gated actions (`StartTournamentRound`, `EndTournament`) validate
  the *token* presented in the message payload against
  `TournamentMeta.organizer_token`; player-gated actions
  (`ReportMatchResult`, `DropFromTournament`) validate against the paired
  `TournamentPlayer.player_token`.
- Consequence: closing/reopening the tournament page's socket does **not**
  unregister the tournament or drop a player's standing — exactly the bug
  class #4615 shipped. The tournament page keeps one socket open for the
  page's lifetime purely as a live-update optimization, per the discussion's
  framing, not because closing it would be unsafe.

## 4. Proposed rollout — four independently reviewable PRs

Unchanged in sequencing from the discussion, refined with this session's
corrections folded in as explicit acceptance criteria per PR:

**PR 1 — pure core, no wiring.**
`crates/lobby-broker/src/tournament.rs`: `TournamentManager`, `MatchArity`,
`ScoringPolicy`, top-to-bottom pod pairing with swap-based repair
(generalized over `arity`, per §2), single-elimination bracket shape, bye
and short-pod assignment, MTR/MSTR-cited standings via arity-selected
`TiebreakOrder`. Exhaustively unit tested with `FakeEnv`, same style as
`lobby.rs`/`broker.rs`. **`MatchArity` must be modeled from this PR's first
commit, not retrofitted in a later PR** — pairing, scoring, and tiebreak
order are all designed as functions of it from day one.
**Acceptance criteria beyond "compiles and has tests":**
- A dedicated test for an odd-sized `HEAD_TO_HEAD` bracket (5, 7, 9 players)
  asserting the swap-repair path succeeds without falling back to a
  rematch-prone pairing — this is the exact case #4615's 90 passing tests
  missed.
- A dedicated test for `COMMANDER_POD` arity with a player count that
  doesn't divide evenly by 4 — including 9 players (must resolve to three
  short pods, `3+3+3`, not one) and 10 players (must resolve to `4+3+3`) —
  asserting §2's partition algorithm produces the correct minimum-short-pod
  count for each, no bye is issued when a short-pod-only partition exists,
  and `had_short_pod` fairness spreads correctly across however many
  players a round actually shorts (not just tracking a single player).
- **NEW — CodeRabbit**: a dedicated test for each `COMMANDER_POD`-arity
  degenerate active-player count — `n = 1` (single bye, no pod, scored per
  point 5), `n = 2` (both players receive a bye — the one explicit
  multiple-bye exception, both scored per point 5, no pod attempted), and
  `n = 5` (one 4-pod plus exactly one bye, not the `n=2` multi-bye case) —
  plus a test confirming a mid-tournament drop that reduces the active
  count to one of these values (e.g. 6 active players, 1 drops mid-event,
  leaving 5 for the next round) resolves identically to starting a round
  at that count directly, proving drops aren't a separate code path.
- A dedicated test asserting `validate_match_result` rejects a
  `HEAD_TO_HEAD` report where game-win counts differ but `winner` names the
  side with fewer game wins, AND a test confirming a `COMMANDER_POD`
  `Decisive` result with no `game_wins` validates on winner-membership alone
  (no spurious game-win check at pod arity).
- A dedicated test asserting `TiebreakOrder::for_arity` selects `HeadToHead`
  at arity 2 and `Multiplayer` at arity > 2, and that both orders' floor
  computes as `1.0 / scoring.win_points` (0.33 at arity 2, ~0.14 at arity 4)
  rather than a hardcoded constant.
- Test vectors pinned against the MTR's and MSTR's own worked examples where
  practical (per the discussion's proposal).
- No protocol changes, no shell wiring — reviewable purely as an algorithm +
  state machine, independent of PRs 2-4.

**PR 2 — protocol + native server.**
`protocol.rs` new `LobbyClientMessage`/`LobbyServerMessage` variants and view
types (`TournamentView`, `TournamentSummary`, `TournamentStanding`,
`PairingView` — `PairingView.players: Vec<PlayerSummary>`, not a fixed
`player_a`/`player_b` pair, so the wire shape itself doesn't bake in
head-to-head), `CreateTournament`'s payload carries `arity: MatchArity`
alongside `scoring`/`bracket`. **Protocol version bump — REVISED, maintainer
review: this text previously said "from 13 to 14," which was stale even
against this document's OWN CONTEXT.md finding #4 (which had already
retracted that framing) and is stale again against current `main` regardless
— `PROTOCOL_VERSION`/`LOBBY_PROTOCOL_VERSION` are fast-moving constants that
will have moved again by the time anyone implements this.** The durable
instruction, independent of whatever the numbers are on any given day:
bump `LOBBY_PROTOCOL_VERSION` (`crates/lobby-broker/src/protocol.rs`) by
one from its value at implementation time — re-read the constant then, do
not trust a number cited in this document — because `TournamentManager`'s
new message variants are lobby-scoped. Do NOT bump the general
`PROTOCOL_VERSION`, which is for `GameState`/`GameAction` wire changes this
proposal doesn't make (see CONTEXT.md finding #4 for the full architectural
reasoning, which remains correct even as the specific numbers churn).
`broker.rs` gains the `tournaments: TournamentManager` field
and the `ConnState` organizer/joined-tournament fields **as tokens** (§3
above, not bare identifiers). Native server dispatch arms in
`phase-server/src/main.rs`. **Reaper — REVISED, maintainer review: the
prior text here said the reaper was "restricted to `Registration`-status
tournaments," which directly contradicted the all-status lifecycle §2
itself specifies, and would have left the 7-day `InProgress`→`Abandoned`
transition and the 30-day `Completed`/`Abandoned` retention window entirely
unwired.** There is exactly ONE `check_expired` sweep, and it implements
all three rules from §2's "Expiry / retention" note on every call — reap
stale `Registration` (300s), transition stale `InProgress` to `Abandoned`
(7 days), and delete retained `Completed`/`Abandoned` records (30 days).
PR 2 wires this single all-status sweep into the existing native reaper
call site (`main.rs:1000`) — it does not add a second reaper, a second
timer, or a status-restricted variant. **NEW, maintainer review — delivery
contract:** PR 2's protocol variants (`LobbyServerMessage::TournamentUpdate`
/ `TournamentRemoved` / `TournamentListUpdate`, §2's "Expiry event
delivery" note) are exactly what the native reap block's existing
`main.rs:2095`-area logic gains a second recovery arm for, alongside its
existing `LobbyGameRemoved` handling — the SAME `bg_lobby_subs` fan-out
loop, not a separate broadcast path, since tournament messages are
lobby-scoped and share that subscriber list.

**PR 3 — Cloudflare Worker shell.**
`lobby-worker/broker-wasm` `mutates_lobby` match extension, `lobby-do.ts`
`ConnAttachment` extension, and **the corrected `is_empty()` predicate**:
```rust
pub fn is_empty(&self) -> bool {
    self.inner.lobby().is_empty() && self.inner.tournaments().is_empty()
}
```
confirmed today's `is_empty()` (`lobby-worker/broker-wasm/src/lib.rs:168-170`)
only checks `self.inner.lobby()` — this predicate MUST be updated in the same
PR that adds the `tournaments` field to `Broker`, not deferred, or a
tournament-only Durable Object will stop rescheduling its cleanup alarm.
**NEW, maintainer review:** the Durable Object's alarm handler — the
WASM-side equivalent of PR 2's native timer loop — needs the identical
widening: it must recover `TournamentUpdate`/`TournamentRemoved`/
`TournamentListUpdate` outbounds from the same widened `reap_expired` call
and fan them out via its own connection-broadcast mechanism, not just
process the pre-existing `LobbyGameRemoved` case. Without this, the
Cloudflare-hosted path silently diverges from the native server's delivery
behavior — durable state changes, but only native-server-connected clients
learn about it
(exactly #4615's finding #4).

**PR 4 — frontend.**
`adapter/types.ts` mirrors, `tournamentClient.ts`, `TournamentLandingPage.tsx`
/`TournamentPage.tsx` + presentational components, i18n keys from day one, routes/nav
entry. **Acceptance criteria beyond "renders":**
- Every RPC handler must close its socket via a single shared
  connect/request/close wrapper (or a `finally` block) — #4615 leaked a
  socket on the error path of every single RPC handler (six confirmed
  instances, RESEARCH.md §3b), not just one; design the client so this bug
  class is structurally impossible rather than manually replicating
  `finally { client?.close(); }` six times.
- Explicit socket-close on unmount, including the cancelled-connect race
  (the `cancelled` flag pattern from #4615's own code was present but
  incomplete — it set the flag but never called `.close()` on the socket
  that resolved after cancellation).
- All new UI chrome routed through `t()` per `client/src/i18n/README.md`
  from the first commit — no English-only strings landing then retrofitted.

## 5. Explicitly out of scope for v1 (unchanged from discussion, confirmed not contradicted by this session's findings)

- Full-mode (server-managed `GameSession`) auto-launch per pairing — v1
  stays lobby-only, matching #4612's original framing exactly.
- Central validation of *reported score content* beyond winner/game-win
  consistency (§2's `validate_match_result` fix) — MTR's own model is
  self-reporting (§2.4), and the broker already trusts client-supplied
  identity elsewhere (deck contents aren't server-validated either, per
  #4612's own stated limitation). The authority-token fix (§3) is about
  *who* may report, not fact-checking *what* they report.
- `ScoringPolicy` hooking into the custom-format-engine (phase 58) —
  see CONTEXT.md's "Relationship to adjacent work." Recommend building
  `ScoringPolicy` standalone in `lobby-broker` regardless of phase 58's
  landing order.
- Auto-advance round timers — leave organizer-gated for v1 pending
  maintainer input; additive later either way.
- Per-seat placement scoring within a pod (2nd/3rd/4th tracked separately,
  rather than just winner/everyone-else) — MSTR itself doesn't track this
  ("no placement tracking beyond winner/loser status" within a single pod
  match, RESEARCH.md §13); `PodOutcome` deliberately has no room for it.
  Revisit only if a real request surfaces, same as the other MSTR/MTR
  fixed-value decisions above.
- Variable pod size *within* a single round beyond what §2's partition
  algorithm requires (e.g. deliberately mixing 3- and 4-player pods for
  reasons other than seating an indivisible player count — an organizer
  preference lever, not a seating-math necessity) — v1 always uses the
  minimum number of short pods the partition algorithm computes, never an
  arbitrary organizer-chosen mix; anything more elaborate is unneeded
  product scope for a first cut.
- **Pod-based single elimination (`arity > HEAD_TO_HEAD`, §2) — maintainer
  review correctly rejected an earlier "in-scope, not designed further"
  framing for this.** Bracket/advancement semantics for a multi-player pod
  bracket are a real, unresolved design question (CONTEXT.md open question
  #4), not a mechanical extension of 1v1 SE's existing 8-seat gate.
  `CreateTournament` rejects `SingleElimination` + `arity != HEAD_TO_HEAD`
  at construction time. Commander/multiplayer pods get Swiss only in v1.

## 6. Testing

Per CLAUDE.md's "test the building block, not the special case": tests
target `TournamentManager`'s pairing/standings/expiry functions across their
full parameter range — varying player counts including odd/even, varying
`ScoringPolicy` values including a non-default draw-value policy, varying
bracket shapes, **and varying `MatchArity` (2 and 4 at minimum, ideally also
an uncommon size like 3 or 6 to confirm nothing hardcodes "4")** — not a
single fixed "8-player Swiss tournament" happy-path test. The specific
regression tests called out in PR 1's acceptance criteria (odd-bracket
swap-repair, winner/game-win consistency, short-pod fairness,
arity-selected tiebreak order) exist precisely because #4615's existing 90
tests didn't cover the arity-2 cases, and nothing existed yet to cover
arity > 2 at all; any building block introduced here needs a test that
would have caught each of the seven review findings plus the Commander-pod
gap, not just tests for the successful head-to-head path.

**NEW — maintainer review**: a dedicated test asserting `CreateTournament`
rejects `BracketShape::SingleElimination` paired with any
`arity != MatchArity::HEAD_TO_HEAD` at construction time (§2/§5) — the
concrete regression test proving pod-based SE's exclusion from v1 is an
enforced construction-time rejection, not just a documentation note that a
future implementer could silently miss.

**NEW — maintainer review (`MatchArity` validated construction, §1)**:
`MatchArity::new(0)` and `::new(1)` both return `Err`; `MatchArity::new(129)`
returns `Err` (the first value where `2n-1` would overflow `u8`);
`MatchArity::new(128)` succeeds and `ScoringPolicy::default_for_arity`
produces `win_points: 255` without panicking or wrapping — the concrete
regression test for the exact overflow maintainer review identified.
Deserializing a `CreateTournament` payload with `arity: 0` in its wire
JSON is rejected at deserialization (via `#[serde(try_from = "u8")]`), not
accepted and only discovered broken later.

**NEW — maintainer review (`ScoringPolicy` validated construction, §1)**:
the identical sibling test for the identical sibling gap — `ScoringPolicy::
new(0, 1, 0)` returns `Err`; `new(3, 1, 0)` succeeds; a `CreateTournament`
payload whose wire JSON carries `scoring.win_points: 0` is rejected at
deserialization (via `#[serde(try_from = "RawScoringPolicy")]`), never
reaching the tiebreak floor's `1.0 / scoring.win_points as f64` division as
a live `NaN`/`inf` value. `new(3, 1, 0)` and every other combination with
`win_points > 0` succeeds regardless of the relationship between
`win_points`/`draw_points`/`loss_points` — this validation is deliberately
scoped to the one value that's structurally unsafe (a zero denominator),
not a policy opinion about which combinations of the three are sensible
tournament rules.

**NEW — maintainer review (durable pairing history and replay-safe
correction, §2)**: a test that reports a result for a pairing, then reports
a DIFFERENT result for the same `pairing_id` (a correction), and asserts
every derived view (standings, that pairing's own `outcome`, both players'
`prior_opponents`) reflects only the corrected result — no residue from
the first report anywhere, proving the derive-from-history design is
genuinely replay-safe and not just replay-tolerant for the exact fields
someone thought to reset. A second test asserts `had_bye`/`had_short_pod`/
`prior_opponents` all correctly derive from a multi-round pairing history
with no separate stored field to fall out of sync.

**NEW — maintainer review (drop-timing forfeit resolution, §2)**: three
dedicated tests — (a) a `HEAD_TO_HEAD` pairing where one player drops while
`Pending` auto-resolves to `PairingOutcome::Forfeit` for the remaining
player, scored as a normal win; (b) a pod pairing where one of four players
drops while `Pending` does NOT auto-resolve — the pod stays `Pending` and
a subsequent real `PodOutcome` report for the remaining players is
accepted normally; (c) `validate_match_result` rejects a reported `winner`
whose `TournamentPlayer.dropped` is true, even when that player is
otherwise a legitimate member of `pairing.players`.

**NEW — maintainer review (tightened head-to-head result validation, §2)**:
a dedicated test matrix for `validate_match_result` covering every illegal
`game_wins` shape for a `HEAD_TO_HEAD` pairing — empty map (rejected, was
previously silently accepted), single-key map (rejected), a legal-looking
but incomplete tally like `1-0` (rejected, not a completed Bo3), `2-2`
(rejected), and each of the four legal completed tallies (`2-0`, `2-1`,
`0-2`, `1-2`) accepted with the correct winner and rejected with the wrong
one. A sibling test confirms a pod (`arity > 2`) result with a non-empty
`game_wins` is rejected outright, and one with an empty map validates
normally.

**NEW — maintainer review (tournament lifecycle retention, "Expiry" §2)**:
four dedicated tests, one per status — (a) a `Registration` tournament with
`last_activity_at` past the 300s window is deleted by `check_expired`,
unchanged from the existing behavior; (b) an `InProgress` tournament with
`last_activity_at` just under 7 days is untouched (proving a real
multi-day gap between rounds doesn't get reaped); (c) an `InProgress`
tournament past the 7-day threshold transitions to `Abandoned` — record
preserved, `pairings`/standings unchanged, only `status` and
`last_activity_at` updated by the transition itself; (d) a `Completed` (or
`Abandoned`) tournament past the 30-day retention window is deleted
outright by `check_expired`, while one still within the window is
untouched — proving the previously-missing terminal-state retention rule
is actually enforced, not just documented.

**NEW — maintainer review (expiry event delivery, §2)**: dedicated
discriminating tests for EACH of the three outcomes' outbound contract,
not just its state change — (a) the `InProgress`→`Abandoned` transition's
returned `Vec<Outbound>` contains a `TournamentUpdate` with the updated
(Abandoned) view AND a `TournamentListUpdate`, not just a bare state
mutation; (b) the 30-day `Completed`/`Abandoned` terminal-deletion path's
returned outbounds contain a `TournamentRemoved` AND a `TournamentListUpdate`;
(c) **the 300-second `Registration`-expiry deletion path — NEW, maintainer
review, previously untested alongside the other two — returns the
identical `TournamentRemoved` + `TournamentListUpdate` pair**, not a bare
deletion with no outbound at all (this is the exact gap review caught: the
Registration path predates the delivery-contract section and was never
swept into its test coverage either); (d) a native-server integration test
confirming the widened `main.rs` reap block recovers and dispatches all
three outcomes' variants to `bg_lobby_subs` alongside its existing
`LobbyGameRemoved` handling, not just some of them; (e) the equivalent
Cloudflare Worker test confirming the Durable Object alarm path fans out
the same variants through its own broadcast mechanism for all three
outcomes — proving PR 3 doesn't silently diverge from PR 2's delivery
behavior for any of them.
