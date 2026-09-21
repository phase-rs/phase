# Matches Tied to a Tournament — Design Proposal (Feature ③)

**Status:** design only — no engine, broker, or frontend code in this PR.

**As of `upstream/main` @ `0db982408` (2026-09-16).** Every `file:line`
below resolves at this commit; re-verify before implementing if the head has
moved.

This proposes tying a tournament **pairing** to an actual **hosted game**, so a
match result reports *itself* when the game ends, instead of a human typing it
into a dialog. It is the first tournament feature that touches the game-hosting
layer (roadmap "Track D"). It follows the shipped organizer (Swiss/single-elim
pairings, standings, self-report, byes, forfeits) and the
recoverable-credential-rotation work (lobby protocol v9).

**Proposed direction (§3), pending maintainer decision: Option B —
server-authoritative verified hosting.** The `phase-server` hosts a game per
pairing, observes the engine's `GameOver`, and reports a verified result —
reusing the exact machinery drafts already use. This is the fork owner's
recommended direction and the document is written around it, but it is **not
recorded as settled**: `CONTEXT.md:392-395` reserves the "v1 stays lobby-only (no
auto-launched `GameSession` per pairing)" call for the maintainer, and B reverses
exactly that posture. This PR exists to obtain that decision. §3 records the
rejected alternative (A) and the case for B; §7 lists the sub-decisions folded
into the implementation PR. Until the maintainer signs off (§9), treat B as
proposed, not decided.

---

## 1. What exists today (the manual path we are replacing)

A tournament pairing is scored by a human, and there is **no link** between a
pairing and any game actually played — confirmed absent: `tournament.rs` holds no
`game_code`/`GameState`/`host_peer` reference anywhere
(`crates/lobby-broker/src/tournament.rs`).

The current loop for one pairing:

1. Organizer starts a round → `generate_pairings`
   (`crates/lobby-broker/src/tournament.rs:2329`) emits
   `TournamentPairing { id, round, players, outcome: None }`
   (`tournament.rs:815`). Byes are emitted **already resolved** as
   `PairingOutcome::Bye` (`tournament.rs:1917, 2009`).
2. The players go to the **casual lobby, separately**, and one hosts a game:
   `CreateGameWithSettings` (`crates/lobby-broker/src/protocol.rs:1077`) — which
   already carries `match_config: MatchConfig` (Bo1/Bo3) — the other joins
   (`JoinGameWithPassword`, `protocol.rs:1100`). This game has **no idea** it
   belongs to a tournament.
3. They play. Today that game is **P2P/host-authoritative** — the host peer runs
   the engine in WASM (`getHostAdapter()`, `client/src/adapter/wasm-adapter.ts:170`)
   and the broker is signaling-only, never seeing `GameState`
   (`crates/lobby-broker/src/broker.rs:739`).
4. A human **reads the winner off the screen** and types it into
   `ReportResultDialog` (`client/src/components/tournament/ReportResultDialog.tsx`)
   → `reportMatchResultOver` (`client/src/services/tournamentClient.ts:813`) →
   `ReportMatchResult { code, pairing_id, player_token, outcome }`
   (`protocol.rs:1218`).
5. The broker authorizes: `handle_report_match_result` (`broker.rs:1447`)
   validates the `player_token` **and** that the reporter is seated *in this
   pairing* (`broker.rs:1470`), then `report_result` (`tournament.rs:2434`) checks
   the pairing's `report_gate` (`tournament.rs:1149`) and `validate_match_result`
   (`tournament.rs:1545`: Bo3 ⇒ completed 2-of-3 tally; Bo1/pod ⇒ empty
   `game_wins`) and writes the single `outcome`.

