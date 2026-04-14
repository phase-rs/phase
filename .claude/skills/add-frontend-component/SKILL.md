---
name: add-frontend-component
description: Use when adding or modifying frontend UI components — interactive overlays for WaitingFor states, game board elements, card choice modals, animation effects, or any React component that interfaces with the engine via GameAction dispatch.
---

# Adding a Frontend Component

> **Hard rules — all frontend work must respect these (see CLAUDE.md § Design Principles):**
> 1. **The frontend is a display layer, not a logic layer.** It renders engine-provided state and dispatches user actions — nothing more. It must never compute, derive, filter, or re-interpret game data. If a component needs a value the engine doesn't expose, the fix is to add it to the engine's output — not to calculate it client-side. Any "smart" frontend code is a bug.
> 2. **CR-correctness is non-negotiable.** The frontend must never contradict the Comprehensive Rules. If it displays information (legal targets, valid choices, game state), that information must come directly from the engine, which is the CR-validated source of truth. Never approximate engine logic in TypeScript.
> 3. **Build reusable component patterns.** New overlays and modals should follow existing patterns (CardChoiceModal, ModeChoiceModal). Extract shared behavior into composable components rather than duplicating across one-off implementations.

The React/TypeScript frontend communicates with the Rust engine through a transport-agnostic adapter layer. Game state flows from engine → adapter → Zustand stores → React components. Player actions flow in reverse via `dispatch()`. This skill covers wiring new UI components into this pipeline.

**Before you start:** Trace how `ScryChoice` works end-to-end. The current path is: `WaitingFor::ScryChoice` in Rust → TypeScript type in `adapter/types.ts` → `GamePage.tsx` renders `CardChoiceModal` → `CardChoiceModal` routes to `ScryModal` → `dispatch({ type: "SelectCards", data: { cards } })`.

---

## Architecture Overview

```
Engine (Rust/WASM)
    ↓ ActionResult { events, waiting_for }
Adapter (WasmAdapter / WebSocketAdapter / TauriAdapter)
    ↓ GameEvent[], GameState, WaitingFor
Stores (Zustand)
    ├─ gameStore: gameState, waitingFor, legalActions, events, dispatch
    ├─ uiStore: selectedObjectId, targetingMode, combatMode
    └─ animationStore: activeStep, queue, positionRegistry
        ↓
React Components
    ├─ GamePage.tsx — routes WaitingFor → overlays/modals
    ├─ components/modal/ — interactive overlays (CardChoiceModal, ModeChoiceModal, ReplacementModal, NamedChoiceModal)
    ├─ components/board/ — battlefield, permanents, player areas
    ├─ components/hand/ — player/opponent hand
    ├─ components/combat/ — attacker/blocker controls
    ├─ components/animation/ — VFX overlay
    └─ components/log/ — game event log
```

---

## Key Files

### Type Definitions — `client/src/adapter/types.ts`

**Manually maintained** TypeScript discriminated unions mirroring Rust serde output (`tag="type", content="data"`):

```typescript
// WaitingFor — ~19 variants, determines which overlay to show
type WaitingFor =
  | { type: "Priority"; data: { player: PlayerId } }
  | { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "DigChoice"; data: { player: PlayerId; cards: ObjectId[]; keep_count: number } }
  // ...

// GameAction — ~18 variants, player responses
type GameAction =
  | { type: "SelectCards"; data: { cards: ObjectId[] } }
  | { type: "ChooseReplacement"; data: { index: number } }
  // ...

// GameEvent — ~33 variants, for log + animations
type GameEvent =
  | { type: "DamageDealt"; data: { source_id: ObjectId; target: TargetRef; amount: number } }
  // ...
```

### Game Store — `client/src/stores/gameStore.ts`

```typescript
interface GameStoreState {
  gameState: GameState | null;
  waitingFor: WaitingFor | null;
  legalActions: GameAction[];
  events: GameEvent[];         // Latest batch
  eventHistory: GameEvent[];   // Rolling window (last 1000)
  adapter: EngineAdapter | null;
}
```

Key action: `dispatch(action: GameAction)` → adapter.submitAction → animations → state update.

### UI Store — `client/src/stores/uiStore.ts`

Ephemeral UI state — targeting mode, combat selections, hovered/selected objects. Combat selections stay in `uiStore` until the player confirms (optimistic UI pattern).

### Dispatch Pipeline — `client/src/game/dispatch.ts`

```
User action
  → Capture DOM snapshot (pre-animation positions)
  → adapter.submitAction(action)
  → normalizeEvents(events) → AnimationSteps
  → enqueueSteps (animation store)
  → Wait for animation duration
  → Update gameStore (state, waitingFor, legalActions)
  → Save to localStorage
```

### WaitingFor → UI Routing — `client/src/pages/GamePage.tsx`

