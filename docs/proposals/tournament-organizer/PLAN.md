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
official Multiplayer Addendum to the Magic Tournament Rules (MSTR) and
TopDeck.gg's production Commander-pairing practice is in RESEARCH.md §13.

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
pub struct MatchArity(pub u8);

impl MatchArity {
    pub const HEAD_TO_HEAD: MatchArity = MatchArity(2);
    pub const COMMANDER_POD: MatchArity = MatchArity(4);
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
pub struct ScoringPolicy {
    pub win_points: u8,
    pub draw_points: u8,
    pub loss_points: u8,
}

impl ScoringPolicy {
    /// MSTR-derived default: `2n - 1` match points for a win, `n` being
    /// `MatchArity`. At `arity = HEAD_TO_HEAD` this is exactly MTR §2.1's
    /// 3/1/0 — the same formula, not a special case of it.
    pub fn default_for_arity(arity: MatchArity) -> Self {
        Self { win_points: 2 * arity.0 - 1, draw_points: 1, loss_points: 0 }
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
arity-selected, not TO-configurable (§2 below) — MTR and MSTR use genuinely
different tiebreak axes, not just different numbers plugged into the same
axes, so this can't be unified the way the floor was; ship both fixed to
their respective official values, revisit only if a real request surfaces.

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
    pub status: TournamentStatus,         // Registration | InProgress | Completed
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
    pub had_bye: bool,                    // bye assignment prefers players who haven't had one
    pub had_short_pod: bool,               // NEW: arity > 2 only — see pod-sizing below
}

/// A single pairing. 2 players for a head-to-head match. Up to `arity`
/// players for a pod match; may be exactly `arity - 1` for a *short pod*
/// (MSTR: prefer one undersized pod over multiple byes — see below) or
/// exactly 1 for a genuine bye.
pub struct TournamentPairing {
    pub round: u32,
    pub players: Vec<String>,             // player_key, len in 1..=arity.0
}

/// One pairing's reported outcome. MSTR: "Match results, not individual
/// Game results, are reported" for pods — a pod match has exactly one
/// winner or is a full draw, never per-seat placement (RESEARCH.md §13).
pub enum PodOutcome {
    /// `game_wins` is only meaningful (and only validated) for a
    /// `HEAD_TO_HEAD` pairing's Bo3 game-win consistency check below —
    /// pods are single-game per MSTR default, so it's empty/unused there.
    Decisive { winner: String, game_wins: std::collections::HashMap<String, u8> },
    Draw,
}
```

`BracketShape::SingleElimination` covers the MTR Appendix E 4-8 player case
as a sibling bracket shape on the *same* manager, not a separate feature —
per "build for the class," a tournament tool that only does Swiss is
incomplete relative to the MTR table it cites as its own round-count source.
Single elimination for `arity > 2` (Commander bracket play) composes the
same way: each bracket "match" is a pod of `arity` players advancing its one
winner, rather than the adjacent-pair advancement 1v1 SE uses — noted here
as in-scope, not designed further, since v1's SE path is already gated to a
fixed 8-seat case (RESEARCH.md §11) and the pod generalization is a
natural, not novel, extension of that same gate.

**Pairing algorithm — top-to-bottom pod assignment with swap-based repair,
generalized over `arity` (supersedes this doc's earlier backtracking
design).** This session's earlier draft proposed fixing #4615's backtracking
bug in place (a one-line base-case fix). Generalizing to N-player pods
makes that moot: the official MSTR algorithm (RESEARCH.md §13) is itself
non-backtracking, and it's the same *shape* CONTEXT.md finding #8 already
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
4. **Pod-size fallback, `arity > 2` only**: prefer as many full `arity`-size
   pods as possible; when the active-player count doesn't divide evenly,
   form one *short pod* of `arity.0 - 1` rather than issuing more than one
   bye (MSTR: "pods may consist of a minimum of 3 players to avoid multiple
   byes" for 4-player events). Prefer assigning the short pod to a player
   who hasn't already had one (`had_short_pod`), mirroring bye fairness —
   a player should not be shorted twice before every other player has been
   shorted once, same fairness rule as `had_bye`. At `arity = 2` there is no
   short-pod case (`arity.0 - 1 == 1`, which is just the existing bye path).
5. Bye assignment (any `arity`, and the only path at `arity = 2`) prefers a
   player who hasn't already had one (`had_bye` flag) among any players
   still unassignable after pod-size fallback.
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

**Match result validation** — fixes #4615 finding #2 directly, generalized
to `PodOutcome`/arbitrary pod size:
```rust
fn validate_match_result(pairing: &TournamentPairing, result: &PodOutcome) -> Result<(), String> {
    match result {
        PodOutcome::Draw => Ok(()), // MSTR: all seated players draw together
        PodOutcome::Decisive { winner, game_wins } => {
            if !pairing.players.contains(winner) {
                return Err("Winner must be one of the pod's players".to_string());
            }
            // Bo3 game-win consistency check applies ONLY to head-to-head
            // pairings — MSTR pods are single-game, there's no per-player
            // game-win count to cross-check.
            if pairing.players.len() == 2 && !game_wins.is_empty() {
                let (a, b) = (&pairing.players[0], &pairing.players[1]);
                if game_wins.get(a) != game_wins.get(b) {
                    let expected = if game_wins.get(a) > game_wins.get(b) { a } else { b };
                    if winner != expected {
                        return Err("Winner must match the player with more game wins".to_string());
                    }
                }
            }
            Ok(())
        }
    }
}
```

**Expiry** — fixes #4615 finding #3 directly: `last_activity_at` bumped on
every mutating call (`join_tournament`, `start_round`, `report_result`,
`drop_player`), and `check_expired` filters on `last_activity_at`, not
`created_at`. Additionally, per the discussion's proposal, staleness reaping
should be restricted to `TournamentStatus::Registration` (abandoned
registration, nobody ever started) — an `InProgress` tournament should not
be reaped by a fixed timeout at all, only by genuine inactivity, since a
real Swiss event with human players between rounds can easily exceed any
reasonable fixed timeout.

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
  doesn't divide evenly by 4 (e.g. 9, 10, 11 players), asserting a single
  short pod (not multiple byes) forms, and that `had_short_pod` fairness
  prevents the same player being shorted twice before every player has been
  shorted once.
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
alongside `scoring`/`bracket`, protocol version bump — **from 13 to 14** (confirmed current
value is 13, not 12 — see CONTEXT.md finding #4; do not copy #4615's stale
"v12" framing). `broker.rs` gains the `tournaments: TournamentManager` field
and the `ConnState` organizer/joined-tournament fields **as tokens** (§3
above, not bare identifiers). Native server dispatch arms in
`phase-server/src/main.rs`. Reaper keyed off `last_activity_at`, restricted
to `Registration`-status tournaments (§2 above).

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
tournament-only Durable Object will stop rescheduling its cleanup alarm
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
- Variable pod size *within* a single round beyond the one-short-pod
  fallback (e.g. deliberately mixing 3- and 4-player pods for reasons other
  than an indivisible player count) — v1 always prefers full `arity`-size
  pods and falls back to at most one short pod per round, per MSTR's own
  stated preference; anything more elaborate is unneeded product scope for
  a first cut.

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
