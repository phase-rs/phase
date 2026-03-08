# Architecture Patterns: Alchemy UI Integration with Forge.rs Engine

**Domain:** MTG game engine frontend / Arena-style UI port
**Researched:** 2026-03-08

## Executive Summary

Alchemy and Forge.rs share the same foundational patterns (Zustand stores, discriminated union types, event-driven dispatch, animation queues) but differ fundamentally in **where the engine lives** and **how dispatch flows**. Alchemy runs a synchronous TypeScript engine in-process; Forge.rs delegates to an async Rust/WASM engine via the EngineAdapter interface. The port must preserve Forge.rs's EngineAdapter abstraction while adopting Alchemy's superior animation pipeline, controller pattern, and game loop architecture.

---

## 1. Dispatch Comparison: gameStore.dispatch()

### Alchemy's Pattern (synchronous, in-process)

```
Component -> dispatchWithAnimations(action, player) -> gameStore.dispatch(action, player)
                                                         |
                                                      reduce(state, action, player, rng)
                                                         |
                                                      { newState, events }
                                                         |
                                                      set({ state: newState, events, legalActions })
                                                         |
                                                      return events
```

Key characteristics:
- **Synchronous**: `dispatch()` calls `reduce()` directly, returns `GameEvent[]` immediately
- **Player-aware**: dispatch takes `(action, actingPlayer)` -- the acting player is explicit
- **Legal actions cached**: After every dispatch, `enumerateLegalActions(newState, humanPlayer)` runs and caches `legalActions` in the store
- **RNG in store**: The `SeededRNG` lives in the Zustand store, passed to the reducer each call
- **Auto-save**: Debounced persistence after each dispatch (unless game over)

### Forge.rs's Pattern (async, cross-boundary)

```
Component -> useGameDispatch() -> gameStore.dispatch(action)
                                     |
                                  adapter.submitAction(action)   [async, WASM/Tauri/WS]
                                     |
                                  adapter.getState()             [async, separate call]
                                     |
                                  set({ gameState, events, waitingFor })
                                     |
                                  return events
```

Key characteristics:
- **Async**: `dispatch()` is `async`, returns `Promise<GameEvent[]>`
- **No player parameter**: The Rust engine tracks whose turn it is internally; actions are validated server-side
- **Two-call pattern**: `submitAction()` returns events, then `getState()` fetches the full state separately
- **WaitingFor instead of legalActions**: Engine tells the UI *what kind of input it needs* (Priority, ManaPayment, TargetSelection, etc.) rather than enumerating all legal actions
- **Undo via state history**: Stores previous `GameState` snapshots, restores via `adapter.restoreState()`

### Integration Decision

**Keep Forge.rs's async EngineAdapter pattern.** The ported UI must work with `Promise<GameEvent[]>` returns. This means:

1. `dispatchWithAnimations` must become async (or the animation pipeline must handle async dispatch)
2. The `OpponentController` pattern needs to use Forge.rs's adapter interface, not direct store dispatch
3. Legal actions enumeration happens in the Rust engine, exposed via `WaitingFor` -- Alchemy's `legalActions` array pattern does not apply

---

## 2. Type Mappings

### Identifiers

| Concept | Alchemy | Forge.rs | Mapping Strategy |
|---------|---------|----------|------------------|
| Player ID | `'player1' \| 'player2'` (string literal) | `number` (PlayerId = 0, 1) | Adapter translation layer, or change UI to use numeric IDs |
| Object ID | `permanentId: string` (UUID) | `ObjectId = number` (incrementing) | Use Forge.rs numeric IDs throughout; Alchemy's string UUIDs are an engine detail |
| Card ID | `cardId: string` (registry key) | `CardId = number` | Card lookup uses Forge.rs's card database, not Alchemy's `CARD_REGISTRY` |

### Phase / Game State

