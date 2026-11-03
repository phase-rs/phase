# Threat model: phase

## What this project does and where untrusted input enters

phase.rs is an open-source Magic: The Gathering rules engine and game
client. A Rust workspace (`crates/`) implements the rules engine, a
WebSocket multiplayer server, draft play and an AI opponent; the same
engine compiles to WASM for a React/TypeScript client (`client/`,
browser PWA and Tauri desktop). Games are played live against other
people, so most of what the server parses comes from a peer who may be
hostile. Treat all of the following as attacker-controlled:

- **WebSocket messages.** `crates/phase-server` (axum, `ws`) accepts
  JSON `ClientMessage` values defined in `crates/server-core/src/
  protocol.rs` (a generated, versioned wire protocol, with a hello
  handshake that enforces a minimum supported version). Lobby creation
  and joining, seat mutations, game actions, draft actions, emotes,
  reconnects and spectator joins all arrive this way, before or shortly
  after a player token is issued. Deserialization alone is not the
  control: `crates/server-core` runs a dedicated guard per message
  family (the `*_wire_guard.rs` modules, e.g. `guard_client_hello`,
  `guard_client_message_before_dispatch`, `guard_game_action_payload`,
  the draft/lobby/spectator/reconnect guards) before dispatch.
- **HTTP P2P draft backups.** Separately from WebSocket traffic,
  `POST /p2p-draft-backup` accepts a JSON body, and
  `GET`/`DELETE /p2p-draft-backup/{code}` accept the code in the path
  and `host_peer_id` in the query. These handlers in
  `crates/phase-server/src/admin.rs` validate the draft code, run
  `guard_p2p_backup` / overwrite and secret-redaction checks, and only
  disclose or delete a snapshot when `host_peer_id` matches its owner.
- **Game actions inside a running game.** A seated player submits
  `GameAction` values that the engine reducer applies to shared
  `GameState`. The server must only accept actions from the player
  entitled to act (`acting_player` / `is_acting` in
  `server-core/src/session.rs`) and must broadcast only the per-player
  filtered view (`filter_state_for_player` in `server-core/src/
  filter.rs`): hands, libraries and other hidden zones of opponents
  must never reach a client that is not entitled to see them.
- **Decks.** Players supply deck choices and deck lists that
  `server-core/src/deck_resolve.rs` (with the legacy-deck guard)
  resolves against the card database, including format-legality
  checks. Card names and quantities are untrusted strings and counts.
- **Drafts.** `crates/draft-core` and the draft session code in
  `server-core` handle draft creation (including passwords), picks
  and spectator joins from remote participants.
- **Card data.** The engine loads card data derived from MTGJSON and
  parses Oracle rules text with a `nom` parser (`crates/phase-parser/
  src/parser/`, the card database in `crates/engine/src/database/`).
  This data is upstream-produced and shipped to every client; a
  malformed or malicious card text or record must not panic, hang or
  corrupt the engine. The maintainer-run `crates/feed-scraper` also
  parses remote HTML (MTG metagame feeds) with `reqwest` + `scraper`.
- **P2P / lobby-broker traffic.** `crates/lobby-broker` brokers
  peer-to-peer games (WebRTC via the client); reservation and backup
  snapshot messages cross the same trust boundary at smaller scale.

The React client renders server-supplied state and imports deck files
(`.dck` / `.dec`); it is audited as source. The image builds the Rust
workspace only (see the Dockerfile), so client findings should be
source-demonstrated and are lower priority than server findings.

## Components that matter most / least

Most important, in this order:

1. `crates/server-core`: the wire guards, `protocol.rs`,
   `session.rs`, `filter.rs` (hidden-information filtering),
   `deck_resolve.rs`, the draft/lobby/spectator/reconnect guards and
   their size and capacity caps (e.g. `MAX_SNAPSHOT_*`,
   `MAX_P2P_SNAPSHOT_LEN`, spectator and subscriber limits). A guard
   bypass or a filtering leak crosses a boundary between players.
2. `crates/phase-server`: the WebSocket dispatch itself, the admin
   and persistence paths (`persistence.rs`, bundled SQLite via
   `rusqlite`), and anything reachable before the guards run.
3. `crates/engine` and `crates/phase-parser`: the game reducer's
   authority checks (who may act, and what an action may change), the
   Oracle-text parser and the card-database loader. The parser faces 34,000+ upstream card texts;
   the reducer faces a hostile seated player.
4. `crates/draft-core` and `crates/lobby-broker`: draft state
   transitions, pack/pick handling, lobby reservations.

Out of scope: `client/src-tauri` build glue, `examples/`-style test
harnesses and test-only helpers (e.g. `server-core/src/harness.rs`),
CI/deployment configuration, and third-party dependencies except
where phase's own use makes a dependency flaw reachable from one of
the inputs above — name that path in the report. A flaw wholly inside
a dependency belongs upstream.

