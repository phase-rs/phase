# Phase 15: Game Loop & Controllers - Research

**Researched:** 2026-03-08
**Domain:** React game loop architecture, controller patterns, auto-priority-pass, Zustand lifecycle management
**Confidence:** HIGH

## Summary

Phase 15 restructures the game's runtime architecture. Currently, GamePage (650 lines) owns adapter creation, AI controller lifecycle, WS adapter setup, and game initialization all in one monolithic `useEffect`. The AI controller dispatches through `gameStore.dispatch()`, bypassing the animation pipeline (`useGameDispatch`), so AI actions are invisible to the player. The phase extracts this into a standalone game loop controller (following the existing `aiController` factory pattern), adds MTGA-style phase stops with auto-priority-pass, and provides a React context for dispatch.

The critical technical challenge is the dispatch unification: `useGameDispatch` is a React hook with per-instance mutex state (via `useRef`), making it unsuitable for the AI controller (which is a plain factory, not a React component). The solution is to extract the snapshot-animate-update logic into a standalone function that the game loop controller owns, then provide it through React context for components.

**Primary recommendation:** Create a `gameLoopController` factory (same pattern as `aiController`) that owns the full lifecycle, extracts dispatch logic from the `useGameDispatch` hook into a standalone module-level function, and adds auto-pass logic using client-side heuristics from the already-available `GameState`.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Brief pause (300-800ms) before each AI action -- fast enough to not bore, slow enough to feel like thinking
- AI actions play through the full animation pipeline (snapshot-animate-update) -- same visual experience as player actions
- AI actions dispatch one at a time, not batched -- player can follow along step by step, matching Arena's pacing
- AI controller must route through the unified dispatch path (currently bypasses animations via gameStore.dispatch)
- MTGA-style aggressive auto-pass: skip all priority windows unless player has relevant instant-speed actions or a phase stop is set
- Phase stops: player can toggle stops on specific phases via the phase indicator bar -- lit up = stop here, dim = auto-pass
- Inline phase stop configuration: click phases on the phase indicator strip to toggle, visible and quick to change mid-game
- Default stops for new game: Pre-combat Main, Post-combat Main, Declare Blockers -- everything else auto-passes
- Full-control mode (existing toggle) disables all auto-passing, stops at every priority window
- Phase stop preferences persist across games via preferencesStore
- Game loop always waits for current animations to complete before dispatching the next auto-pass
- Brief visual beat (~200ms) between auto-passed phases so the phase indicator updates visibly
- Game loop controller follows standalone factory pattern (like aiController) -- subscribes to Zustand stores, dispatches via callback
- Thin hook wrapper manages controller lifecycle (create on mount, dispose on unmount)
- Move AI controller creation, WS adapter setup, and game initialization out of GamePage into the game loop provider/controller
- GamePage focuses on layout and overlays only -- roughly halves its current 650-line size

### Claude's Discretion
- Whether AI and WebSocket opponents share a unified OpponentController interface or stay separate (depends on what makes cleanest code given push vs pull difference)
- Whether game loop owns AI controller lifecycle or they remain separate peers
- Whether to use React context or Zustand-only for dispatch (idiomatic choice: Zustand stores for state, thin component for lifecycle)
- Whether to unify useGameDispatch + gameStore.dispatch into single path or keep both
- Game loop controller internal architecture (state machine vs simple subscription)
- Phase stop data structure and storage format

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| LOOP-01 | OpponentController abstraction supports AI (via WASM) and network (via WebSocket) opponents | Architecture Patterns: OpponentController interface design, AI vs WS push/pull analysis |
| LOOP-02 | useGameLoop hook auto-advances game phases, waits for animations, and delegates to controller | Architecture Patterns: Game loop controller factory, dispatch unification, animation awaiting |
| LOOP-03 | GameDispatchProvider context provides dispatch + controller to all components (no prop drilling) | Architecture Patterns: React context provider, dispatch function extraction |
| LOOP-04 | Auto-priority-pass skips trivial priority windows (e.g. upkeep with no triggers, empty stack) | Architecture Patterns: Auto-pass heuristics, phase stop data model |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| zustand | existing | State management + subscriptions for controllers | Already used throughout; `subscribeWithSelector` enables reactive controller pattern |
| react | existing | Context provider for dispatch lifecycle | Standard React context for dependency injection |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| zustand/middleware (persist) | existing | Phase stop preferences persistence | Store phase stops in preferencesStore via localStorage |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| React context for dispatch | Zustand store for dispatch function | Context is simpler for "provide a function" vs "provide state"; but Zustand store already exists for game state. Recommendation: React context wrapping the dispatch function |
| Client-side auto-pass heuristics | WASM `get_legal_actions` binding | Adding a new WASM export is more accurate but adds Rust build complexity; client-side heuristics using GameState fields (stack, phase, hand) are sufficient for MTGA-style behavior |