| Alchemy | Forge.rs | Notes |
|---------|----------|-------|
| `Phase` discriminated union (mulligan/draw/energy/play/battle/targeting/learning/game_over) | `Phase` string enum (Untap/Upkeep/Draw/PreCombatMain/...) + `WaitingFor` union | Forge.rs separates "what MTG phase" from "what input is needed". The UI must key off `WaitingFor` for interaction prompts, and `phase` for visual indicators |
| `state.phase.type === 'battle'` with step sub-union | `phase === 'DeclareAttackers'` + `WaitingFor.DeclareAttackers` | Combat sub-steps are top-level phases in Forge.rs |
| `state.activePlayer` | `state.active_player` | Same concept, different casing (snake_case from Rust serde) |
| `state.players.player1.board: (Permanent \| null)[]` | `state.battlefield: ObjectId[]` + `state.objects` lookup | Forge.rs uses a flat object store with zone-based ID lists. Board slot positions do not exist in the engine |

### GameAction

| Alchemy | Forge.rs | Notes |
|---------|----------|-------|
| `PLAY_CARD { cardIndex }` | `CastSpell { card_id, targets }` or `PlayLand { card_id }` | Forge.rs distinguishes lands from spells. Targets are part of the cast action, not a separate targeting phase (though `TargetSelection` WaitingFor exists for multi-target) |
| `ADVANCE_PHASE` | `PassPriority` | Forge.rs uses MTG priority passing; phase advances happen automatically when both players pass |
| `DECLARE_ATTACKER { permanentId }` | `DeclareAttackers { attacker_ids[] }` | Forge.rs sends all attackers at once, not one at a time |
| `ASSIGN_BLOCKER { blocker, attacker }` | `DeclareBlockers { assignments: [blocker, attacker][] }` | Same: all blockers sent at once |
| `CONCEDE` | No equivalent found | May need to add to Forge.rs engine |
| Learning challenge actions | N/A | Alchemy-specific, will not be ported |

### GameEvent

| Alchemy | Forge.rs | Notes |
|---------|----------|-------|
| `CARD_PLAYED { player, cardId, permanentId? }` | `SpellCast { card_id, controller }` + `ZoneChanged` | Forge.rs emits finer-grained events |
| `CREATURE_ENTERED { permanentId, slot }` | `ZoneChanged { object_id, from, to: 'Battlefield' }` | No slot concept in Forge.rs |
| `DAMAGE_DEALT { targetId, amount, source }` | `DamageDealt { source_id, target: TargetRef, amount }` | Very similar, different field names |
| `CREATURE_DIED { permanentId, cardId }` | `CreatureDestroyed { object_id }` | Forge.rs does not include card_id in the event |
| `PLAYER_DAMAGED { player, amount, source }` | `LifeChanged { player_id, amount }` (negative) | Forge.rs uses signed amount for both damage and healing |
| `CREATURE_HEALED`, `PLAYER_HEALED` | `LifeChanged` / no direct equivalent | Healing is less explicit in Forge.rs events |
| `SPELL_RESOLVED { cardId, targets }` | `StackResolved { object_id }` | Forge.rs does not include targets in the resolved event |
| `ATTACKERS_DECLARED`, `BLOCKERS_DECLARED` | `AttackersDeclared`, `BlockersDeclared` | Similar structure |

### Mapping Strategy

Create an **event normalization layer** that translates Forge.rs `GameEvent[]` into the animation system's expected format. This sits between `adapter.submitAction()` and the animation pipeline.

```
adapter.submitAction(action)
  -> Forge.rs GameEvent[]
  -> normalizeEvents(events, preState, postState)
  -> NormalizedEvent[] (animation-compatible format)
  -> groupEventsIntoSteps() (ported from Alchemy)
  -> AnimationStep[]
  -> animationStore.enqueueSteps()
```

---

## 3. Animation Pipeline Integration

### Alchemy's dispatchWithAnimations

This is the critical integration point. It does:

1. **Pre-dispatch snapshot**: Reads element positions and board state before dispatch (so dying creatures still have positions for death animations)
2. **Dispatch**: Calls `gameStore.dispatch(action, player)` (synchronous)
3. **Controller notification**: Calls `onLocalAction()` for network broadcast
4. **Audio cues**: Immediate SFX for card draws, summons
5. **Card reveal injection**: Adds reveal effects for opponent plays / untargeted spells
6. **Event grouping**: `groupEventsIntoSteps(events, positions, cardIdMap)` converts flat events into timed animation steps
7. **Display health init**: Initializes intermediate health/damage overlays so values animate per-step
8. **Board snapshot preservation**: Preserves pre-dispatch board when deaths occur so dying creatures remain visible during preceding combat animations
9. **Enqueue**: Pushes steps to `animationStore`

