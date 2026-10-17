# Physical contests (Eye to Eye, Mouth to Mouth)

Two Unhinged cards ask players to do something physical:

| Card | Oracle text |
|------|-------------|
| Eye to Eye | You and target creature's controller have a staring contest. If you win, destroy that creature. |
| Mouth to Mouth | Choose target opponent and target creature they control. You and that player have a breath-holding contest. If you win, you gain control of that creature. |

The Comprehensive Rules define no physical contests, so the contest itself carries no CR number. The surrounding rules it plugs into (CR 608.2c "if you win", CR 109.4, CR 115) are annotated where they are implemented.

An online game cannot ask anyone to stare or hold their breath, so both cards use one **sustained-clicking contest**: both players keep activating a control, and the first player to stop for longer than the inactivity timeout loses.

> The Oracle text says "breath-holding contest" for Mouth to Mouth. The adaptation spec called it a "mouth-to-mouth contest"; the Oracle wording is the one rendered in the UI.

## Where things live

| Layer | Location | Owns |
|-------|----------|------|
| Data model | `crates/engine/src/types/contest.rs` | `ContestState`, lifecycle phases, outcome/result/cancel types, `ContestConfig` |
| Rules | `crates/engine/src/game/contest.rs` | the deterministic state machine and the host driver entry points |
| Effect | `Effect::PhysicalContest { kind, contestant }`, `game/effects/physical_contest.rs` | enters `WaitingFor::PhysicalContest` |
| Parser | `parser/oracle_effect/mod.rs` (`try_parse_physical_contest`) | "you and \<player\> have a \<staring\|breath-holding\> contest" |
| AI | `crates/phase-ai/src/contest.rs` | when an AI seat clicks (shared by server and WASM) |
| Server | `crates/server-core/src/contest.rs`, `phase-server` (`run_contest_driver`) | clock, rate limit, tick task, broadcast |
| Local engines | `drive_contest_js` (engine-wasm), `useLocalContestHost` (client) | the same host duties for single-player / P2P host |
| UI | `client/src/components/contest/ContestOverlay.tsx` | rendering and click input only |

The engine never reads a clock. Every input is stamped by the **host** with a monotonic millisecond counter, so the state machine is deterministic and fully testable without sleeping.

## Lifecycle

`Pending → Preparing → Active → Resolving → Completed`, with `Cancelled` reachable from any non-terminal phase. Illegal transitions are no-ops (`ContestPhase::can_transition_to`), which makes every entry point idempotent: a duplicate start, a repeated tick, a late click or a second settle changes nothing.

* **Pending** – the effect resolved; the host has not supplied a clock yet.
* **Preparing** – countdown to GO. Clicks are not counted.
* **Active** – clicks accepted, inactivity monitored.
* **Resolving** – outcome decided, input ignored. Transient: settled in the same host call.
* **Completed / Cancelled** – terminal. `ContestState::complete()` returns the result exactly once.

## Deciding the result

Outcome is decided by sustained activity, never by click count or rate.

* A player's deadline is `last_activity + inactivity_timeout`.
* **Ordering rule.** Before any input is applied, time is advanced to that input's stamp. A click stamped *at* the deadline counts; a timeout needs `now > deadline`.
* **Settle window.** A lapsed player may still recover by clicking until `deadline + settle_window`, which doubles as the jitter allowance. The contest is decided once at least one player has lapsed and every lapsed player is out of that window.
* **Tie.** If both lapsed and their deadlines are within `settle_window` of each other the result is `ContestOutcome::Tie`: a distinct outcome, not a win, loss or cancellation. It never restarts the contest and triggers neither player's consequences. Otherwise the earlier deadline loses.
* The result depends only on stamps and deadlines, never on how late a tick ran. That includes the hard cap: inactivity is evaluated up to the cap *first*, and only a contest still undecided is cancelled, so a late tick that crosses the cap cannot turn a decidable contest into a cancellation.
* A repeated `start` is a no-op that observes no time, so a host that re-sends its start every tick cannot move the clock past inputs it is about to apply. Inputs are applied in stamp order, each at its own stamp, before the tick's time.

Defaults (`ContestConfig`): countdown 3s, inactivity 2s, reconnect grace 10s, settle window 150ms, hard cap 5 minutes of *unpaused* Active play (→ `Cancelled(TimeLimit)`).

