# Phase 15: Game Loop & Controllers - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

The game plays smoothly end-to-end — AI opponent acts automatically through the animation pipeline, trivial priority windows auto-pass via MTGA-style phase stops, and dispatch/controller lifecycle is managed centrally rather than scattered through GamePage. Stack visualization, mana payment UI, and combat assignment UI are separate phases (Phase 17).

</domain>

<decisions>
## Implementation Decisions

### AI opponent behavior
- Brief pause (300-800ms) before each AI action — fast enough to not bore, slow enough to feel like thinking
- AI actions play through the full animation pipeline (snapshot→animate→update) — same visual experience as player actions
- AI actions dispatch one at a time, not batched — player can follow along step by step, matching Arena's pacing
- AI controller must route through the unified dispatch path (currently bypasses animations via gameStore.dispatch)

### Auto-priority-pass rules
- MTGA-style aggressive auto-pass: skip all priority windows unless player has relevant instant-speed actions or a phase stop is set
- Phase stops: player can toggle stops on specific phases via the phase indicator bar — lit up = stop here, dim = auto-pass
- Inline phase stop configuration: click phases on the phase indicator strip to toggle, visible and quick to change mid-game
- Default stops for new game: Pre-combat Main, Post-combat Main, Declare Blockers — everything else auto-passes
- Full-control mode (existing toggle) disables all auto-passing, stops at every priority window
- Phase stop preferences persist across games via preferencesStore

### Game loop pacing
- Game loop always waits for current animations to complete before dispatching the next auto-pass
- Brief visual beat (~200ms) between auto-passed phases so the phase indicator updates visibly
- Game loop controller follows standalone factory pattern (like aiController) — subscribes to Zustand stores, dispatches via callback
- Thin hook wrapper manages controller lifecycle (create on mount, dispose on unmount)

### GamePage extraction
- Move AI controller creation, WS adapter setup, and game initialization out of GamePage into the game loop provider/controller
- GamePage focuses on layout and overlays only — roughly halves its current 650-line size
- Game loop controller owns the full lifecycle: init game, create opponent controller, manage auto-pass, teardown

### Claude's Discretion
- Whether AI and WebSocket opponents share a unified OpponentController interface or stay separate (depends on what makes cleanest code given push vs pull difference)
- Whether game loop owns AI controller lifecycle or they remain separate peers
- Whether to use React context or Zustand-only for dispatch (idiomatic choice: Zustand stores for state, thin component for lifecycle)
- Whether to unify useGameDispatch + gameStore.dispatch into single path or keep both
- Game loop controller internal architecture (state machine vs simple subscription)
- Phase stop data structure and storage format

</decisions>

<specifics>
## Specific Ideas

- "MTGA does aggressive skip unless you have full control on. They also allow you to toggle waiting on certain phases" — MTGA is the primary reference for auto-pass behavior
- Phase stops should be toggleable directly on the phase indicator strip, not buried in settings
- AI should feel like it's "thinking" with a brief pause, not instant or laggy
- One action at a time for AI (like Arena shows opponent actions), not batched

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `aiController.ts`: Standalone factory with start/stop/dispose, subscribes to waitingFor — pattern for game loop controller
- `useGameDispatch.ts`: Snapshot→animate→update flow with mutex queue — the animation pipeline AI needs to route through
- `uiStore.ts`: Has `autoPass` boolean + `toggleAutoPass()` — extend with phase stops
- `preferencesStore.ts`: Zustand persist store — add phase stop defaults
- Phase indicator in GamePage center divider — extend with clickable phase stop toggles

### Established Patterns
- Standalone factory controllers (aiController) for side-effect-driven logic subscribing to Zustand stores
- Zustand stores with `subscribeWithSelector` for reactive state management
- Module-level refs for cross-component state (currentSnapshot pattern from useGameDispatch)
- WaitingFor discriminated union drives all game flow decisions

### Integration Points
- `WaitingFor.Priority { player }`: Triggers auto-pass check — is there a phase stop or relevant action?
- `gameStore.dispatch()`: Current AI dispatch path — needs to route through animation pipeline
- `useGameDispatch()`: Current player dispatch path — per-component hook with its own mutex
- `GamePage useEffect`: Currently creates adapter, inits game, creates AI controller, manages WS — extraction target
- `EngineAdapter.submitAction()`: Underlying WASM/WS call — unchanged

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 15-game-loop-controllers*
*Context gathered: 2026-03-08*