## Architecture Patterns

### Current State (Problems to Solve)

**Problem 1: Dual dispatch paths.** `useGameDispatch` (hook) runs snapshot-animate-update but is per-component with `useRef` mutex. `gameStore.dispatch` skips animations entirely. AI currently uses `gameStore.dispatch`, making AI actions invisible.

**Problem 2: GamePage monolith.** GamePage owns adapter creation, game init, AI controller lifecycle, WS adapter setup, and all online state -- 650 lines mixing lifecycle with layout.

**Problem 3: No auto-pass.** Every priority window requires explicit player pass. No phase stop configuration.

### Recommended Architecture

```
client/src/
├── game/
│   ├── controllers/
│   │   ├── aiController.ts          # Existing: AI action scheduling
│   │   ├── gameLoopController.ts    # NEW: orchestrates game flow
│   │   └── types.ts                 # NEW: OpponentController interface
│   ├── dispatch.ts                  # NEW: standalone dispatch function (extracted from hook)
│   └── autoPass.ts                  # NEW: auto-pass heuristic logic
├── providers/
│   └── GameProvider.tsx             # NEW: React context + controller lifecycle
├── stores/
│   ├── preferencesStore.ts          # MODIFY: add phaseStops
│   └── uiStore.ts                   # MODIFY: phase stop state for current game
├── components/
│   └── controls/
│       └── PhaseStopBar.tsx         # NEW: clickable phase indicator with stops
└── pages/
    └── GamePage.tsx                 # SIMPLIFY: layout only, uses GameProvider
```

### Pattern 1: Standalone Dispatch Function
**What:** Extract the snapshot-animate-update flow from `useGameDispatch` hook into a module-level function that both controllers and components can call.
**When to use:** Any code path that sends an action to the engine.
**Why:** `useGameDispatch` uses `useRef` for its mutex, tying it to React component lifecycle. A module-level function with module-level mutex state can be called from factories (AI controller, game loop controller) and React components alike.

```typescript
// game/dispatch.ts
// Extracted from useGameDispatch.ts -- same logic, no React dependency
let isAnimating = false;
const pendingQueue: PendingAction[] = [];

export async function dispatchAction(action: GameAction): Promise<void> {
  if (isAnimating) {
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ action, resolve, reject });
    });
  }
  isAnimating = true;
  try {
    await processAction(action);
  } finally {
    await processQueue();
    isAnimating = false;
  }
}

// processAction: snapshot -> submitAction -> normalize -> animate -> update state
// Same logic as current useGameDispatch processAction callback
```

### Pattern 2: Game Loop Controller Factory
**What:** Standalone factory (like `aiController`) that owns game lifecycle, auto-pass, and opponent controller.
**When to use:** Created once per game session by GameProvider.

```typescript
// game/controllers/gameLoopController.ts
export interface GameLoopController {
  start(): void;
  stop(): void;
  dispose(): void;
}

export function createGameLoopController(config: GameLoopConfig): GameLoopController {
  let unsubscribe: (() => void) | null = null;

  function onWaitingForChanged() {
    const { waitingFor } = useGameStore.getState();
    if (!waitingFor) return;
    if (waitingFor.type === "GameOver") return;

    // Delegate to opponent controller if opponent's turn
    // Check auto-pass if player's turn
    if (isPlayerTurn(waitingFor)) {
      maybeAutoPass(waitingFor);
    }
  }

  function start() {
    unsubscribe = useGameStore.subscribe(
      (s) => s.waitingFor,
      onWaitingForChanged,
    );
  }
  // ...
}
```

### Pattern 3: OpponentController Interface
**What:** Common interface for AI and WebSocket opponents. AI polls WASM for actions; WS receives actions via server push.
**When to use:** Abstracts opponent behavior for the game loop.

**Recommendation: Keep AI and WS separate** -- they have fundamentally different flows:
- AI: subscribe to waitingFor -> schedule delay -> call `get_ai_action()` -> dispatch
- WS: server pushes state updates -> adapter already handles this via gameStore

