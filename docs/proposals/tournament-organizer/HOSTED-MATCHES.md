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

**Decision taken (§3): Option B — server-authoritative verified hosting.** The
`phase-server` hosts a game per pairing, observes the engine's `GameOver`, and
reports a verified result — reusing the exact machinery drafts already use. This
document is written around that choice; §3 records the rejected alternative and
why B was chosen, and §7 lists the sub-decisions that remain for the
implementation PR.

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
the `LobbyManager` that owns the tournament. So B needs **no new crate boundary
and no change to the tournament core's purity** — it adds a server-side hosting
layer *beside* the draft one.

---

## 3. The decision (§3): trust model — **B chosen**

Because a game is host-authoritative, the auto-report can come from two very
different places, and the choice determines deployment model, protocol surface,
and failure-handling scope for the whole feature.

**Option A — P2P convenience auto-report (rejected).** Keep games P2P; the host
peer auto-submits `ReportMatchResult` on `gameOver` through the existing gate.
Trust is *identical to today* (unverified self-report) — automation, not
anti-cheat. Cheapest (one additive lobby bump, no server needed, runs on the
lobby-only Worker) and preserves the "lobby-only, no auto-launched GameSession"
property (`CONTEXT.md:392, 424`) — but it does **not** close the trust gap.

**Option B — server-authoritative verified hosting (CHOSEN).** `phase-server`
hosts a `GameSession` per pairing (the §2 draft pattern), observes
`WaitingFor::GameOver` server-side, and reports a **verified** result. Genuine
anti-cheat: the server saw the game end.

**What B costs (corrected against the codebase):**

- **No new crate boundary; the tournament core stays GameState-free.** The pure
  `tournament.rs` remains the sole authority for pairings/standings and never
  touches `GameState`; the new hosting layer lives in `server-core`/`phase-server`
  exactly like `draft_session.rs`. The *module* invariant (`tournament.rs:1-36`)
  is preserved.
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

**Why B over A** (the user's call): A automates the labor but leaves results
unverified — a self-report either way. B is the only option that makes an
auto-reported result *trustworthy*, which is the point of tying a match to a
tournament for competitive play. The draft precedent means B is a well-trodden
pattern rather than greenfield, and it costs less than a from-scratch estimate
because the crate deps, message routing, and hosting machinery already exist.

---

## 4. Architecture (B)

```text
 lobby-broker (pure, WASM-safe, GameState-free)          UNCHANGED core
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

- **Tournament core (`tournament.rs`)** — unchanged. Auto-report enters through
  the same `report_result`/`report_gate`/`validate_match_result` path a manual
  report uses; byes/forfeits still refuse a report and so never get a hosted game.
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
- **Disconnect / concede** — the engine match layer already resolves
  disconnect/host-kick via `apply_trusted_match_forfeit`
  (`crates/engine/src/game/match_flow.rs`) to match `Completed` with a forfeit
  result; the same GameOver hook auto-reports it. Reused, not rebuilt.

---

## 5. Protocol / versioning impact (B)

- New `server-core` `ServerMessage::TournamentMatchStart { pairing_id, round,
  game_code, player_token, your_player }` (additive, sibling to `DraftMatchStart`).
- Possibly a lobby-side field marking a tournament as "hosted / verified" so the
  client knows to expect an auto-launched game and hide the manual report button
  (additive on `TournamentSummary`/create).
- `LOBBY_PROTOCOL_VERSION` (currently `9`, `protocol.rs:741`) bumps for any new
  lobby field; `scripts/check-protocol-version.mjs` literals updated. **No P2P
  wire-version change** — hosted games use the existing server game path, not new
  first-contact frames. `MIN_SUPPORTED_LOBBY_PROTOCOL` stays low (additive).

---

## 6. Bye / no-show / disconnect (B)

- **Bye / forfeit** — pre-resolved outcomes (`PairingOutcome::Bye|Forfeit`,
  `tournament.rs:789`); `report_gate` refuses a report on them, so the hosting
  layer never spawns a game for them.
- **No-show / never-connects** — a hosted game that no seat joins within a
  start-timeout resolves to a forfeit (or `drop_player`,
  `tournament.rs:2532`). This timeout policy is **new** and is a §7 sub-decision
  (does a no-show auto-forfeit, or wait for organizer action?).
- **Mid-game disconnect** — handled by the engine match layer's
  `apply_trusted_match_forfeit` → match `Completed` → auto-report. No new logic.
- **Lost report** — server-authoritative, so the server reports directly; there
  is no lost self-report frame to recover (unlike A).

---

## 7. Sub-decisions for the implementation PR (do not block the design)

1. **Hosting trigger** — spawn all of a round's games at `StartRound` (draft
   does this, `spawn_match_games_for_round`), or lazily when both seats are ready?
   Draft-parity says at round start.
2. **No-show timeout policy** — auto-forfeit after a start-timeout vs. wait for an
   explicit organizer `drop_player`. (Check whether the draft path already has a
   timeout to mirror.)
3. **Reconnect / re-host** — `report_result` is already a replay-safe overwrite;
   define whether a crashed hosted game can be re-spawned for the same pairing and
   how the stale `game_code` is cleared from `active_matches`.
4. **Reservation binding** — bind hosted-table reservations to the pairing's
   tournament `player_token`s (reuse `join_game_with_name_and_reservation` +
   `LobbyReservation`) so only the seated players occupy the table.
5. **Deployment surfacing** — how the client learns a tournament is "hosted"
   (phase-server) vs. "manual report" (Worker), and how the UI adapts (hide the
   report dialog when hosted; keep it as the disconnect/override path).
6. **Single-game single-elim H2H** — a Bo1 head-to-head (Arena-style bracket)
   still needs the per-event `match_type` path (added in lobby v8, PR #8723) so a
   1-0 result validates; confirm hosted mode honors it.

---

## 8. Open questions for the maintainer

1. **Deployment scope.** Is `phase-server`-only verified hosting acceptable for
   v1 (Worker tournaments keep manual reporting), or must hosting work on the
   Worker broker too (which would force A-style P2P hosting as a fallback)?
2. **No-show policy (§7.2).** Auto-forfeit on start-timeout, or always
   organizer-driven?
3. **Rollout.** Is Feature ③ one PR (hosting + auto-report + UI) or split
   (core hosting layer + auto-report, then client), given the draft path is a
   ready template?
