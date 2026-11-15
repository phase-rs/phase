# Tournament Organizer — Reality-check on the Phase-61 proposal

**As of `upstream/main` @ `9b175e42f` (2026-09-07).**

This document corrects and supersedes the planning framing in the sibling
`CONTEXT.md` / `PLAN.md` / `RESEARCH.md`. Those were written as a *from-scratch
design proposal* against an older `main` (research commit `d65111246`). The
feature they proposed has since **shipped**, and — importantly — several of the
proposal's "not implemented" claims are **false today**. Verified by a
code sweep on 2026-09-07; every file:line below resolves at the commit stamped
above, so the next reader does not re-derive a stale conclusion (one already
misled a whole planning pass — see §2).

> Rule of thumb this doc exists to enforce: **the proposal describes the past.**
> Read `tournament.rs` and `match_config.rs`, not the proposal, for what the
> code does. Treat the proposal as history.

The re-scoped roadmap, the reusable integration hooks for game-hosting, and the
open design questions this sweep surfaced are **not** in this document — they
are a product discussion, posted as a comment on the PR rather than merged as
repo doctrine. This file is the verified reality-check only.

---

## 1. The Phase-61 organizer SHIPPED

The entire four-PR rollout described in `PLAN.md §4` merged
(`2f665a28b` → `dc88a81d7`), plus two follow-on features and a review round:

| Element (proposal §1–§3) | Status | Evidence |
|---|---|---|
| `TournamentManager`, `tournament.rs` | **BUILT** (~5,200 LOC) | `crates/lobby-broker/src/tournament.rs:1805` |
| `MatchArity` (2..=128, H2H, COMMANDER_POD) | BUILT | `crates/lobby-broker/src/tournament.rs:287,306` |
| `ScoringPolicy` + `default_for_arity` (2n−1) | BUILT | `crates/lobby-broker/src/tournament.rs:357,419` |
| `PodOutcome` / `PairingOutcome` (Reported/Bye/Forfeit) | BUILT | `crates/lobby-broker/src/tournament.rs:593,612` |
| `BracketShape` (Swiss + SingleElimination) | BUILT | `crates/lobby-broker/src/tournament.rs:573` |
| Arity-selected tiebreaks (MTR H2H / MSTR pod) | BUILT | `crates/lobby-broker/src/tournament.rs:1046` |
| `default_total_rounds` / `total_rounds()` / `plus_rounds` | BUILT | `crates/lobby-broker/src/tournament.rs:840,1006` |
| Swiss pairing (greedy + carry/swap, non-backtracking) | BUILT | `crates/lobby-broker/src/tournament.rs:2064` |
| Pod partitioning / short-pod fairness / byes | BUILT | `crates/lobby-broker/src/tournament.rs:680,1386` |
| Organizer/player token auth + reconnect + expiry/rotation | BUILT | `crates/lobby-broker/src/tournament.rs:158`; `crates/lobby-broker/src/protocol.rs:1083` |
| `report_gate` / `open_actions` (broker-owned affordances) | BUILT | `crates/lobby-broker/src/tournament.rs:923` |
| Self-report `ReportMatchResult`, `drop_player`/forfeit | BUILT | `crates/lobby-broker/src/tournament.rs:1321` |
| `format` label, `plus_rounds` (lobby protocol v7) | BUILT | PR #8691 (merged) |
| v6 client-consumption (report_gate/open_actions/scoring) | IN REVIEW | PR #8673 (open) |

`LOBBY_PROTOCOL_VERSION` is **7** (`crates/lobby-broker/src/protocol.rs:583` at
this stamp), not `1` as the proposal states — the proposal correctly predicted
this constant would drift. (It has since moved to `8` in the open PR #8723; see
§3.)

---

## 2. Two proposal claims are now FALSE

1. **`CONTEXT.md` finding #1: "Greenfield within `lobby-broker` — no
   `tournament.rs`/`TournamentManager` exists."** False. It is ~5,200 LOC of
   shipped, tested code.
2. **`CONTEXT.md` "Relationship to adjacent work": "the engine models single
   games only — no best-of-N, no tournament round, no draw/tiebreak state
   machine."** Two of these clauses are **false, and consequential** — this line
   nearly derailed a roadmap discussion by implying the match layer had to be
   *built*. Precisely:
   - **"models single games only" / "no best-of-N" — FALSE.** The best-of-N
     match layer exists and is arity-aware (see §3).
   - **"no tournament round" / "no draw/tiebreak state machine" — substantially
     true *of `crates/engine`*, but misleading as stated.** Round/pairing and
     draw/tiebreak machinery does exist — it just lives in `lobby-broker`
     (`TiebreakOrder` at `crates/lobby-broker/src/tournament.rs:1046`) and
     `draft-core`, not in the engine crate the sentence is scoped to. The
     `CONTEXT.md` claim reads as "nothing exists" when the accurate statement is
     "not in the engine crate."

   The claim was inherited from phase 58's research and was already stale when
   the proposal quoted it.

---

## 3. The match / best-of-N layer already exists (and is arity-aware)

Fully implemented, wired engine → session → lobby protocol → frontend:

- **Types** (`crates/engine/src/types/match_config.rs`): `MatchType {Bo1, Bo3}`,
  `MatchPhase {InGame, BetweenGames, Completed}`, `MatchConfig`,
  `MatchScore {p0_wins, p1_wins, draws}`, `MatchForfeitCause/Result`.
- **Flow** (`crates/engine/src/game/match_flow.rs`):
  `handle_game_over_transition` (game over → between-games **sideboard** →
  next game), `handle_submit_sideboard` (CR 100.2a/100.4a/100.5),
  `handle_choose_play_draw` (loser picks play/draw), `apply_trusted_match_forfeit`
  (match concede / host-kick / disconnect). Handles 4-player tables.
- **Completion**: observable as `GameState.match_phase == Completed` +
  `GameState.match_score` (`crates/engine/src/types/game_state.rs`), serialized
  in every state snapshot.
- **Hosting**: `match_config: MatchConfig` rides `CreateGameWithSettings` (and
  `JoinTargetInfo`/`PeerInfo`) — `crates/lobby-broker/src/protocol.rs:913`. You
  can host a Bo3 in the casual lobby today.
- **Frontend**: `match_config`/`matchType` already in `multiplayerStore`,
  `ws-adapter.ts`, `types.ts`.

### Match structure is arity-dependent (already encoded in the outcome model)

`crates/engine/src/types/match_config.rs:30`: **"Bo3 is inherently 2-player."**
`crates/lobby-broker/src/tournament.rs:1369`: **"Pod results are single-game per
MSTR - game_wins must be empty."**

| Event | Match structure | Recorded outcome |
|---|---|---|
| Head-to-head (Standard/Modern) | **Bo3** | `PodOutcome::Decisive.game_wins` = a completed Bo3 tally (2-0 / 2-1) |
| Commander pod (3–4p) | **Bo1**, single game | `game_wins` = **empty** (one winner) |

At this stamp, head-to-head outcome validation (`validate_match_result`,
`crates/lobby-broker/src/tournament.rs:1321`) expects a *completed-Bo3 tally*
(2-0 / 2-1), so a single-game 1-0 H2H result does not fit. That is a real gap
for anyone who wants a **single-game single-elimination** H2H event
(Arena-style constructed, or an MTGO-style single-game bracket): H2H is not
*always* Bo3. Closing it needs a per-event **match-structure setting** plus a
small outcome-model change, and it is useful independent of game-hosting — even
in the self-report model, a single-game event needs to report "X won the one
game," which the Bo3-tally requirement blocks.

> **Since this stamp:** that gap is now addressed in the open PR **#8723**
> (lobby protocol v8), which adds a per-event `match_type` (Bo1/Bo3) on
> `CreateTournament` and keys `validate_match_result` on it. Recorded here as a
> forward reference; the file:line evidence above is pinned to `9b175e42f`,
> where the gap is still present.

---

## 4. The "no manual reporting" flow already exists — for draft pods

The keystone finding of the sweep. A **draft pod is already** "an organizing
structure that runs Swiss pairings, auto-hosts a game per pairing, and
auto-reports the result with zero manual entry." That is the *shape* of the
flagship tournament feature people ask for — already working in production **for
drafts** (a tournament equivalent would be new work; see the scope note below):

1. **Spawn a game per pairing** —
   `crates/server-core/src/draft_session.rs:582` (`spawn_match_games_for_round`):
   loop pairings → `create_game_n_players(...)` (`:655`) →
   `join_game_with_name_and_reservation(...)` (`:664`) → `start_game(...)`
   (`:676`) → store `active_matches[match_id] = game_code` (`:680`).
2. **Notify players** — `ServerMessage::DraftMatchStart { match_id, round,
   game_code, player_token, your_player }`
   (`crates/server-core/src/protocol.rs:985`).
3. **Detect the outcome** — the engine reaches `WaitingFor::GameOver { winner }`;
   `crates/phase-server/src/main.rs:6588` extracts the winner.
4. **Auto-report** — `report_draft_game_over(...)`
   (`crates/phase-server/src/main.rs:5350`): finds the pod owning the
   `game_code`, maps `PlayerId` → seat → `match_id`, calls
   `DraftAction::ReportMatchResult { match_id, winner_seat }`, and broadcasts
   updated standings. Four call sites cover the game-over, concede and
   concede-match paths: `crates/phase-server/src/main.rs:6815`, `:7112`,
   `:9491`, `:9602`.

**Scope of this finding.** What the citations above establish is that the
*draft* path already does host-a-game-per-pairing and auto-report-on-game-over,
in production — the match layer, the N-player game host, and the
auto-report-on-game-over hook all exist and are exercised there. That draft path
is a **reference pattern** for a future tournament equivalent; it is **not** a
claim that tournament game-hosting exists. Nothing here yet establishes
tournament-owned game creation, ownership/credential routing into hosted tables,
tournament match lifecycle, bye/no-show/disconnect behavior, or failure
handling. Building a `report_tournament_game_over` sibling that reuses this
pattern is **future work**, and its concrete integration hooks and the open
design questions it raises (trust model, which crate hosts, bye/no-show
interplay) are tracked in the PR discussion — deliberately outside this
implementation-status document, which per its own boundary (§ preamble) records
only verified, shipped behavior.