The WebSocket adapter already implements `EngineAdapter` and pushes state updates directly. It doesn't need an "opponent controller" -- the server drives the opponent. An `OpponentController` interface is only useful for AI-like opponents. However, LOOP-01 requires the abstraction, so define a minimal interface that the WS adapter trivially implements (as a no-op start/stop since the server drives it).

```typescript
// game/controllers/types.ts
export interface OpponentController {
  start(): void;
  stop(): void;
  dispose(): void;
}
// AI: createAIController returns OpponentController
// WS: createWsOpponentController wraps the adapter's event flow
```

### Pattern 4: Auto-Pass Heuristics (Client-Side)
**What:** Determine if a priority window should be auto-passed without calling WASM.
**When to use:** Every time `WaitingFor.Priority` arrives for the player.

Key insight: `get_legal_actions` is NOT exposed to WASM. Rather than adding a new WASM binding (requires Rust build), use the game state fields already available on the client:

```typescript
// game/autoPass.ts
export function shouldAutoPass(
  state: GameState,
  waitingFor: WaitingFor,
  phaseStops: Set<Phase>,
  fullControl: boolean,
): boolean {
  if (fullControl) return false;
  if (waitingFor.type !== "Priority") return false;
  if (waitingFor.data.player !== PLAYER_ID) return false;

  // Never auto-pass if stack has items (opponent may have cast something)
  if (state.stack.length > 0) return false;

  // Stop at phases the player has explicitly marked
  if (phaseStops.has(state.phase)) return false;

  // Don't auto-pass if player has castable spells in hand
  // (heuristic: check if hand has non-land cards and player has mana)
  if (hasRelevantActions(state)) return false;

  return true;
}

function hasRelevantActions(state: GameState): boolean {
  const player = state.players[PLAYER_ID];
  const hasMana = player.mana_pool.mana.length > 0;
  const hasInstants = player.hand.some((id) => {
    const obj = state.objects[id];
    // Instant-speed check: has "Instant" type or "Flash" keyword
    return obj && (
      obj.card_types.core_types.includes("Instant") ||
      obj.keywords.includes("Flash")
    );
  });
  return hasMana && hasInstants;
}
```

### Pattern 5: Phase Stop Data Model
**What:** Which phases the player wants to stop at.
**Storage:** `preferencesStore` (persisted to localStorage) for defaults; `uiStore` for current-game overrides.

```typescript
// In preferencesStore:
phaseStops: Phase[] // default: ["PreCombatMain", "PostCombatMain", "DeclareBlockers"]
setPhaseStops: (stops: Phase[]) => void

// Auto-pass reads from preferencesStore for defaults,
// uiStore for any runtime toggles during the current game
```

### Pattern 6: GameProvider (React Context for Lifecycle)
**What:** React component that creates the game loop controller, provides dispatch via context, cleans up on unmount.
**When to use:** Wraps GamePage.

```typescript
// providers/GameProvider.tsx
const GameDispatchContext = createContext<(action: GameAction) => Promise<void>>(
  () => Promise.reject("No GameProvider"),
);

export function GameProvider({ children }: { children: React.ReactNode }) {
  const controllerRef = useRef<GameLoopController | null>(null);

  useEffect(() => {
    // Init adapter, game, controller
    // Start controller
    return () => {
      controllerRef.current?.dispose();
    };
  }, []);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}

export function useDispatch() {
  return useContext(GameDispatchContext);
}
```

### Anti-Patterns to Avoid
- **Hook-level mutex for shared dispatch:** `useRef` in `useGameDispatch` creates per-component mutex instances. If two components both use the hook, they have independent mutexes -- not actually serialized. The module-level approach fixes this.
- **Calling WASM from React render:** All WASM calls must be in effects/callbacks, never during render.
- **Tight coupling between controller and component:** Controllers should subscribe to Zustand stores, not receive React state as props.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| State subscriptions in controllers | Custom event emitter | `zustand.subscribe` with selector | Already used in aiController, battle-tested |
| Persisted preferences | Custom localStorage wrapper | `zustand/middleware/persist` | Already used in preferencesStore |
| Animation timing | Custom timer management | Existing `useAnimationStore` + `setTimeout` | Animation pipeline already built in Phase 14 |
| Async serialization | Custom lock/semaphore | Module-level boolean flag + promise queue | Same pattern as current useGameDispatch, just lifted out of React |

## Common Pitfalls

### Pitfall 1: Dispatch Race Conditions
**What goes wrong:** AI dispatch and player dispatch interleave, corrupting animation state.
**Why it happens:** Two independent mutex instances (hook vs controller) don't coordinate.
**How to avoid:** Single module-level dispatch function with single mutex. All paths (AI, player, auto-pass) go through it.
**Warning signs:** Animations playing simultaneously, state updates out of order.