**Today's report is an unverified self-report.** The broker never saw the game;
a seated player asserts the result and is believed (guarded only by "you must be
seated"). Option B removes both the manual labor **and** the trust gap: the
server hosts the game and reports what the engine actually decided.

---

## 2. The reference pattern B copies: drafts already do this, server-authoritatively

A draft pod already is "an organizing structure that runs Swiss pairings,
auto-hosts a game per pairing, and auto-reports the result with zero manual
entry" — in the native `phase-server`, verified. B is a near-exact mirror.

- **State**: `DraftSession.active_matches: HashMap<match_id, game_code>`
  (`crates/server-core/src/draft_session.rs:35-36`), plus a reverse lookup
  `draft_for_game_code(game_code)` scanning sessions
  (`draft_session.rs:783`) and a forward `game_and_seat_for` returning
  `(pairing, game_code, PlayerId)` (`draft_session.rs:80`).
- **Spawn a game per pairing** — `spawn_match_games_for_round`
  (`draft_session.rs:77`): derives `match_config` from the event
  (`session.config.kind.match_config()`, `draft_session.rs:89`), then per pairing
  `create_game_n_players(...)` (`draft_session.rs:724`) →
  `join_game_with_name_and_reservation(...)` (`draft_session.rs:733`) →
  `start_game(...)` (`draft_session.rs:746`) → `active_matches.insert(match_id,
  game_code)` (`draft_session.rs:749`).
- **Notify players** — `ServerMessage::DraftMatchStart { match_id, round,
  game_code, player_token, your_player }` (`crates/server-core/src/protocol.rs:985`;
  sent at `draft_session.rs:753`).
- **Detect + report** — the engine reaches `WaitingFor::GameOver { winner }`
  (`crates/engine/src/types/game_state.rs:12760`); `phase-server` extracts the
  winner (`crates/phase-server/src/main.rs:6588`) and `report_draft_game_over`
  (`main.rs:5350`) reverse-maps `game_code → draft → match_id → seat` and reports.
  Four call sites cover game-over / concede / concede-match
  (`main.rs:6815, 7112, 9491, 9602`).

**Why this makes B cheap:** `server-core` and `phase-server` **already depend on
`lobby-broker`** (`crates/server-core/Cargo.toml:10`,
`crates/phase-server/Cargo.toml:13`) and already route every tournament message
straight into the pure `tournament.rs` core ("Tournament variants are
lobby-scoped, so they delegate straight to…",
`crates/server-core/src/client_message_wire_guard.rs:211`); `phase-server` holds
the `LobbyManager` that owns the tournament. So B needs **no new crate boundary and
no change to the core's *purity*** (it stays GameState-free) — it adds a server-side
hosting layer *beside* the draft one, plus additive state/API on the core itself
(enumerated in §3/§4: `ReportGate::Hosted` + `ReportAuthority`, generation, durable
persistence). "Extended, still pure" — not "unchanged."

---

## 3. The decision: trust model — **B proposed (awaiting maintainer sign-off)**

Because a game is host-authoritative, the auto-report can come from two very
different places, and the choice determines deployment model, protocol surface,
and failure-handling scope for the whole feature. B is the recommended direction;
the maintainer decision it needs is stated in §9.

**Option A — P2P convenience auto-report (rejected).** Keep games P2P; the host
peer auto-submits `ReportMatchResult` on `gameOver` through the existing gate.
Trust is *identical to today* (unverified self-report) — automation, not
anti-cheat. Cheapest (one additive lobby bump, no server needed, runs on the
lobby-only Worker) and preserves the "lobby-only, no auto-launched GameSession"
property (`CONTEXT.md:392, 424`) — but it does **not** close the trust gap.

**Option B — server-authoritative verified hosting (PROPOSED).** `phase-server`
hosts a `GameSession` per pairing (the §2 draft pattern), observes
`WaitingFor::GameOver` server-side, and reports a **verified** result. Genuine
anti-cheat: the server saw the game end. **Server-only result authority** is
part of the proposal, not an afterthought — see §4.1.

**What B costs (corrected against the codebase):**

- **No new crate boundary; the tournament core stays pure/GameState-free — but it
  is *extended*, not unchanged.** `tournament.rs` remains the sole authority for
  pairings/standings and never touches `GameState` (the *purity* invariant,
  `tournament.rs:1-36`, holds); the hosting/orchestration lives in
  `server-core`/`phase-server` like `draft_session.rs`. But the R1–R9 requirements
  add **additive core state/API** to `tournament.rs` itself: a `ReportGate::Hosted`
  arm + a `ReportAuthority` parameter on `report_result` (R1, §4.1), durable
  per-pairing **hosting authority** + a monotonic **generation** with atomic
  validate→publish (R3, §4.3), and durable **persistence/rehydration** of tournament
  state + receipts (R7, §4.4). Enumerated where each is required; none introduces
  `GameState`.
- **The retired invariant is the product-level "v1 stays lobby-only (no
  auto-launched `GameSession` per pairing)"** (`CONTEXT.md:392`, open question #3).
  Hosted tournaments require the native `phase-server` to spawn and observe games;
  the lobby-only Cloudflare Worker broker cannot host. So **verified hosting is a
  `phase-server`-only capability** — a tournament served by the Worker keeps
  manual `ReportMatchResult`; one served by `phase-server` can host + auto-report
  verified. (Both share the same pure core, so this is a deployment capability
  flag, not a fork of the tournament logic.)
- **Genuinely new surface:** the hosting orchestration + `active_matches` map +
  reservation/credential routing into hosted tables + no-show/timeout + reconnect.