Conditional rendering based on `waitingFor.type` + `playerId` check:

```tsx
{(waitingFor?.type === "TargetSelection" ||
  waitingFor?.type === "TriggerTargetSelection") &&
  waitingFor.data.player === playerId && <TargetingOverlay />}
<ModeChoiceModal />
<CardChoiceModal />
{waitingFor?.type === "ReplacementChoice" &&
  waitingFor.data.player === playerId && <ReplacementModal />}
```

**All overlays gate on `waitingFor.data.player === playerId`** to prevent the wrong player from seeing choices in multiplayer.

---

## Checklist — Adding a New Frontend Component

### Phase 1 — TypeScript Types

- [ ] **`client/src/adapter/types.ts` — `WaitingFor` union** (if new interactive state)
  Add a variant matching the Rust `WaitingFor` enum. Must match the serde output format exactly:
  ```typescript
  | { type: "YourChoice"; data: { player: PlayerId; cards: ObjectId[]; /* ... */ } }
  ```
  The `player` field is required — it gates UI visibility.

- [ ] **`client/src/adapter/types.ts` — `GameAction` union** (if new response type)
  Add the response variant. Reuse `SelectCards` if the response is just card IDs.
  ```typescript
  | { type: "YourResponse"; data: { selection: /* ... */ } }
  ```

- [ ] **`client/src/adapter/types.ts` — `GameEvent` union** (if new event for log/animation)
  ```typescript
  | { type: "YourEvent"; data: { /* event payload */ } }
  ```

- [ ] **`client/src/adapter/types.ts` — `GameObject` interface** (if new fields on objects)
  Add optional fields with `?:` to avoid breaking existing state deserialization.

### Phase 2 — Component Implementation

Three common patterns for new components:

#### Pattern A: Card Choice Overlay (most interactive effects)

Used by: Scry, Dig, Surveil, Reveal, Search, DiscardToHandSize.

```tsx
// client/src/components/modal/YourOverlay.tsx
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { useGameDispatch } from "../../hooks/useGameDispatch";

export function YourOverlay({ data }: { data: YourChoiceData }) {
  const objects = useGameStore((s) => s.gameState?.objects ?? {});
  const inspectObject = useUiStore((s) => s.inspectObject);
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const handleConfirm = () => {
    dispatch({ type: "SelectCards", data: { cards: [...selected] } });
  };

  return (
    <AnimatePresence>
      <motion.div
        initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}
        className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      >
        {/* Card display + selection UI */}
        <button onClick={handleConfirm} disabled={!isValid}>Confirm</button>
      </motion.div>
    </AnimatePresence>
  );
}
```

**Key conventions:**
- `useGameStore` for game state (objects, zones)
- `useUiStore.inspectObject` for card preview on hover
- `useGameDispatch()` to send actions
- Framer Motion `AnimatePresence` for entry/exit animations
- Dark theme: `bg-gray-900`, `ring-1 ring-gray-700`, accent colors (cyan, amber, emerald)
- `fixed inset-0 z-50` for full-screen overlay backdrop

#### Pattern B: Simple Choice Modal

Used by: Mulligan, Play/Draw, modal spells.

```tsx
<ChoiceModal
  title="Choose an option"
  options={[
    { id: "opt1", label: "Option A", description: "Does X" },
    { id: "opt2", label: "Option B", description: "Does Y" },
  ]}
  onChoose={(id) => dispatch({ type: "YourResponse", data: { choice: id } })}
/>
```

#### Pattern C: Board Element

For non-overlay components (new zone display, counter indicators, status badges):
- Place in the relevant subdirectory (`components/board/`, `components/zone/`, etc.)
- Subscribe to `useGameStore` for data
- No dispatch needed if read-only

### Phase 3 — GamePage Routing

- [ ] **`client/src/pages/GamePage.tsx` — conditional render**
  Add your overlay to the `GamePageContent` component alongside existing overlays:
  ```tsx
  {waitingFor?.type === "YourChoice" && waitingFor.data.player === playerId && (
    <YourOverlay data={waitingFor.data} />
  )}
  ```
If your overlay is a card choice type, integrate into the existing `CardChoiceModal` switch instead of adding a new top-level conditional.

### Phase 4 — Animation Integration (if applicable)

- [ ] **`client/src/animation/eventNormalizer.ts`** — Event grouping
  If your new `GameEvent` should trigger visual effects:
  - Add to `OWN_STEP_TYPES` if it should always start a new animation step
  - Add to `MERGE_TYPES` if it should merge into the preceding step
  - Add duration to `EVENT_DURATIONS` in `animation/types.ts`

- [ ] **`client/src/components/animation/AnimationOverlay.tsx`** — Visual effect
  Add rendering for your event type if it needs VFX (particles, arcs, screen effects).