### Pitfall 2: Auto-Pass Infinite Loop
**What goes wrong:** Auto-pass dispatches PassPriority, which updates waitingFor, which triggers auto-pass check again, creating a tight loop.
**Why it happens:** No debounce or animation-wait between auto-passes.
**How to avoid:** Always await animation completion + the ~200ms visual beat before the next auto-pass check. Use the dispatch queue to serialize.
**Warning signs:** Browser freezes, phase indicator flickers.

### Pitfall 3: Controller Cleanup on Unmount
**What goes wrong:** Zustand subscriptions leak if controller.dispose() isn't called.
**Why it happens:** Missing cleanup in useEffect return, or controller created outside effect.
**How to avoid:** Create controller in useEffect, call dispose in cleanup function.
**Warning signs:** Memory leaks, stale subscriptions firing after navigation.

### Pitfall 4: WS Adapter Double-Handling
**What goes wrong:** Online games break because WS adapter already pushes state updates, but game loop controller also tries to manage state.
**Why it happens:** Game loop controller designed for AI (pull model) applied to WS (push model).
**How to avoid:** WS opponent controller is a no-op wrapper. The WS adapter already updates gameStore state via its own mechanism. The game loop controller only manages auto-pass for the local player, not opponent actions.
**Warning signs:** Duplicate state updates, events processed twice.

### Pitfall 5: Phase Stop Persistence Confusion
**What goes wrong:** Player changes phase stops in-game, but they don't persist OR they unexpectedly change defaults.
**Why it happens:** Mixing current-game state with persisted preferences.
**How to avoid:** PreferencesStore holds default stops. Initialize current-game stops from defaults on game start. In-game toggles modify current-game only. Optionally save current stops back to preferences on game end.
**Warning signs:** Stops resetting mid-game, or test games affecting default stops.

## Code Examples

### Extracting Dispatch from Hook to Module

Current `useGameDispatch` processAction (lines 39-88 of useGameDispatch.ts) contains the core snapshot-animate-update flow. The extraction keeps identical logic but uses module-level variables instead of `useRef`:

```typescript
// game/dispatch.ts
import { useAnimationStore } from "../stores/animationStore";
import { useGameStore } from "../stores/gameStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { normalizeEvents } from "../animation/eventNormalizer";
import { SPEED_MULTIPLIERS } from "../animation/types";
import type { GameAction } from "../adapter/types";

let isAnimating = false;
const pendingQueue: Array<{
  action: GameAction;
  resolve: () => void;
  reject: (err: unknown) => void;
}> = [];

export let currentSnapshot = useAnimationStore.getState().captureSnapshot();

export async function dispatchAction(action: GameAction): Promise<void> {
  if (isAnimating) {
    return new Promise((resolve, reject) => {
      pendingQueue.push({ action, resolve, reject });
    });
  }
  isAnimating = true;
  try {
    await processAction(action);
  } finally {
    await drainQueue();
  }
}

async function processAction(action: GameAction): Promise<void> {
  // Same logic as useGameDispatch lines 39-88
  const { adapter, gameState } = useGameStore.getState();
  if (!adapter || !gameState) throw new Error("Game not initialized");

  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  const events = await adapter.submitAction(action);
  const steps = normalizeEvents(events);

  const speed = usePreferencesStore.getState().animationSpeed;
  const multiplier = SPEED_MULTIPLIERS[speed];
  if (steps.length > 0 && multiplier > 0) {
    useAnimationStore.getState().enqueueSteps(steps);
    const totalDuration = steps.reduce(
      (sum, step) => sum + step.duration * multiplier, 0
    );
    await new Promise<void>((r) => setTimeout(r, totalDuration));
  }

  const newState = await adapter.getState();
  useGameStore.setState(/* same as current */);
}

async function drainQueue(): Promise<void> {
  while (pendingQueue.length > 0) {
    const next = pendingQueue.shift()!;
    try {
      await processAction(next.action);
      next.resolve();
    } catch (err) {
      next.reject(err);
    }
  }
  isAnimating = false;
}
```

### Phase Stop Toggle on Phase Indicator