### Forge.rs's Current useGameDispatch

Much simpler:
1. Calls async `gameStore.dispatch(action)`
2. If events returned, calls `animationStore.enqueueEffects(events)` (basic 1:1 event-to-effect mapping)

### Integration Plan

**Replace Forge.rs's `useGameDispatch` with an adapted version of Alchemy's `dispatchWithAnimations`.**

Key adaptations needed:

1. **Make it async**: Forge.rs dispatch returns `Promise<GameEvent[]>`, so the wrapper must be async
2. **Event translation**: Insert the normalization layer between Forge.rs events and `groupEventsIntoSteps()`
3. **Position registry**: Port Alchemy's `positionRegistry` module-level Map pattern (already proven more performant than putting positions in Zustand)
4. **Remove Alchemy-specific concerns**: Strip learning challenges, TTS narration, easy-read mode
5. **Preserve pre-dispatch snapshot**: The async gap between "read positions" and "dispatch completes" could be problematic -- positions must be captured before the async call, not after

```typescript
// Adapted dispatchWithAnimations for Forge.rs
export async function dispatchWithAnimations(
  action: GameAction,
  controller?: OpponentController,
): Promise<GameEvent[]> {
  // 1. Capture positions BEFORE async dispatch
  const positions = getPositions();
  const preState = useGameStore.getState().gameState;
  const objectIdMap = buildObjectIdMap(preState); // objectId -> cardId

  // 2. Async dispatch through EngineAdapter
  const events = await useGameStore.getState().dispatch(action);

  // 3. Notify controller (for network broadcast)
  controller?.onLocalAction(action);

  // 4. Normalize Forge.rs events to animation format
  const normalizedEvents = normalizeForgeEvents(events, preState);

  // 5. Group into animation steps (ported from Alchemy)
  const steps = groupEventsIntoSteps(normalizedEvents, positions, objectIdMap);

  // 6. Initialize display overlays + enqueue
  if (steps.length > 0) {
    initializeDisplayOverlays(steps, preState);
    useAnimationStore.getState().enqueueSteps(steps);
  }

  return events;
}
```

### Animation Store

**Port Alchemy's animationStore wholesale.** It is far more sophisticated than Forge.rs's current version:
- Step-based queue (grouped effects with durations) vs flat effect queue
- Board snapshot preservation for death animations
- Display health/damage overlays for per-step progressive updates
- Position registry as module-level Map (not in Zustand) for performance
- Speed multiplier for animation pacing

---

## 4. OpponentController Pattern vs Forge.rs AI Integration

### Alchemy's Pattern

Alchemy uses an `OpponentController` interface with two implementations:

```typescript
interface OpponentController {
  onOpponentPhase(): void;      // Called when it's the opponent's turn
  onLocalAction(action, player): void;  // Called after human dispatches
  dispose(): void;
}
```

- **AIController**: Schedules AI actions with randomized delay, waits for animations to finish, calls `store.dispatch()` directly
- **NetworkController**: Broadcasts local actions to peer, applies remote actions on receipt
- **GameDispatchProvider**: React context that wraps `dispatchWithAnimations` with controller notification

The `useGameLoop` hook ties everything together:
- Subscribes to `gameStore.state` and `animationStore.isAnimating`
- On each tick: detect turn changes, trigger opponent controller, auto-advance trivial phases
- Auto-actions: skip draw/energy/end phases, auto-confirm empty attack declarations, auto-advance when no playable actions

### Forge.rs's Current Pattern

Forge.rs has no game loop hook, no auto-advance, no controller abstraction. AI is called via `get_ai_action()` WASM export. The AI decision happens in the Rust engine.

### Integration Plan

**Port Alchemy's OpponentController + useGameLoop, adapted for Forge.rs.**

The controller interface maps cleanly, but the implementations differ:

**AIController adaptation:**
- Instead of calling a TS `chooseAction()`, call Forge.rs's WASM `get_ai_action()`
- The delay + animation-wait logic ports directly
- `isOpponentPhase()` maps to checking `WaitingFor.data.player !== humanPlayer`

**NetworkController adaptation:**
- Maps directly to Forge.rs's `WsAdapter` -- the WebSocket transport already exists
- Local actions broadcast via WebSocket instead of WebRTC peer
- Remote actions dispatch through `adapter.submitAction()`

**useGameLoop adaptation:**
- `getAutoAction()` must map to Forge.rs's `WaitingFor` instead of Alchemy's `Phase.type`
- Auto-priority-pass when no legal actions (replace Alchemy's `ADVANCE_PHASE` auto-dispatch)
- MTG has more phases to potentially auto-advance (untap, upkeep, draw are automatic in standard play)
- Full control / auto-pass toggles (already in Forge.rs's uiStore) control when to stop

```typescript
// Adapted auto-action logic for MTG
function getAutoAction(
  state: GameState,
  humanPlayer: PlayerId,
  fullControl: boolean,
): AutoAction | null {
  const wf = state.waiting_for;
  if (wf.data.player !== humanPlayer) return null;

  switch (wf.type) {
    case 'Priority':
      // Auto-pass if not in full control and no instant-speed actions available
      if (!fullControl && !hasInstantSpeedActions(state, humanPlayer)) {
        return { action: { type: 'PassPriority' }, delay: 200 };
      }
      return null;
    case 'MulliganDecision':
      return null; // Always wait for player
    default:
      return null;
  }
}
```

---

## Recommended Architecture

### Component Boundaries

| Component | Responsibility | Communicates With |
|-----------|---------------|-------------------|
| **EngineAdapter** (existing) | Async WASM/Tauri/WS bridge to Rust engine | gameStore |
| **gameStore** (adapted) | Holds GameState, dispatches through adapter, tracks waitingFor | EngineAdapter, all UI |
| **animationStore** (ported from Alchemy) | Step-based animation queue, position registry, display overlays | dispatchWithAnimations, animation components |
| **uiStore** (merged) | Card selection, targeting, combat mode, full-control/auto-pass | UI components |
| **dispatchWithAnimations** (ported + adapted) | Pre-snapshot, async dispatch, event normalization, animation grouping | gameStore, animationStore, controller |
| **GameDispatchProvider** (ported) | React context providing dispatch + controller to component tree | dispatchWithAnimations, OpponentController |
| **OpponentController** (ported) | AI scheduling / network broadcast abstraction | gameStore, EngineAdapter |
| **useGameLoop** (ported + adapted) | Auto-advance, turn detection, controller triggering | gameStore, animationStore, OpponentController |
| **Event Normalizer** (new) | Translates Forge.rs GameEvent[] to animation-compatible format | dispatchWithAnimations |

### Data Flow

```
User Click
  -> Component handler
  -> useGameDispatch() (from GameDispatchProvider context)
  -> dispatchWithAnimations(action)
      -> capture positions (sync)
      -> gameStore.dispatch(action) (async, through EngineAdapter)
      -> controller.onLocalAction(action) (network broadcast)
      -> normalizeEvents(forgeEvents) -> groupEventsIntoSteps()
      -> animationStore.enqueueSteps(steps)
  -> animationStore.isAnimating = true
  -> AnimationLayer renders active step effects
  -> animationStore.advanceStep() on timer
  -> ... repeat until queue empty
  -> animationStore.isAnimating = false
  -> useGameLoop tick fires
  -> check WaitingFor: opponent? -> controller.onOpponentPhase()
  -> check WaitingFor: auto-advance? -> dispatchWithAnimations(autoAction)
```

### New Layer: Event Normalization

This is the critical new component that does not exist in either project. It bridges the gap between Forge.rs's MTG-specific events and the animation system's expected format.

```typescript
interface NormalizedEvent {
  type: 'DAMAGE_DEALT' | 'PLAYER_DAMAGED' | 'CREATURE_DIED' | 'CREATURE_ENTERED'
       | 'SPELL_RESOLVED' | 'CARD_PLAYED' | 'ATTACKERS_DECLARED' | 'BLOCKERS_DECLARED'
       | 'CREATURE_HEALED' | 'PLAYER_HEALED' | 'KEYWORD_TRIGGERED';
  // ... fields matching what groupEventsIntoSteps expects
}

function normalizeForgeEvents(
  events: GameEvent[],
  preState: GameState,
): NormalizedEvent[] {
  return events.flatMap(event => {
    switch (event.type) {
      case 'DamageDealt': {
        const target = event.data.target;
        if ('Player' in target) {
          return [{
            type: 'PLAYER_DAMAGED',
            player: target.Player,
            amount: event.data.amount,
            source: String(event.data.source_id),
          }];
        }
        return [{
          type: 'DAMAGE_DEALT',
          targetId: String(target.Object),
          amount: event.data.amount,
          source: String(event.data.source_id),
        }];
      }

      case 'CreatureDestroyed': {
        const obj = preState.objects[String(event.data.object_id)];
        return [{
          type: 'CREATURE_DIED',
          permanentId: String(event.data.object_id),
          cardId: String(obj?.card_id ?? 0),
        }];
      }

      case 'ZoneChanged': {
        if (event.data.to === 'Battlefield') {
          return [{
            type: 'CREATURE_ENTERED',
            permanentId: String(event.data.object_id),
            slot: -1,
          }];
        }
        return [];
      }

      case 'LifeChanged': {
        const amount = event.data.amount;
        if (amount < 0) {
          return [{
            type: 'PLAYER_DAMAGED',
            player: event.data.player_id,
            amount: Math.abs(amount),
            source: 'engine',
          }];
        }
        if (amount > 0) {
          return [{
            type: 'PLAYER_HEALED',
            player: event.data.player_id,
            amount,
          }];
        }
        return [];
      }

      // ... remaining event mappings
      default:
        return [];
    }
  });
}
```

---

## Patterns to Follow

### Pattern 1: Context-Based Dispatch (from Alchemy)

**What:** Provide dispatch function via React context, wrapping raw store dispatch with animation and controller logic.
**When:** Always -- this is the only way components should dispatch actions.

```typescript
// GameDispatchProvider wraps dispatchWithAnimations + controller
<GameDispatchProvider controller={controller}>
  <GameBoard />  {/* all children use useGameDispatch() */}
</GameDispatchProvider>
```

### Pattern 2: Module-Level Position Registry (from Alchemy)

**What:** Keep element positions in a module-level `Map<string, ElementPosition>` instead of Zustand.
**When:** Always for position tracking. Zustand would cause needless re-renders on every ResizeObserver callback.

### Pattern 3: Pre-Dispatch Snapshot (from Alchemy)

**What:** Capture board positions and state before dispatching, so animations can reference elements that may have been removed.
**When:** Every dispatch. Critical for death animations during combat.

### Pattern 4: Step-Based Animation Queue (from Alchemy)

**What:** Group events into `AnimationStep[]` with durations, rather than animating events individually.
**When:** Always. This enables block links preceding combat strikes preceding deaths -- proper sequencing.

---

## Anti-Patterns to Avoid

### Anti-Pattern 1: Synchronous Dispatch Assumption

**What:** Porting Alchemy's `dispatchWithAnimations` without making it async.
**Why bad:** Forge.rs's EngineAdapter returns promises. Synchronous calls would require blocking or ignoring results.
**Instead:** Make `dispatchWithAnimations` async. The animation pipeline already handles async (steps enqueue regardless of dispatch timing).

### Anti-Pattern 2: Duplicating State Between Engine and UI

**What:** Maintaining board state in both the Rust engine and a TypeScript shadow state.
**Why bad:** Desync risk, double memory, stale data bugs.
**Instead:** Single source of truth is the Rust engine via `adapter.getState()`. UI reads from `gameStore.gameState` which is always the latest engine snapshot.

### Anti-Pattern 3: String/Number ID Confusion

**What:** Mixing Alchemy's string IDs with Forge.rs's numeric IDs without consistent conversion.
**Why bad:** Map lookups fail silently (`Map.get("1") !== Map.get(1)`).
**Instead:** Use string-coerced numeric IDs everywhere in the UI (`String(objectId)`), since DOM data attributes and Map keys are strings anyway. Establish this convention early and enforce it.

### Anti-Pattern 4: Porting Alchemy's Learning System

**What:** Bringing over `START_LEARNING_CHALLENGE`, learning stores, challenge policy.
**Why bad:** Alchemy-specific feature unrelated to MTG gameplay. Adds dead code and complexity.
**Instead:** Strip all learning-related code during port. The `dispatchWithAnimations` learning challenge interception (lines 42-88 in Alchemy) should be removed entirely.

### Anti-Pattern 5: Porting Alchemy's Engine Types Directly

**What:** Using Alchemy's `GameState`, `Phase`, `Permanent` types alongside Forge.rs's.
**Why bad:** Two competing type systems for the same concepts. The Rust engine's types (generated via tsify) are the source of truth.
**Instead:** Use Forge.rs's generated types. Create the normalization layer only for animation events, not for game state.

---

## Build Order

This ordering respects dependencies (each step builds on the previous):

### Phase 1: Foundation (no visible UI changes yet)

1. **Event Normalizer** -- Translate Forge.rs `GameEvent[]` to animation-compatible format
2. **Port animationStore** -- Alchemy's step-based queue, position registry, display overlays
3. **Port dispatchWithAnimations** -- Async version, wired to EngineAdapter + new animationStore

### Phase 2: Game Loop and Controllers

4. **Port OpponentController interface** -- Same interface, adapted for Forge.rs's async dispatch
5. **Port AIController** -- Uses `get_ai_action()` WASM export instead of TS AI
6. **Port useGameLoop** -- Auto-advance mapped to `WaitingFor`, turn detection, animation-awareness
7. **Port GameDispatchProvider** -- Context provider wiring dispatch + controller

### Phase 3: UI Components

8. **Port uiStore** -- Merge Alchemy's interaction state with Forge.rs's targeting/combat modes
9. **Port board components** -- GameBoard, PlayerArea, CardSlot, CreatureCard
10. **Port hand components** -- HandDisplay, HandCard (adapt for Forge.rs card data)
11. **Port combat UI** -- Attacker selection, blocker assignment (adapt for batch declaration)
12. **Port animation components** -- AnimationLayer, particle effects, floating numbers

### Phase 4: MTG-Specific Additions

13. **Stack visualization** -- Forge.rs has a real stack; Alchemy does not
14. **Mana payment UI** -- Forge.rs has mana tapping; Alchemy uses simple energy
15. **Priority/instant-speed interaction** -- Full control toggle, auto-pass logic
16. **Targeting UI adaptation** -- Forge.rs has richer targeting (any permanent, any player)

---

## Key Integration Points Summary

| Integration Point | What Changes | New vs Modified |
|-------------------|-------------|-----------------|
| `gameStore.dispatch` | Keep existing async adapter dispatch | **Modified** -- add WaitingFor tracking |
| `animationStore` | Replace entirely with Alchemy's version | **New** (replaces existing) |
| `dispatchWithAnimations` | Port from Alchemy, make async, add event normalizer | **New** |
| `GameDispatchProvider` | Port from Alchemy | **New** |
| `OpponentController` | Port interface + AI/Network implementations | **New** |
| `useGameLoop` | Port from Alchemy, adapt auto-actions for MTG | **New** |
| `Event Normalizer` | Bridge Forge.rs events to animation format | **New** |
| `uiStore` | Merge Alchemy's interaction state into existing store | **Modified** |
| `useGameDispatch` | Replace with context-based dispatch from Provider | **Modified** (simplified) |
| Position registry | Port module-level Map from Alchemy | **New** |

---

## Sources

- Alchemy source: `/Users/matt/dev/alchemy/src/game/` (gameStore, dispatchWithAnimations, controllers, animationStore, uiStore, types)
- Forge.rs source: `/Users/matt/dev/forge.rs/client/src/` (adapter/, stores/, hooks/)
- Both projects' type definitions compared directly via source code analysis

**Confidence: HIGH** -- based on direct source code analysis of both projects, no external sources needed.