### Phase 5 — Game Log (if applicable)

- [ ] **`client/src/viewmodel/logFormatting.ts`** — Event formatting
  Add a case for your `GameEvent` type to produce a human-readable log string.

- [ ] **`client/src/components/log/LogEntry.tsx`** — Custom rendering (if needed)
  Most events use the default text format. Only add custom rendering for events that need icons, card references, or special formatting.

### Phase 6 — Multiplayer Considerations

- [ ] **Player gating** — Every interactive overlay MUST check `waitingFor.data.player === playerId`. Without this, both players see the choice UI.

- [ ] **State filtering** — If the component displays hidden information (opponent's hand, library cards), ensure the server-side filter in `crates/server-core/src/filter.rs` correctly hides/reveals cards. The frontend should display whatever the filtered state contains — don't add client-side visibility logic.

---

## Component Directory Reference

| Directory | Purpose | Examples |
|-----------|---------|---------|
| `components/modal/` | Interactive overlays for WaitingFor states | CardChoiceModal, ModeChoiceModal, ReplacementModal, ChoiceModal, NamedChoiceModal, BattleProtectorModal, TributeModal, CombatTaxModal |
| `components/board/` | Battlefield elements | PermanentCard, GameBoard, PlayerArea, CommandDisplay |
| `components/card/` | Card rendering | CardImage, CardPreview, ArtCropCard |
| `components/combat/` | Combat interaction | AttackerControls, BlockerControls, DamageAssignmentModal |
| `components/controls/` | Game controls | PhaseTracker, PassButton, LifeTotal |
| `components/hand/` | Hand display | PlayerHand, OpponentHand |
| `components/hud/` | Player info display | PlayerHud, OpponentHud, ManaPoolSummary |
| `components/zone/` | Zone displays | GraveyardPile, LibraryPile, ZoneViewer |
| `components/stack/` | Stack display | StackDisplay, StackEntry |
| `components/targeting/` | Target selection | TargetingOverlay, TargetArrow |
| `components/animation/` | Visual effects | AnimationOverlay, DamageFloat, DeathShatter |
| `components/log/` | Game event log | GameLog, LogEntry, GameLogPanel |
| `components/lobby/` | Multiplayer lobby | GameList, ReadyRoom, HostSetup |

---

## Styling Conventions

- **Tailwind CSS v4** — utility classes, no CSS modules
- **Dark theme**: `bg-gray-900` base, `bg-gray-800` cards, `ring-1 ring-gray-700` borders
- **Accent colors**: Cyan (`text-cyan-400`) for info, Amber (`text-amber-400`) for warnings, Emerald (`text-emerald-400`) for success, Red (`text-red-400`) for danger
- **Card sizing**: CSS custom properties `--card-w`, `--card-h` (set by preferences store)
- **Animations**: Framer Motion for all transitions. `AnimatePresence` for mount/unmount. Staggered delays: `delay: 0.1 + index * 0.08`
- **Responsive**: `max-w-md` / `max-w-sm` for modals, `inset-0` for full-screen backdrops

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Missing player gate (`waitingFor.data.player === playerId`) | Both players see the overlay in multiplayer | Always check player ID |
| Types don't match Rust serde output | Deserialization silently fails, `waitingFor` is null | Match exact `tag="type", content="data"` format |
| Dispatching action without waiting for animation | State updates before animation completes, visual glitch | Use `useGameDispatch()` which handles the pipeline |
| Adding client-side visibility logic | Diverges from server-filtered state, multiplayer security hole | Trust the filtered state from the adapter |
| Modifying `gameStore` directly | Bypasses animation pipeline and persistence | Always go through `dispatch()` |
| Not using `AnimatePresence` | Overlay appears/disappears instantly | Wrap in `AnimatePresence` with enter/exit transitions |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update directory reference table** if new component directories were added
3. **Update WaitingFor routing section** if new overlay patterns emerged

### Verification

```bash
test -f client/src/adapter/types.ts && \
test -f client/src/stores/gameStore.ts && \
test -f client/src/stores/uiStore.ts && \
test -f client/src/stores/animationStore.ts && \
test -f client/src/pages/GamePage.tsx && \
test -f client/src/game/dispatch.ts && \
test -f client/src/animation/eventNormalizer.ts && \
test -f client/src/components/modal/CardChoiceModal.tsx && \
rg -q "type WaitingFor" client/src/adapter/types.ts && \
rg -q "type GameAction" client/src/adapter/types.ts && \
rg -q "type GameEvent" client/src/adapter/types.ts && \
rg -q "useGameDispatch" client/src/hooks/useGameDispatch.ts && \
echo "✓ add-frontend-component skill references valid" || \
echo "✗ STALE — update skill references"
```