## Identity

Every contest carries an engine-authored `id`: a 53-bit hash (so every host can carry it as a JavaScript number; `0` is never valid) of the state revision at resolution and the interaction session. Every contest resolves in its own action and each action advances the revision, so ids differ within a game. The revision alone is rewindable, though: an approved takeback restores an earlier state, and replaying the same actions reaches the same revision again. The takeback rotates the interaction session, so the replayed contest gets a new id and a frame stamped for the discarded branch's contest is rejected (`WrongContest`) rather than counted. Every host input — a click, a progress frame, the AI's schedule — is bound to it: `contest_click` takes the id and rejects any other (`ClickRejection::WrongContest`) *before* it can observe time or consume the running contest's sequence numbers. Sequence numbers restart per contest, so without the id a frame delayed from one contest would be accepted as fresh activity in the next. `ClientMessage::ContestClick { contest, seq }`, `ServerMessage::ContestProgress { contest, .. }`, the P2P `contest_click { contest, seq }` frame and the WASM click batch all carry it.

## Disconnects

* While any seat is away the contest clock is **paused for everyone** (a connected player cannot lose to a clock the absentee does not share). On return every running clock is shifted by the outage — the inactivity deadlines *and* the hard cap's epoch — so nobody gains or loses budget and nothing resets. `ContestState::active_elapsed_ms` is that same unpaused timebase, and the AI's cadence and stamina run on it (and it does not click while paused).
* Every host must report seats' connectivity to the engine, including seats that were already away when the contest started: the server seeds it from its live connection table on each tick; the local WASM driver is handed the away seats on every `drive_contest_js` call; a native host's server does it itself. The engine keeps evaluating grace expiry on every tick, even while the surrounding game is paused for the disconnect.
* An absentee past `reconnect_grace` **forfeits the contest only** (`ContestLossReason::Forfeit`), not the game.
* If both seats are away when a grace expires, or either is gone for good, the contest is `Cancelled(BothDisconnected)`.
* A participant who concedes or is eliminated → `Cancelled(ParticipantLeftGame)`.
* A restored server session cannot continue a contest (its clock epoch is gone) → `Cancelled(HostAborted)`.
* Disconnecting after your own deadline has passed does not dodge the loss: lapses resolve before the connection change is applied.
* Reporting a seat's connectivity is **clock-neutral unless it is an edge**: hosts repeat the report every tick, and a repeat of the state a seat is already in never advances the contest clock. Input is processed in the order it happened — buffered clicks first, each at its own host stamp, then the seats' connectivity (an edge only observable at the tick), then the clock — so a click stamped before a delayed tick is judged against the deadlines in force when it was made. `engine-wasm`'s `run_contest_drive` is that ordering and is what `drive_contest_js` runs (it is unit-tested natively).
* A native P2P host's engine authority is its server, which reads a seat's presence from that seat's own loopback socket. When a guest's PeerJS channel drops mid-game the host **suspends** the seat (`NativeP2PBridge.suspendGuest`: closes the loopback, keeps the seat's token and last view) so the server pauses the contest clock and starts the ten-second grace; on return the seat is reattached on its own token (`resumeGuest`) and the view the server sends it is the reconnect handoff. A host action for an away seat (conceding it) reattaches first and, if that concession does not commit, suspends the seat again. Unlike `detachGuest`, suspension never removes the seat.

## Anti-cheat and validation

* The server resolves the sender from the authenticated token and the instant from its own clock. `ClientMessage::ContestClick` carries only a per-contest sequence number; clients can claim neither a timestamp, a click count nor a winner. P2P guests do the same (`contest_click`, host-stamped).
* Duplicate or reordered deliveries are rejected by the sequence number; clicks outside `Active`, from non-participants, or after a player's recovery window are rejected by the engine.
* Per-seat token bucket (`ContestPolicy`, default 30 clicks/s sustained, burst 40). A dropped click is harmless — it only ever signals activity, and a player at the limit is active. Fast human clicking tops out near 15/s, so legitimate input is never throttled.
* A P2P host applies the same policy to guests' `contest_click` frames before buffering them (`ContestClickBuffer`): only while a contest is running, only from its two seats, only for its identity, only with a `u32` sequence, within the same rate and a 64-click per-seat queue bound. A malformed frame is dropped on its own; the WASM driver likewise decodes the click batch entry by entry, so a bad entry can never abort the batch (which also advances the clock).
* A takeback cannot be requested while a contest is running — ordinary actions are frozen during a takeback vote, and the real-time contest must not mutate under it. Undoing a *completed* contest is allowed; the replayed contest has a new identity (see Identity).
* A host retains clicks it took for an engine call that then failed: the local overlay returns its clicks to its queue, and the P2P host returns the guests' clicks to `ContestClickBuffer` (only those of the contest still running, within the same per-seat bound). If the engine had in fact accepted a click, the retry is rejected there as a stale sequence, so it is never counted twice.
* Macros and automation are bounded, not detected: a bot can sustain the maximum rate, but only for the 5-minute cap, after which the contest is cancelled with no effect.
* Held keys do not auto-click (`event.repeat` is ignored), so holding Space is not a cheap macro.

## Game integration

* `WaitingFor::PhysicalContest` has no acting player (`NoActor::HostContestDriver`) — the host advances it.
* On settle the controller's win is stamped on the parked continuation as `optional_effect_performed`, which the existing `AbilityCondition::EventOutcomeWon` ("If you win") reads. A loss, a tie and a cancellation all leave it false, so Eye to Eye does not destroy and Mouth to Mouth does not steal.
* The host settles outside `apply()`, so settlement itself runs what `apply()` would after a resolution: the active player (CR 117.3b — not necessarily the caster) is handed priority, the parked continuation resumes, and the normal post-action pipeline then runs state-based actions and puts triggers on the stack (CR 117.5, CR 704.3, CR 603.3) — a destroyed token ceases to exist, a death trigger is stacked, the legend rule can prompt after control changes. A continuation that pauses on a decision of its own is left parked for the action that answers it.
* Results are applied exactly once; after settlement every further tick/click/cancel is `NotInContest`.
* A P2P browser host publishes a changed or settled contest to its own screen (`publishHostSnapshot`, then the guests) **before** any follow-up AI play, and tells the caller it did (`ContestDriveResult.published`) so the overlay does not commit the same transition a second time. The AI's own snapshots unmount the overlay; a transition published after them would never reach the host's store, resolution events included.
* A game-ending contest on the server keeps its driver until the terminal commit lands: a failure with the Full key still active is retried (the same artifact each time, so a commit that already moved the ratings is not rebuilt), one that leaves the key inactive retires the room in memory, and neither releases the driver early.
* Headless hosts (self-play, benchmarks, gates) have no clock or input, so `auto_play::run_ai_actions_headless` / `_bounded_headless` settle an all-AI contest in-batch on a deterministic virtual clock (`contest::settle_headless_contest`) and play on. The live batches (`run_ai_actions`, used by the server and WASM engine) deliberately do not: those hosts drive the contest in real time. A contest with a human contestant is never settled headlessly.

## Transports

| Mode | Host |
|------|------|
| phase-server (online, native-ai) | server tick task every 50ms; clients send `ContestClick`; live counters via `ContestProgress` |
| Single-player WASM, human hotseat | client drives `drive_contest_js` every 50ms with `performance.now()`; the AI seats are the established `aiSeatIds` (none in hotseat, where each human gets their own control) |
| P2P (browser engine) | host drives like single-player (and keeps ticking while a guest is away); guests send `contest_click`, gated and stamped on arrival |
| P2P (native engine) | the native server drives; the host's own and every guest's click is relayed over that seat's own authenticated loopback socket as a `ContestClick` |

AI seats click on a human-ish schedule (`phase_ai::contest::ContestClicker`): a 220–380ms cadence for a random 2.5–9s, so the AI can win or lose.

## Assumptions and known limits

* A contest is only ever between the controller and one other player (both printed cards). "You and each opponent" is deliberately declined by the parser.
* A recorded replay cannot reproduce a real-time result; starting a contest invalidates an in-progress WASM replay recording, as debug actions do.
* P2P guests see opponent click counters only at state transitions (no live `ContestProgress` over the P2P wire).
* The WASM batch decoder (`decode_contest_clicks`) takes `JsValue` and cannot run in a native unit test; its guarantees are covered at the P2P boundary (`contestClickBuffer`, host adapter tests) and by the engine's own sequence/identity rejection.
* Server restarts cancel a running contest.