**Why B over A** (the fork owner's recommendation): A automates the labor but
leaves results unverified — a self-report either way — and, worse, an unverified
report can be *forged* by a seated player before the game even ends (§4.1). B is
the only option that makes an auto-reported result *trustworthy*, which is the
point of tying a match to a tournament for competitive play. The draft precedent
means B is a well-trodden pattern rather than greenfield, and it costs less than a
from-scratch estimate because the crate deps, message routing, and hosting
machinery already exist. This recommendation is put to the maintainer, not
asserted as decided (§9.5).

---

## 4. Architecture (B)

```text
 lobby-broker (pure, WASM-safe, GameState-free)   EXTENDED (additive), still pure
   tournament.rs: pairings, standings, report_gate,
                  report_result, validate_match_result
        ▲ report_result(pairing_id, PodOutcome)                 (verified)
        │
 server-core / phase-server (native, holds LobbyManager)   NEW hosting layer
   TournamentHosting (sibling to draft_session hosting):
     active_matches: HashMap<PairingId, game_code>
     spawn_match_games_for_round  ── create → join(reservation) → start
        │ TournamentMatchStart { pairing_id, round, game_code, player_token }
        ▼
   phase-server main loop: WaitingFor::GameOver(winner)
     report_tournament_game_over  ── game_code → tournament → pairing → seat
        └─────────────────────────── report_result(verified PodOutcome) ─┘
```

- **Tournament core (`tournament.rs`)** — **pure/GameState-free but extended**
  (not unchanged). Auto-report still enters through the
  `report_result`/`report_gate`/`validate_match_result` path a manual report uses,
  and byes/forfeits still refuse a report — but the core gains additive state/API:
  `ReportGate::Hosted` + a `ReportAuthority` param on `report_result` (R1),
  per-pairing hosting authority + generation with atomic validate→publish (R3), and
  persistence/rehydration of tournament state + receipts (R7). All additive, none
  touching `GameState`.
- **Hosting layer (`server-core`, driven by `phase-server`)** — new, mirrors
  `draft_session.rs`. Per-tournament `active_matches: HashMap<PairingId,
  game_code>`; on round start, for each non-bye pairing, spawn a `GameSession`
  with a `MatchConfig` derived from the pairing's resolved `match_type`
  (Bo3 head-to-head assembles a 2-of-3 tally in the engine; Bo1/pod one game),
  bind reservations to the pairing's tournament `player_token`s, then send
  `TournamentMatchStart`.
- **Outcome detection (`phase-server`)** — reuse the existing
  `WaitingFor::GameOver` extraction (`main.rs:6588`); add
  `report_tournament_game_over` (sibling to `report_draft_game_over`,
  `main.rs:5350`) that reverse-maps `game_code → tournament → PairingId → seat →
  player_key`, builds a `PodOutcome` (Bo3 from the engine `match_score`; Bo1/pod
  single winner, empty `game_wins` — so `validate_match_result` passes by
  construction), and calls `report_result`. Wire the same four game-over / concede
  / concede-match call sites the draft path uses.
- **Disconnect / concede** — match-type-dependent, and **not** uniformly covered by
  one existing primitive (see §6.1). For **2-seat Bo3**, `apply_trusted_match_forfeit`
  (`crates/engine/src/game/match_flow.rs:276-285`) resolves the match to `Completed`
  and the same hook auto-reports it. That primitive **hard-rejects non-Bo3 and
  non-two-seat sessions**, so Bo1 head-to-head (single-elim) and pods need a
  different trusted-terminal path — a real design fork tracked in §6.1 and §9.6.

### 4.1. Result authority: server-only for hosted pairings

**Requirement.** For a hosted pairing, the **server is the sole reporter**. The
client-facing `ReportMatchResult` RPC must be **rejected** for any pairing that is
currently hosted — not merely hidden in the UI. Hiding `ReportResultDialog` is a
display choice, not an authorization control: the RPC still exists, and
`handle_report_match_result` (`broker.rs:1447-1488`) authorizes any seated player
holding a valid `player_token`. Left as-is, a seated player could **forge an
outcome before `WaitingFor::GameOver`** (CWE-863). So:

**Design subtlety (raised in review).** `report_result` (`tournament.rs:2434-2503`)
applies **one** `report_gate` to **every** caller — the exhaustive `match` on
`report_gate(pairing)` runs identically for the client handler
(`broker.rs:1486-1487`) and any server path. So a naive `ReportGate::Hosted → deny`
arm would deny the server's own hosted report too. The gate is deliberately
*WHO-independent* (it answers "is this pairing reportable?", not "may this caller
report?"), and the caller-identity check lives one layer up in the broker
(`authorize_player`). The fix keeps that split and adds an explicit **authority**:

- The pairing carries **durable hosting authority** — persisted state marking it
  hosted (and its current generation, §4.3). `report_gate` returns a new
  `ReportGate::Hosted` for such a pairing: WHO-independent, so the wire
  `PairingView` shows it and the client handler (which cannot present authority)
  is refused.
- `report_result` gains an explicit `ReportAuthority` parameter
  (`SeatedPlayer` | `System`). The `Hosted` arm **admits `System`, denies
  `SeatedPlayer`**; the other arms are unchanged. `System` is not constructible
  from the wire — it is chosen by the server code path
  (`report_tournament_game_over`), never by an RPC. The client handler always
  passes `SeatedPlayer`.
- The **only** writer of a hosted pairing's outcome is thus the server path,
  invoked from the engine's observed terminal state — inaccessible to clients — and
  the core stays `GameState`-free (authority is a plain enum parameter, not a
  session handle).
- **Manual-only mode is retained** for non-hosted pairings (and the hosted
  tournament's per-pairing manual fallback, §5): a non-hosted pairing has no
  hosting authority, so `report_gate` stays `Open` and the seated-player
  self-report path is unchanged.

**Publication contract (raised in review).** "Sole writer = the server path" must
mean a **broker-level system-report action**, not a raw `report_result` core
mutation. `report_result` (`tournament.rs:2434-2502`) only mutates core state; the
`TournamentUpdate` outbound that refreshes every subscriber's pairing/standings is
minted **exclusively** by `Broker::settle_gated` from a `GatedEffect`
(`broker.rs:178, 333-356` — on success it emits
`[ack?, ToSubscribers(TournamentUpdate{code, view})]`; the client report handler
goes through this at `broker.rs:1447-1504`). A hosted handoff that called
`report_result` directly would advance core state but leave subscribers stale and
bypass the action/receipt semantics that make retries observable. So the design
requires:

- A broker-level, **non-wire-constructible** system-report action (the `System`
  authority of R1) that produces the **same** `GatedEffect` → `settle_gated` →
  `TournamentUpdate` outbounds as a client report, differing only in that it is
  admitted by the `Hosted` gate and cannot originate from an RPC frame.
- `report_tournament_game_over` invokes that action and **applies its outbounds**
  (via the existing `main.rs:4766-4823` apply path) as part of the receipt-backed
  publish sequence (§4.4) — publication and receipt-marking in the same
  transaction, so a retry re-emits the same update idempotently.

The design therefore does **not** ship a `player_token` to the client for the
purpose of reporting a hosted result; any token the client holds is for joining
the hosted table, never for asserting its outcome.

### 4.2. Durable, idempotent terminal handoff (one path for all endings)

**Requirement.** A hosted pairing's result must survive process restart and must
be reported **exactly once** across every way a game can end: normal game-over,
concede, disconnect/forfeit, and **restart recovery**. The live GameOver/concede
hooks alone are insufficient because a restart can terminalize a game without ever
reaching them:

- `finish_restored_full_startup` (`phase-server/src/main.rs:249`) terminalizes a
  restored `WaitingFor::GameOver` session via `terminal_artifact(session, winner,
  "Game ended", ranked_result)` (`main.rs:281`) and returns. That
  `FullTerminalArtifact` (`persistence.rs`) carries only `winner`, reason, and
  `ranked_result` — **no pairing identity and no Bo3 `match_score`**. A restored
  hosted game would thus be torn down **without** calling `report_result`, leaving
  the pairing stuck pending after a restart.

So the design requires:

1. **A persisted, typed hosted-match terminal payload** attached to the terminal
   artifact: the `PairingId` (and tournament code), the hosted-game **generation**
   (§4.3), and the complete `PodOutcome` including `match_score` for Bo3 — enough
   to reconstruct the exact report with no live session.
2. **One idempotent report path** — `report_tournament_game_over` — that every
   ending routes through (normal, concede, disconnect, and the restored path in
   `finish_restored_full_startup`), and that **calls `report_result` before the
   hosted game is removed** from `active_matches`. Idempotency mirrors the existing
   `save_ranked_result_idempotent` precedent (`persistence.rs:874`): re-running the
   handoff for an already-reported (pairing, generation) is a no-op, so a
   crash-then-recover or a duplicate terminal cannot double-report or clobber.

Because `report_result` is itself a replay-safe overwrite (`tournament.rs`:
"re-reporting is a correction, not a refusal"), idempotency must be enforced at
the **handoff** layer via the generation fence (§4.3), not assumed from
`report_result`. The persisted payload here is not free-standing: it is the
crash-surviving receipt whose durable home and rehydration ordering §4.4
specifies — without which this recovery handoff cannot run at all on the native
server.

### 4.3. Current-game fencing for rehost/correction

**Requirement.** Rehosting a pairing (after a crash, a lost table, or an organizer
re-launch) must not let a **stale terminal** from the abandoned game overwrite the
rehosted game's result. Because `report_gate` deliberately treats re-reporting as
a permitted correction (`tournament.rs` `report_gate_answers_every_arm...` —
"re-reporting is a correction, not a refusal"), nothing today would stop a late
terminal from an old game from landing after a rehost. So:

- Each hosted pairing carries a monotonic **generation** (epoch) that increments
  on every (re)host. `active_matches` keys on `(PairingId, generation)` → the
  current `game_code`, and the persisted terminal payload (§4.2) records the
  generation it belongs to.
- `report_tournament_game_over` accepts a terminal **only if its generation
  matches the pairing's current generation**; a terminal from a superseded
  generation is dropped (logged, not applied). This "current-game fence" is the
  hosted analogue of the credential-rotation nonce fence — a stale artifact is
  inert, the live game's result wins.
- Rehost therefore = bump generation, spawn a new `GameSession`, replace the
  `active_matches` entry; the old game's `game_code` is cleared and any terminal it
  later emits is fenced out.

**Atomicity (raised in review).** The generation check and the outcome write must
be **one atomic critical section per pairing**, not a check-then-act. Today
`report_result` validates the gate and *then* mutates
`meta.pairings[index].outcome` (`tournament.rs:2461,2500`) as separate steps; a
rehost that bumps the generation **between** a terminal's generation-validation and
its publication would let a stale terminal (validated against the old generation)
still publish and overwrite the rehosted result — a TOCTOU race. So:

- Generation-validate → publish is a single per-pairing transaction (a per-pairing
  lock or compare-and-set on `(pairing, generation)` that couples the check to the
  write), and **rehost** (bump-generation + swap `active_matches`) takes the *same*
  per-pairing critical section, so the two can never interleave. The restore path
  (§4.2) enters the same section.
- This does **not** rely on `report_result`'s replay-safe overwrite: an identical
  replay after a crash is already a no-op, but that only covers *duplicate* reports
  of the *same* generation — it does nothing against the *cross-generation* race,
  which only the atomic section closes.

### 4.4. Durable tournament ownership across restart (raised in review)

**The gap.** §4.2's recovery handoff assumes a live `TournamentManager` exists to
receive `report_result` after a restart. On the native `phase-server` — the **only**
place hosted tournaments run (§3) — it does not:

- Startup builds a **fresh** `Broker` (`main.rs:2166`, `Broker::new()`); the restore
  pass loads **only persisted game sessions** (`main.rs:2170-2195`,
  `load_active_full_sessions` → `finish_restored_full_startup`). Tournament state is
  never rehydrated.
- `tournament.rs` has **no persistence hooks**. Its "durable" design target is the
  Cloudflare **Durable Object** (the Worker) — but the Worker cannot host games, so
  that durability does not apply to hosted tournaments.
- The persisted terminal artifact stores only key / revision / display / recipients
  (`persistence.rs:54-63`) and the terminal table keys only `game_code` +
  `generation` (`persistence.rs:1013-1020`) — no tournament/pairing identity.

So after a restart a recovered hosted game can be terminalized and retired with **no
authoritative pairing to update** — making R2/R3 recovery *impossible*, not merely
unimplemented, and green CI does not exercise this cross-process boundary.

**Requirement (R7).** The implementation must establish **durable tournament state
plus a per-`(pairing, generation)` hosted receipt**, rehydrated **before** the
game-session restore pass runs, with report publication **transactionally coupled**
to the receipt. Two acceptable shapes (implementation sub-decision, both blessed in
review):

1. **Durable registry + receipt, rehydrated first.** Persist the tournament
   (pairings + hosting authority) and a hosted receipt `(tournament_code,
   pairing_id, generation, game_code) → optional PodOutcome`. On startup, rehydrate
   the `TournamentManager` and open receipts **before** the session-restore pass, so
   `finish_restored_full_startup` finds a live authority; mark the receipt applied
   in the **same transaction** as `report_result` (the atomic section of §4.3
   extended across the process boundary).
2. **Transactional outbox / reconciler.** Persist the terminal as an outbox row
   owning the same identity + generation fence + idempotent `PodOutcome`; a
   post-restart reconciler drains unresolved rows into the rehydrated tournament,
   exactly-once. The receipt/outbox owns *what* to report; rehydration owns *where*.

Either way, the durable receipt — not the in-memory `active_matches` map — is the
crash-surviving source of truth for the generation fence (§4.3), and session
recovery must not retire a hosted game before its receipt is reconciled.

### 4.5. Deck submission, format validation, and a readiness/lock lifecycle (raised in review)

A pairing cannot become a server-authoritative table from tokens + `match_type`
alone. `SessionManager::create_game_n_players` requires a `PlayerDeckPayload` and a
`FormatConfig` it validates (`session.rs:1688-1718`), and each join supplies another
deck payload (`:1888-1895`) — but `TournamentPlayer` carries only
identity/token/display/drop, **no deck** (`tournament.rs:890-902`). The draft host
already models the missing piece: it **refuses** to spawn a pairing without submitted
decks and supplies resolved decks + a format to session creation
(`draft_session.rs:680-746`). Hosted tournaments need the same, so **R10** requires:

- **Authenticated deck submission** — each seated player submits a deck bound to
  their tournament `player_token`, before their pairing can be hosted. The
  tournament's `GameFormat` (already metadata, [[tournament-roadmap-track-b]]) drives
  a `FormatConfig`; submission is **validated** against it at submit time, not only at
  session create.
- **A per-pairing readiness gate** — a pairing is hostable only when all seats have a
  valid submitted deck; a **submission timeout** feeds the no-show path (§6, forfeit/
  `drop_player`) rather than hanging.
- **Snapshot + lock/release ownership** — the decks used to spawn are **snapshotted**
  into the durable receipt (§4.4) so a rehost/recovery (R3/R7) reproduces the *same*
  table, and the pairing's deck set is **locked** for the duration of the hosted game
  (no mid-match resubmission), released on terminalization.
- **Durable authority** — this lifecycle (submission, validation, readiness, lock,
  snapshot) is owned by the same durable tournament state that R7 rehydrates; decks
  live with the receipt, not only in the live session.

This is additive to the core (a deck field/table keyed by `(pairing, player)`; still
no `GameState`) plus reuse of the existing `PlayerDeckPayload`/`FormatConfig`
validation. It is a genuine new surface the vertical slice must include — a hosted
match is not reproducible or server-authoritative without it.

---

## 5. Protocol / versioning impact (B)

- New `server-core` `ServerMessage::TournamentMatchStart { pairing_id, round,
  game_code, player_token, your_player }` (additive, sibling to `DraftMatchStart`).
- A lobby-side field marking a tournament as "hosted / verified" so a client knows
  to expect an auto-launched game (additive on `TournamentSummary`/create).
- `LOBBY_PROTOCOL_VERSION` (currently `9`, `protocol.rs:741`) bumps for any new
  lobby field; `scripts/check-protocol-version.mjs` literals updated. **No P2P
  wire-version change** — hosted games use the existing server game path, not new
  first-contact frames. `MIN_SUPPORTED_LOBBY_PROTOCOL` stays low (additive).

**Capability gate (required — additive is not enough on its own).** Additive wire
fields keep old clients from crashing, but they do **not** stop an old client from
being *seated in* a hosted pairing it cannot participate in: it would never receive
/act on `TournamentMatchStart`, so its game would never launch and the pairing
would hang. Hosted mode must therefore be **gated on every participant's client
advertising support**, exactly as per-event match-type was
(`MIN_LOBBY_PROTOCOL_FOR_MATCH_TYPE = 8`, and the sibling
`MIN_LOBBY_PROTOCOL_FOR_RECOVERABLE_ROTATION`/`_DEFAULT_SCORING`/`_TOURNAMENT_ACK`
floors in `protocol.rs` + `ws-adapter.ts`):

- Introduce a `MIN_LOBBY_PROTOCOL_FOR_HOSTED_MATCH` capability floor (registered in
  `scripts/check-protocol-version.mjs`'s bare-integer allowlist, like its
  predecessors).
- **Selecting hosted mode is refused** (typed refusal, mirroring
  `TournamentIncompatible` with a `neededLobbyVersion`) unless the organizer's
  client meets the floor; and a **per-pairing fallback to manual reporting** covers
  the case where a *seated player's* client is below the floor — that pairing runs
  in manual-report mode (§4.1's retained path) instead of hosting, rather than
  hanging. The tournament thus degrades to manual per pairing, never to a stuck
  pairing.

---

## 6. Bye / no-show / disconnect (B)

- **Bye / forfeit** — pre-resolved outcomes (`PairingOutcome::Bye|Forfeit`,
  `tournament.rs:789`); `report_gate` refuses a report on them, so the hosting
  layer never spawns a game for them.
- **No-show / never-connects** — a hosted game that no seat joins within a
  start-timeout resolves to a forfeit (or `drop_player`,
  `tournament.rs:2532`). This timeout policy is **new** and is a §7 sub-decision
  (does a no-show auto-forfeit, or wait for organizer action?).
- **Mid-game disconnect** — two sub-paths: explicit concede/host-kick
  (match-type-dependent, §6.1) and **reconnect-grace-timer expiry** (§6.2), the
  latter currently bypassing the report handoff entirely.
- **Lost report** — server-authoritative, so the server reports directly; there
  is no lost self-report frame to recover (unlike A).

### 6.1. Trusted-terminal for disconnect is not uniform (raised in review)

The only cited trusted-forfeit primitive, `apply_trusted_match_forfeit`
(`match_flow.rs:276-285`), **rejects any session that is not Bo3 and not exactly
two seats** ("Match forfeits require a best-of-three match" / "require exactly two
players"). So it covers **2-seat Bo3 only** — not Bo1 head-to-head (single-elim,
lobby v8) and not pods (Bo1, 3–4 seats). Normal game-over reports fine for every
class **with one exception**: a game can reach `WaitingFor::GameOver { winner: None }`
(a real in-game draw, CR 104.4a, `sba.rs:137-163`) which a non-Bo3 match completes
without a winner (`match_flow.rs:210-225`) — reportable as a `Draw` everywhere
*except single-elimination*, which rejects it (§6.3). So two gaps, not one: the
**trusted mid-game disconnect** for Bo1/pod (this section, R6), and the
**single-elim draw** (§6.3, R9). Pods themselves report a normal winner or a `Draw`
fine — their only gap is disconnect (R6).

This is a genuine fork the design must resolve (§9.6), not hand-wave:

- **Option (i) — restrict hosted v1 to 2-seat Bo3.** Simplest and fully covered by
  the existing primitive, but it **excludes single-elimination (Bo1 H2H) and pod
  events** from hosting — a large cut, since single-elim is a flagship use.
- **Option (ii) — define a generic trusted-terminal primitive** covering Bo1
  (2-seat) and n-seat pods: a transport-trusted terminalization that yields a
  `WaitingFor::GameOver`-shaped outcome (winner = the non-abandoning seat, or the
  pod's remaining/kill result) which the same handoff (R2) reports. This is the one
  piece of genuinely **new engine work** in B and belongs in `match_flow`, bound to
  authenticated transport identity exactly as `apply_trusted_match_forfeit` is.

Recommendation: **(ii)** — restricting to Bo3 would gut single-elim/pod hosting;
but this is the maintainer's call (§9.6). Either way, the §8 matrix's disconnect
row is honestly scoped to the primitive that actually exists.

### 6.2. Reconnect-grace expiry must route through the report handoff (raised in review)

A distinct disconnect sub-path — the **reconnect grace timer expiring** — currently
bypasses everything above, and is the one that actually fires on a player who drops
and never returns. Today `main.rs:2343-2405` calls
`ReconnectManager::check_expired()` (`reconnect.rs:87-100`), which **collapses
per-seat identity to a deduplicated game code**, then builds
`terminal_artifact(session, None, …)` — **`winner: None`** — and removes the
session. It never selects a trusted forfeit winner and never invokes
`report_tournament_game_over`. Left as-is, a hosted pairing whose grace timer
lapses is retired with no winner and **stays pending forever**.

The design requires an explicit **grace-expiry hook** for hosted games:

- Use `check_expired_with_players()` (`reconnect.rs:104-117`), which **retains the
  expired `(game_code, PlayerId)`** per seat, instead of the identity-collapsing
  `check_expired()`.
- Choose **trusted terminal semantics** explicitly: a single seat's expiry ⇒ the
  other seat wins (Bo1) / the match forfeits to the other seat (Bo3, via §6.1's
  primitive). **Simultaneous expiry** is its own problem — see §6.3 (R9); it is not
  a free-form "double-loss per policy," because that is not representable.
- Route that terminal through the **same generation-fenced, receipt-backed
  `report_tournament_game_over`** path (§4.2–§4.4) **before** the session is
  removed — so the pairing is reported, not silently retired.

**Atomicity vs. reconnect (raised in review — R8's real hazard).** The hook is not
safe as a check-then-act: today the expiry worker takes the state lock only in
bursts — `check_expired` under lock, then **releases** it to `prepare_full_terminal`
(async, no lock, `main.rs:2346-2381`), then re-acquires only to `remove_game`
(`main.rs:2383-2395`). In that gap a reconnect wins: `handle_reconnect` sees the
already-removed disconnect record as `NotFound` and **re-seats the player anyway**
(`session.rs:2558-2571` — `ReconnectResult::NotFound ⇒ connected[player] = true`),
accepted on the socket path (`main.rs:7804-7849`). A stale expiry worker then
reports and removes an **actively reconnected** session. So R8 requires an **atomic
session/epoch claim** that expiry takes *before* preparing the terminal and that
**reconnect admission also checks** — once expiry has claimed the epoch, a
concurrent reconnect is rejected (or forces revalidation), and the claim is
re-verified immediately before report+removal. This extends §4.3's per-pairing
critical section to span reconnect admission ↔ terminal report ↔ removal, and the
implementation must carry a **race test** proving no expiry/reconnect interleaving
can report or delete a reconnected session.

This is R8 (§7). It composes with R6 (per-class trusted-terminal the hook selects),
R7 (the receipt it writes through), and R9 (§6.3, the simultaneous-expiry outcome).

### 6.3. Draws are representable everywhere except single-elimination (corrected in review)

A drawn terminal happens two ways: a **real in-game draw** — the engine reaches
`WaitingFor::GameOver { winner: None }` (CR 104.4a simultaneous loss;
`sba.rs:137-163`) and a non-Bo3 match completes without converting it to a winner
(`match_flow.rs:210-225`) — and a **simultaneous full expiry** (every live seat's
grace lapses at once). Both yield "no winner," so both need a representable outcome.

**Correction (raised in review): pods are Swiss and take a `Draw` fine — do not
group them with single-elimination.** `validate_match_result` accepts
`PodOutcome::Draw` for every arity (`tournament.rs:1547-1549`), and a `Draw` is
rejected **only** for `BracketShape::SingleElimination` (`tournament.rs:2492-2499`
— the comment explicitly keeps Swiss and pod draws). Single-elimination is itself
gated to head-to-head (`tournament.rs:742-748` — pod single-elim is "explicitly
excluded"). So:

- **Swiss — head-to-head *and* pods:** a no-winner terminal (in-game draw or
  simultaneous expiry) maps `winner: None` ⇒ **`PodOutcome::Draw`**, representable
  and legal, through the receipt (§4.4) and `TournamentUpdate` outbound (R1). A
  drawn Swiss pod scores as MSTR's all-seated-players-draw. **Nothing special is
  needed here** — pods are fully hostable for the draw case.
- **Single-elimination only (head-to-head):** a `Draw` cannot advance the bracket
  (`:2494-2499`), and there is **no organizer-authored resolution path today** —
  `TournamentAction` is only `StartRound`/`EndTournament`/`Drop`
  (`tournament.rs:705-712`), the sole outcome-writer `report_result` is
  seated-player-only (`broker.rs:1465-1487`). So a drawn single-elim pairing (from a
  real in-game draw **or** a double-expiry) has no legal way to resolve. This is a
  **single-elimination-only** fork (R9):
  - **(a) Build a single-elim draw-adjudication path** — either an organizer-authored
    resolution action (persisted state + authorized action + view/outbound/receipt +
    recovery/reconnect/race tests) or an automatic **replay game** (re-host a
    tiebreaker until a winner emerges). Real new surface either way.
  - **(b) Firm exclusion of the un-adjudicable case** — hosted single-elim requires a
    winner; a drawn single-elim game/expiry is scoped out of v1 (documented as
    unsupported) or single-elim hosting is deferred entirely. Note the **Bo1 vs Bo3**
    axis is orthogonal to this: single-elim admits `MatchType::Bo1`
    (`tournament.rs:902-909`, admitted `:2118-2134`), whose disconnect also lacks a
    trusted primitive (`apply_trusted_match_forfeit` is Bo3/2-seat, `match_flow.rs:278-285`)
    — that gap is R6, separate from this draw gap.

  Recommendation for the recorded **full-coverage** scope: build (a) as a **replay
  game** for single-elim draws (cleanest, no new report authority; it reuses hosting
  to produce a winner), with organizer override as the fallback. §9.7 records the
  choice. Until then, the doc does **not** claim a resolution path that does not
  exist.

Every Swiss terminal (H2H and pod) is serialized through the atomic claim (R8) and
receipt/outbound (R7/R1) and covered by tests — never an implicit `None`. Only
single-elimination's draw case remains open, gated by §9.7.

---

## 7. Requirements vs. sub-decisions

The trust/recovery boundaries below are **requirements of the design** (raised in
review), not deferrable — they are specified above and repeated here as a
checklist the implementation PR must satisfy:

- **R1 — Server-only result authority + broker publication** for hosted pairings:
  `ReportGate::Hosted` (WHO-independent) + an explicit
  `ReportAuthority::{SeatedPlayer,System}` param on `report_result` — `Hosted`
  admits `System` (server code path only, not wire-constructible), denies
  `SeatedPlayer` (client). The `System` write is a **broker-level action** emitting
  the same `GatedEffect` → `settle_gated` → `TournamentUpdate` outbounds as a client
  report (not a raw core mutation), applied as part of the receipt-backed publish
  (§4.1).
- **R2 — Durable, idempotent terminal handoff** through one report path across
  normal / concede / disconnect / restart-recovery, reporting before the hosted
  game is removed; typed persisted payload carries pairing id + full `PodOutcome`
  incl. `match_score` (§4.2).
- **R3 — Current-game generation fence, applied atomically** — generation-validate
  and outcome-publish in one per-pairing critical section (CAS/lock) that rehost
  and recovery also take, closing the cross-generation TOCTOU race (§4.3).
- **R4 — Capability gate + per-pairing manual fallback** (`MIN_LOBBY_PROTOCOL_FOR_HOSTED_MATCH`);
  never seat a below-floor client into a hosting-only pairing (§5).
- **R5 — Reservation binding** — hosted-table reservations bound to the pairing's
  tournament `player_token`s (`join_game_with_name_and_reservation` +
  `LobbyReservation`) so only seated players occupy the table.
- **R6 — Uniform trusted-terminal for disconnect** — the disconnect path must cover
  every hosted match class, not just 2-seat Bo3 (`apply_trusted_match_forfeit`'s
  limit); resolve via §9.6 (generic primitive vs. Bo3-only scope) (§6.1).
- **R7 — Durable tournament ownership across restart** — persist tournament state +
  a per-`(pairing, generation)` receipt, rehydrated **before** session recovery,
  with report publication transactionally coupled to the receipt (or an equivalent
  transactional outbox/reconciler). Without it, R2/R3 recovery is impossible on the
  native server, where the `Broker` is rebuilt fresh (§4.4).
- **R8 — Reconnect-grace-expiry hook, atomic vs. reconnect** — the grace-timer
  expiry path must use `check_expired_with_players()` (not the identity-collapsing
  `check_expired()`), take an **atomic session/epoch claim** that reconnect admission
  also checks (so a reconnect can't re-seat a session expiry is terminalizing across
  the lock-release gap, `main.rs:2346-2395` vs `session.rs:2558-2571`), re-verify the
  claim immediately before report+removal, and route through the receipt-backed
  handoff **before** session removal — with a race test. Replaces today's
  `terminal_artifact(…, None, …)` + remove (§6.2).
- **R9 — Single-elimination draw adjudication** — a no-winner terminal (in-game draw
  `GameOver{winner:None}` *or* simultaneous expiry) maps to `PodOutcome::Draw`, which
  is legal everywhere **except single-elimination** (`tournament.rs:2492-2499`). Swiss
  H2H **and pods** just report the `Draw` — no special handling. Only single-elim
  (H2H-only) can't advance a draw and has no organizer report path today; v1 must
  **(a)** adjudicate it (replay game — recommended — or an organizer-authored action)
  or **(b)** scope the drawn single-elim case out. Corrected: pods are Swiss and are
  **not** part of this gap (§6.3; decision §9.7).
- **R10 — Deck submission, format validation, readiness/lock lifecycle** — a pairing
  cannot spawn a server-authoritative table from tokens + `match_type` alone
  (`create_game_n_players` needs `PlayerDeckPayload` + `FormatConfig`,
  `session.rs:1688-1718`; `TournamentPlayer` has no deck). Require authenticated
  per-`player_token` deck submission validated against the tournament format, a
  readiness gate with submission timeout, and deck snapshot/lock into the durable
  receipt (R7) so rehost/recovery reproduce the same table — mirroring the draft host
  (`draft_session.rs:680-746`) (§4.5).

Genuinely open **sub-decisions** (do not block recording the design, resolved in
the implementation PR):

1. **Hosting trigger** — spawn a round's games at `StartRound` (draft parity,
   `spawn_match_games_for_round`) vs. lazily when both seats are ready.
2. **No-show timeout policy** — auto-forfeit after a start-timeout vs. wait for an
   organizer `drop_player` (§9.2). (Check whether the draft path has a timeout to
   mirror.)
3. **Single-game single-elim H2H** — confirm hosted mode honors the per-event
   `match_type` path (lobby v8, PR #8723) so a Bo1 1-0 result validates.
4. **Durable-ownership shape (R7)** — durable tournament registry + receipt
   rehydrated before session restore, vs. a transactional outbox/reconciler
   (§4.4). Both are acceptable; pick one in the implementation PR.

---

## 8. Failure matrix (how each ending is reported, under R1–R10)

| Ending | Detector | Reports via | Fenced by |
|---|---|---|---|
| Normal game-over | `WaitingFor::GameOver` (`main.rs:6588`) | `report_tournament_game_over` → `report_result` | generation (R3) |
| Concede / concede-match | existing concede sites | same idempotent path (R2) | generation (R3) |
| Disconnect — 2-seat Bo3 | `apply_trusted_match_forfeit` → `Completed` | same idempotent path (R2) | generation (R3) |
| Disconnect — Bo1 / pod | generic trusted-terminal (§6.1, **new**) or scoped out (§9.6) | same idempotent path (R2) | generation (R3) |
| Reconnect-grace expiry (single) | grace-expiry hook, atomic claim vs. reconnect (§6.2, R8) | same idempotent path (R2) | epoch claim (R8) + generation (R3) |
| In-game draw (`winner: None`) | `sba.rs:137-163` / `match_flow.rs:210-225` | Swiss H2H+pod ⇒ `Draw`; single-elim ⇒ adjudicate (§6.3, R9) | generation (R3) |
| Simultaneous full expiry | Swiss H2H+pod ⇒ `Draw`; single-elim ⇒ adjudicate/exclude (§6.3, R9) | receipt + outbound (R1/R7) | epoch claim (R8) |
| Restart recovery | `finish_restored_full_startup` (`main.rs:249`) after tournament rehydration (R7) | durable receipt → same path (R2) | generation, durably (R3+R7) |
| No-show / never-connects | start-timeout (new, §9.2) | forfeit / `drop_player` | n/a (no game) |
| Bye / pre-resolved | `generate_pairings` | never hosted | n/a |
| Client below floor | capability gate (R4) | manual self-report (§4.1) | n/a |

---

## 9. Open questions for the maintainer

1. **Deployment scope.** Is `phase-server`-only verified hosting acceptable for
   v1 (Worker tournaments keep manual reporting), or must hosting work on the
   Worker broker too (which would force A-style P2P hosting as a fallback)?
2. **No-show policy (§7.2).** Auto-forfeit on start-timeout, or always
   organizer-driven?
3. **Rollout.** Is Feature ③ one PR (hosting + auto-report + UI) or split
   (core hosting layer + auto-report, then client), given the draft path is a
   ready template?
4. **Terminal-payload placement.** Is extending `FullTerminalArtifact` with the
   typed hosted-match payload (R2) the right home, or should the hosted-match
   terminal persist alongside it as a separate idempotent record
   (`save_ranked_result_idempotent`-style)?
5. **The trust-model decision itself.** B (server-authoritative) is proposed and
   reverses the lobby-only-v1 posture `CONTEXT.md:392-395` reserves for you —
   please confirm B, or direct A (P2P convenience) instead.
6. **Disconnect scope (§6.1).** For Bo1 H2H (single-elim) and pods,
   `apply_trusted_match_forfeit` does not apply. Add a **generic trusted-terminal**
   primitive in `match_flow` (recommended — keeps single-elim/pod hosting), or
   **restrict hosted v1 to 2-seat Bo3** and defer the rest?
7. **Single-elimination draw adjudication (§6.3, R9).** Corrected: pods are Swiss
   and report a `Draw` fine — only **single-elimination** can't advance a draw (from
   a real in-game draw or a double-expiry). For full-coverage single-elim hosting,
   should a drawn single-elim pairing be resolved by **(a) an automatic replay game**
   (recommended — reuses hosting, no new report authority), **(a′) an organizer-
   authored action**, or **(b) scoped out** of v1? (Independent of §9.6, which is the
   Bo1/pod *disconnect* primitive.)