```typescript
// components/controls/PhaseStopBar.tsx
const ALL_PHASES: Phase[] = [
  "Untap", "Upkeep", "Draw", "PreCombatMain",
  "BeginCombat", "DeclareAttackers", "DeclareBlockers",
  "CombatDamage", "EndCombat", "PostCombatMain", "End", "Cleanup",
];

const PHASE_LABELS: Record<Phase, string> = {
  Untap: "UT", Upkeep: "UP", Draw: "DR", PreCombatMain: "M1",
  BeginCombat: "BC", DeclareAttackers: "DA", DeclareBlockers: "DB",
  CombatDamage: "CD", EndCombat: "EC", PostCombatMain: "M2",
  End: "EN", Cleanup: "CL",
};
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Hook-level dispatch with useRef mutex | Module-level dispatch function | This phase | Enables non-React callers (controllers) |
| AI bypasses animation pipeline | AI routes through unified dispatch | This phase | AI actions become visible |
| Manual pass every priority | MTGA-style auto-pass with phase stops | This phase | Smooth game flow |
| GamePage owns all lifecycle | GameProvider extracts lifecycle | This phase | GamePage ~halved in size |

## Open Questions

1. **Should `get_legal_actions` be exposed to WASM?**
   - What we know: It exists in `forge-ai/src/legal_actions.rs` but no WASM binding. Client-side heuristics can approximate it.
   - What's unclear: Whether the heuristic (check instant/flash in hand + mana available) is accurate enough for good UX.
   - Recommendation: Start with client-side heuristics. They cover 95% of cases. Add WASM binding later if needed. The heuristic can be conservative (stop if unsure) to avoid frustrating players.

2. **WS adapter interaction with game loop controller**
   - What we know: WS adapter pushes state updates. The server drives opponent actions.
   - What's unclear: Whether auto-pass should work in online games (opponent is on the server, not AI).
   - Recommendation: Auto-pass works for the local player in both AI and online modes. The game loop controller only auto-passes when it's the local player's priority.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | vitest (jsdom environment) |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && pnpm test -- --run --reporter=verbose` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| LOOP-01 | OpponentController interface works for AI and WS | unit | `cd client && pnpm test -- --run src/game/controllers/__tests__/opponentController.test.ts` | Wave 0 |
| LOOP-02 | Game loop auto-advances, waits for animations, delegates | unit | `cd client && pnpm test -- --run src/game/controllers/__tests__/gameLoopController.test.ts` | Wave 0 |
| LOOP-03 | GameDispatchProvider provides dispatch via context | unit | `cd client && pnpm test -- --run src/providers/__tests__/GameProvider.test.tsx` | Wave 0 |
| LOOP-04 | Auto-pass skips trivial priority, respects phase stops | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cd client && pnpm test -- --run && cd client && pnpm run type-check`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/game/__tests__/autoPass.test.ts` -- covers LOOP-04 auto-pass heuristics
- [ ] `client/src/game/__tests__/dispatch.test.ts` -- covers standalone dispatch function
- [ ] `client/src/game/controllers/__tests__/gameLoopController.test.ts` -- covers LOOP-02
- [ ] `client/src/game/controllers/__tests__/opponentController.test.ts` -- covers LOOP-01
- [ ] `client/src/providers/__tests__/GameProvider.test.tsx` -- covers LOOP-03

## Sources

### Primary (HIGH confidence)
- Codebase analysis: `client/src/hooks/useGameDispatch.ts` -- current dispatch implementation
- Codebase analysis: `client/src/game/controllers/aiController.ts` -- existing controller pattern
- Codebase analysis: `client/src/stores/gameStore.ts` -- game state management
- Codebase analysis: `client/src/pages/GamePage.tsx` -- current lifecycle management (extraction target)
- Codebase analysis: `client/src/adapter/types.ts` -- WaitingFor, Phase, GameState types
- Codebase analysis: `crates/engine-wasm/src/lib.rs` -- WASM bindings (no `get_legal_actions`)
- Codebase analysis: `crates/forge-ai/src/legal_actions.rs` -- server-side legal action computation
- Codebase analysis: `client/src/stores/preferencesStore.ts` -- persistence pattern

### Secondary (MEDIUM confidence)
- MTGA auto-pass behavior -- referenced in CONTEXT.md from user's domain expertise with Arena

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all libraries already in use, no new dependencies
- Architecture: HIGH -- follows established patterns (aiController factory, Zustand subscriptions), direct codebase analysis
- Pitfalls: HIGH -- identified from reading actual code paths and understanding dispatch race conditions
- Auto-pass heuristics: MEDIUM -- client-side approach is a pragmatic approximation; may need tuning

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable architecture, no external dependency changes)
