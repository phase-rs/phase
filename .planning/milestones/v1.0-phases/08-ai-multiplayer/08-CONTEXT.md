# Phase 8: AI & Multiplayer - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

A player can play a complete game of Magic against a competent AI opponent at selectable difficulty levels, or connect to another player over a WebSocket server for authoritative multiplayer — with Standard-format card coverage reaching 60%+ through automated gap analysis and priority-based handler implementation. Covers AI-01 through AI-05, MP-01 through MP-04, PLAT-05.

</domain>

<decisions>
## Implementation Decisions

### AI approach
- Hybrid: game tree search (Alchemy's proven pattern) + selectively ported Forge combat/evaluation heuristics
- Game tree search provides the core decision framework (minimax/MCTS with bounded search)
- Forge's domain intelligence ported for: board evaluation (`CreatureEvaluator`), combat math (`ComputerUtilCombat`, `AiAttackController`, `AiBlockController`), and per-card ability hints (`AiPlayDecision`)
- Forge's mana payment AI, game flow management, and OOP orchestration NOT ported — replaced by our engine's existing systems
- All AI code is pure Rust (no platform deps), compiles to both native and WASM from same source

### AI difficulty
- 5 difficulty levels: Very Easy, Easy, Medium, Hard, Very Hard (matching Alchemy's pattern)
- Scaling via: temperature (randomness), search depth/nodes, lookahead toggles
- Low difficulty = heuristic-only + high temperature (random picks)
- High difficulty = tree search + Forge evaluation + low temperature (deterministic best-move)
- Same code runs native (Tauri) and WASM (PWA) — WASM gets tighter search budgets (fewer nodes/depth)

### AI combat
- Port Forge's combat AI directly — `AiAttackController` and `AiBlockController` logic
- Forge's combat AI is its strongest area: evaluates profitable attacks, assigns blockers to minimize damage, factors in combat tricks

### Multiplayer server
- Dedicated Rust WebSocket server — authoritative, handles hidden information server-side
- Clients never see opponent's hand or library order — server filters GameState before sending
- New `WebSocketAdapter` implements existing `EngineAdapter` interface (same React UI, different transport)
- Architecture: `server-core` library crate for session/protocol/filtering, used by both standalone server binary and Tauri host mode
- Crate structure: `crates/server-core` (shared library), `crates/forge-server` (standalone binary), Tauri app imports `server-core` for host mode
- Zero redundancy — WebSocket protocol, hidden info filtering, session management in `server-core` once

### Matchmaking
- Direct connect (game code / IP:port) for v1 — one player hosts, other enters code to join
- Lobby browser as a second mode — server lists open games, players browse and join
- Both modes use the same `server-core` infrastructure

### Reconnection
- Grace period (2-5 minutes) after disconnect — server keeps game alive
- Reconnecting player gets full filtered GameState snapshot
- Opponent sees "Waiting for opponent to reconnect" with countdown timer
- Timeout = forfeit

### Card coverage
- Automated parsing test: load all Standard-legal card .txt files, parse abilities, check if every ApiType/trigger/keyword has a registered handler
- Per-card coverage report: each card → supported/unsupported with specific missing handler listed
- Priority-by-frequency gap filling: implement missing handlers that appear most frequently across Standard cards first (maximize coverage per effort)
- Enhanced coverage dashboard: per-card view showing Standard card status, filterable by set, type, support status

### AI integration with UI
- Simulated thinking delay: 0.5-2s before each AI action, with random variance (Alchemy's `aiBaseDelay` pattern)
- Full animations for AI actions — same card movement, combat VFX, spell effects, game log as human actions
- Main menu with mode selection: "Play vs AI" (pick difficulty, deck) and "Play Online" (host/join, deck, connect)
- Deck builder accessible from both modes
- AI's hand hidden by default (face-down cards like a real opponent)
- "Show AI hand" debug toggle available for learning/testing

### Claude's Discretion
- Exact tree search algorithm (minimax vs MCTS vs alpha-beta)
- AI search budget defaults per platform (native vs WASM node/depth limits)
- Specific Forge heuristic functions to port vs. skip
- WebSocket protocol message format details
- Server config (port, max games, grace period duration)
- Lobby browser UI design
- AI thinking delay exact timing and variance
- Main menu visual design

</decisions>

<specifics>
## Specific Ideas

- Alchemy project (`../alchemy`) has proven AI architecture: `aiConfig.ts` with difficulty presets, temperature scaling, search config, personality weights — port this pattern directly
- Alchemy's `aiController.ts` pattern: schedule AI actions with delay, wait for animations to finish before next action, re-check if AI still needs to act
- Alchemy has working PeerJS + metered.ca TURN WebRTC implementation — reference but not using for v1 (WebSocket server chosen instead for hidden info security)
- Forge's AI reference at `../forge/forge-ai/` and `../forge/forge-gui/src/main/java/forge/gamemodes/net/` for combat AI and multiplayer patterns
- "Highly idiomatic with clean architecture, as little redundancy as possible" — key principle for server crate layering

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `crates/engine/src/game/engine.rs`: `apply(state, action) → ActionResult` — core function AI calls to simulate game states
- `crates/engine/src/types/actions.rs`: `GameAction` enum — all 11 action types the AI must be able to generate
- `crates/engine/src/types/game_state.rs`: `GameState` + `WaitingFor` — AI responds to each WaitingFor variant
- `client/src/adapter/types.ts`: `EngineAdapter` interface — WebSocketAdapter implements this for multiplayer
- `client/src/stores/gameStore.ts`: Zustand game store with dispatch — AI controller hooks into this (Alchemy pattern)
- `client/src/components/controls/CardCoverageDashboard.tsx`: Existing dashboard — enhance with per-card coverage
- `../alchemy/src/engine/aiConfig.ts`: Complete AI config pattern with 5 difficulty levels, personality weights, search config
- `../alchemy/src/game/controllers/aiController.ts`: AI controller pattern with delay scheduling and animation waiting

### Established Patterns
- `apply()` is pure function: `&mut GameState + GameAction → ActionResult` — perfect for game tree search (clone state, apply, evaluate)
- Build-registry-per-call pattern (effects, triggers, replacements, static abilities) — AI builds registries for simulation
- WaitingFor state machine drives all decisions — AI must handle: Priority, ManaPayment, TargetSelection, DeclareAttackers, DeclareBlockers, ReplacementChoice
- EngineAdapter abstraction: React components don't know if they're talking to WASM, Tauri IPC, or WebSocket
- GameEvent stream drives animations and game log — AI actions produce same events as human actions
- Auto-target when exactly one legal target (Phase 4) — AI benefits from this simplification
- Greedy mana payment (Phase 3) — AI doesn't need to solve mana payment optimization

### Integration Points
- New `crates/forge-ai` crate for AI logic (depends on `engine`)
- New `crates/server-core` crate for multiplayer session/protocol (depends on `engine`)
- New `crates/forge-server` binary crate (depends on `server-core`)
- `client/src/adapter/ws-adapter.ts`: New WebSocketAdapter implementing EngineAdapter
- `client/src/game/controllers/aiController.ts`: New AI controller (Alchemy pattern)
- Main menu / game setup page: new React route for mode selection
- Engine needs new `get_legal_actions(state) → Vec<GameAction>` function — core primitive for both AI and server-side validation

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 08-ai-multiplayer*
*Context gathered: 2026-03-08*