## How to exercise it

The scanner runs the audit with external networking disabled; pass
`--bind 127.0.0.1` for any live server (its default bind is `0.0.0.0`).
Every dependency is in the cargo cache, `CARGO_NET_OFFLINE` is set,
and the server binary plus the listed guard/session test binaries are
already built in `/src`.

- **Tests**: `cargo test -p server-core -p lobby-broker -p draft-core
  -p seat-reducer` runs the wire-guard and session suites (already run
  once in the build). Run `cargo test -p phase-server` for the
  WebSocket/HTTP server's own tests; the image does not pre-run that
  large target. Engine behaviour is covered by
  `cargo test -p phase-engine` (large; run targeted modules, e.g.
  `cargo test -p phase-engine -- parser`, rather than the whole suite).
- **A server to attack**: `PHASE_DEV_FIXTURE=1 ./target/debug/phase-server --bind 127.0.0.1`
  listens for WebSocket connections on loopback (see `--help` for the
  port flag). Fixture mode loads the committed
  `data/mtgjson/test_fixture.json` and disables the startup download
  bootstrap, so it runs with no network and no generated production
  `card-data.json`; the Dockerfile smoke-tests this path and `/health`
  must answer. Without `PHASE_DEV_FIXTURE=1`, a clean checkout exits
  during data bootstrap because this binary has no embedded
  `PHASE_CHANNEL` identity and cannot download a compatible database.
  Fixture mode has no `draft-pools.json`, so server-hosted draft start
  is unavailable against the live fixture server; exercise draft logic
  through `draft-core` and `phase-server` tests, which inject pool
  fixtures (e.g. `tst_draft_pools`). The fastest reproducer is usually
  a Rust test in `server-core` that feeds bytes or a `ClientMessage`
  through the guard and dispatch path — no live server needed.
- **Hidden information**: `filter_state_for_player` and
  `guard_game_state_for_broadcast` are the functions to test; a
  reproducer shows a field from one player's hidden zone present in
  another player's filtered view or broadcast.
- **Parser**: the Oracle parser entry points under
  `crates/phase-parser/src/parser/` accept arbitrary card text; feeding
  hostile text (deeply nested, huge, or pathological) and observing a
  panic, hang or unbounded allocation is in scope.
- **Patches**: propose a minimal patch against `main` with a
  regression test in the owning crate. Keep unrelated refactoring,
  and especially game-rules changes, out of a security patch.

## How you rate severity

A finding counts when one principal (an unauthenticated client, a
lobby peer, a spectator, or a player in a game) can affect another
player, the shared server, or a game they are not entitled to affect,
or can make server-side handling of a connection or session panic,
hang, exhaust resources or corrupt state. Severity follows the blast
radius. State the exact message sequence or input and the
configuration it needs; a finding against the defaults weighs more
than one needing a raised limit.

- **Critical**: memory corruption or code execution on the server
  reachable from WebSocket input; acting as another player (seat or
  reconnect-token confusion) with full control of their game.
- **High**: hidden-information disclosure (an opponent's hand,
  library order or other hidden zone delivered to a player or
  spectator not entitled to it); an unauthenticated input that
  crashes or hangs the shared server process, or grows memory past
  the guards' caps; bypassing a wire guard to reach dispatch with a
  message the guard exists to reject, where that changes another
  player's game or the lobby state.
- **Medium**: a panic, hang or excessive allocation confined to one
  game session or one connection; a parser panic on a single hostile
  card text or deck entry; draft or lobby state corruption that
  affects only the attacker's own session.
- **Low**: disclosure of non-secret server internals to an
  unauthenticated peer; issues that need a trusted input (a
  maintainer-supplied card-data file or feed) to trigger.

A wrong game ruling is not by itself a vulnerability: this engine
implements a complex ruleset and rules bugs are expected and fixed as
bugs. It becomes a security finding only when it crosses the boundary
above — for example a player taking an action the server should have
rejected, in a way that affects an opponent.

## Anything to leave alone

- Self-impact only: a client crashing its own game or rendering its
  own state incorrectly, with no effect on peers or the server.
- Resource use that stays within the documented guard caps
  multiplied by the number of concurrent games/connections the
  operator configured; the absence of rate limiting or authentication
  inside test harnesses and examples.
- Findings in test-only code, or in a dependency with no demonstrated
  path from the untrusted inputs listed above.
- Reports that require violating the documented contracts of the
  wire protocol (e.g. a modified client lying about its own state to
  itself); the server re-derives everything that matters, and a case
  where it does not is a finding about the server, not the client.
