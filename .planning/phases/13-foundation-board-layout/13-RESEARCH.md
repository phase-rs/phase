# Phase 13: Foundation & Board Layout - Research

**Researched:** 2026-03-08
**Domain:** React UI layout, responsive CSS, drag interaction, Zustand state management
**Confidence:** HIGH

## Summary

Phase 13 replaces the existing functional but basic game UI with an Arena-style full-screen board layout. The current codebase already has most building blocks: CSS custom properties for card sizing (`--card-w`/`--card-h` with media queries), `GameBoard` with three-row type partitioning, `PermanentCard` with counter badges and attachment rendering, `PlayerHand` with Framer Motion animations, `LifeTotal` with motion-value animation, `GameLog` with event formatting, and the `ChoiceModal` pattern for overlays. The existing stores (`gameStore` with `subscribeWithSelector`, `uiStore` for ephemeral state, `animationStore`) establish patterns the new `preferencesStore` follows.

The primary work is restructuring `GamePage.tsx` from a side-panel layout to full-screen, enhancing `PlayerHand` with fan-from-bottom + drag-to-play, adding P/T display boxes with color-coded modified values, implementing token/land grouping with count badges, creating the view model layer, building the slide-out log panel, zone viewer modals, and the preferences store with Zustand `persist` middleware.

**Primary recommendation:** Build incrementally on the existing component structure -- restructure layout first, then enhance components in-place, add the view model layer, and wire preferences last.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Three-row battlefield grouped by type: creatures top, non-creature permanents middle, lands bottom -- MTGA style
- CSS custom properties for responsive card sizing: `--card-w` / `--card-h` scaling with viewport via media queries (desktop ~7vw, tablet ~12vw, mobile ~18vw)
- Same-name tokens stack with count badge, expandable on click to show individuals
- Same-name lands group together with count badge, same expand behavior
- Auras/equipment tuck behind host permanent with top edge visible, click host to see full attachments
- Tapped permanents rotate 90 degrees clockwise, no opacity change -- matches Arena exactly
- WUBRG-based battlefield background auto-selection from Forge assets based on player's dominant mana color, with neutral fallback for multicolor
- Arena-style P/T box showing MODIFIED values (not base), green when buffed, red when damaged/debuffed
- Small counter badge separately showing +1/+1 or -1/-1 count
- Loyalty displayed in shield badge on planeswalkers
- MTGA-style peek fan from bottom edge: cards peek in collapsed state, expand upward on hover
- Drag-to-play with 50px minimum drag threshold to prevent accidental plays
- Playable cards highlighted with green glow border based on legal actions from engine
- Non-playable cards slightly dimmed
- Hover preview shows full-size card image to the side
- Opponent hand: compact card backs fanned horizontally at top edge
- Touch: tap hand zone to expand, tap card to select (shows preview), tap again or drag to play, long-press for full preview, tap elsewhere to deselect & collapse
- Full-screen board with no permanent side panel -- board fills entire viewport
- Player HUD inline with hand zone at bottom, opponent HUD inline at top
- Phase indicator between the two battlefields (divider area)
- Game log: slide-out panel from right edge with toggle button, overlays board when open
- Zone viewers (graveyard, exile): modal overlays with scrollable card grid
- Settings gear icon in player HUD opens preferences modal during gameplay
- HUD positioning built as standalone component -- architecturally flexible for layout toggle
- Color-coded log entries by event type: combat (red), spells (blue/purple), life changes (green/red), zone changes (gray)
- Verbosity filter with three levels: full, compact, minimal
- New dedicated `preferencesStore` (Zustand with `persist` middleware to localStorage)
- Separate from `uiStore` (ephemeral)
- Settings: card size scaling, HUD layout mode, game log default state, board background theme

### Claude's Discretion
- Exact CSS custom property values and breakpoint thresholds
- Card aspect ratio and border radius
- Counter badge exact positioning and sizing
- Drag animation easing and snap-back behavior
- Log panel width and animation timing
- Settings modal layout and control types
- View model function signatures and selector granularity
- Hybrid view model approach: pure mapping functions for data transformation consumed via lightweight Zustand selectors for reactivity

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| BOARD-01 | CSS custom properties for responsive card sizing | Existing `--card-w`/`--card-h` in index.css; update values to vw-based per CONTEXT decisions |
| BOARD-02 | Multi-row layout grouped by type | Existing `GameBoard` + `partitionByType` + `BattlefieldRow`; enhance with proper MTGA ordering |
| BOARD-03 | Same-name tokens stack with count badge | New grouping logic in view model; `GroupedPermanent` component wrapping `PermanentCard` |
| BOARD-04 | Same-name lands group with count badge | Same grouping mechanism as BOARD-03, applied to lands row |
| BOARD-05 | Tapped permanents at 90 degree rotation | Existing `CardImage` has `tapped` prop using 30deg; change to 90deg per Arena spec |
| BOARD-06 | Auras/equipment attached to host | Existing `PermanentCard` renders `attachments` array; refine visual tuck layout |
| BOARD-07 | Counter overlays on permanents | Existing counter badges in `PermanentCard`; add P/T box with color coding |
| BOARD-08 | Persistent damage number on creatures | Existing damage overlay in `PermanentCard`; integrate with P/T box display |
| BOARD-09 | WUBRG battlefield backgrounds | New feature; compute dominant color from player's lands, apply background image/gradient |
| HAND-01 | MTGA-style fan layout with peek/expand | Existing `PlayerHand` with hover-lift; restructure to bottom-edge fan with collapsed peek state |
| HAND-02 | Drag-to-play with threshold | New: add Framer Motion `drag` prop + `onDragEnd` with 50px threshold check |
| HAND-03 | Playable cards highlight from legal actions | Existing white glow on priority; refine to green glow based on `WaitingFor.Priority` legal actions |
| HAND-04 | Opponent hand card backs | Existing `OpponentHand` component; already functional, refine styling |
| HUD-01 | Player HUD with life, mana, phase | Existing `LifeTotal` + `PhaseTracker`; compose into inline HUD component |
| HUD-02 | Opponent HUD with life and mana | Same pattern as HUD-01 for opponent side |
| HUD-03 | Life total animation on change | Existing `LifeTotal` has motion animation; add red/green flash colors |
| ZONE-01 | Graveyard viewer modal | New modal using `ChoiceModal` pattern; fetch objects from `player.graveyard` |
| ZONE-02 | Exile zone viewer | Same pattern as ZONE-01 for `gameState.exile` |
| ZONE-03 | Zone card counts on indicators | New inline badges showing count; clickable to open viewer |
| LOG-01 | Scrollable game log panel | Existing `GameLog` component; restructure as slide-out panel from right edge |
| LOG-02 | Color-coded log entries | Existing `formatEvent` function; add color classification per event type |
| LOG-03 | Log verbosity filtering | New: add filter state to log component, three verbosity levels |
| INTEG-01 | All UI through EngineAdapter | Existing pattern; all components already use `gameStore` which wraps adapter |
| INTEG-02 | GameObject view model | New: pure mapping functions `toCardProps()`, `toBattlefieldProps()` etc. with Zustand selectors |
| INTEG-03 | Preferences store with localStorage | New: Zustand store with `persist` middleware |
</phase_requirements>

## Standard Stack

### Core (already installed)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| React | ^19.0.0 | UI framework | Already in project |
| Zustand | ^5.0.11 | State management | Already in project, `subscribeWithSelector` + `persist` middlewares |
| Framer Motion | ^12.35.1 | Animations & drag gestures | Already in project, provides `drag`, `useDragControls`, `AnimatePresence` |
| Tailwind CSS | ^4.2.1 | Styling | Already in project, CSS custom properties integrate naturally |
| React Router | ^7.13.1 | Routing | Already in project |

### Supporting (already installed)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| idb-keyval | ^6.2.2 | IndexedDB caching | Card image cache (existing) |
| Vitest | ^3.0.0 | Testing | All component/store tests |
| @testing-library/react | ^16.3.0 | React testing | Component behavior tests |

### No new dependencies needed
All requirements can be fulfilled with the existing stack. Framer Motion covers drag gestures, Zustand covers persist middleware, Tailwind + CSS custom properties covers responsive layout.

## Architecture Patterns

### Current Project Structure (relevant parts)
```
client/src/
  adapter/          # EngineAdapter + types.ts (GameObject, GameState, etc.)
  components/
    board/          # GameBoard, BattlefieldRow, PermanentCard
    card/           # CardImage, CardPreview
    hand/           # PlayerHand, OpponentHand
    controls/       # LifeTotal, PhaseTracker, PassButton, FullControlToggle
    log/            # GameLog
    modal/          # ChoiceModal, CardDataMissingModal, ReplacementModal
    combat/         # CombatOverlay
    targeting/      # TargetingOverlay
    mana/           # ManaPaymentUI
    stack/          # StackDisplay, StackEntry
  hooks/            # useCardImage, useGameDispatch, useKeyboardShortcuts, useLongPress
  stores/           # gameStore, uiStore, animationStore
  pages/            # GamePage, MenuPage, DeckBuilderPage
  services/         # scryfall, imageCache, deckParser
```

### New/Modified Structure
```
client/src/
  components/
    board/          # GameBoard (restructured), BattlefieldRow, PermanentCard (enhanced), GroupedPermanent (new)
    card/           # CardImage (fix 90deg tap), CardPreview (position-aware)
    hand/           # PlayerHand (fan+drag), OpponentHand (refined)
    hud/            # PlayerHud (new), OpponentHud (new), ManaPoolSummary (new), LifeTotal (moved+enhanced)
    log/            # GameLogPanel (slide-out), LogEntry (color-coded), LogFilter (new)
    zone/           # ZoneViewer (new), ZoneIndicator (new)
    settings/       # PreferencesModal (new)
  stores/
    preferencesStore.ts  # NEW - Zustand with persist middleware
  viewmodel/        # NEW - pure mapping functions + selector helpers
```

### Pattern 1: View Model Layer (Hybrid Approach)
**What:** Pure functions that map deep engine `GameObject` to flat component props, consumed through Zustand selectors.
**When to use:** Every component that reads from game state.
**Example:**
```typescript
// viewmodel/cardProps.ts - Pure mapping function
interface CardViewProps {
  id: number;
  name: string;
  tapped: boolean;
  power: number | null;
  toughness: number | null;
  basePower: number | null;
  baseToughness: number | null;
  damageMarked: number;
  isPowerBuffed: boolean;
  isPowerDebuffed: boolean;
  isToughnessBuffed: boolean;
  isToughnessDebuffed: boolean;
  effectiveToughness: number | null; // toughness - damage_marked
  counters: Array<{ type: string; count: number }>;
  loyalty: number | null;
  isCreature: boolean;
  isLand: boolean;
  attachedTo: number | null;
  attachmentIds: number[];
  keywords: string[];
  colorIdentity: string[];
}

export function toCardProps(obj: GameObject): CardViewProps {
  const isPowerBuffed = obj.power !== null && obj.base_power !== null && obj.power > obj.base_power;
  const isPowerDebuffed = obj.power !== null && obj.base_power !== null && obj.power < obj.base_power;
  // ... etc
  return { /* flat props */ };
}

// viewmodel/battlefieldProps.ts
interface GroupedPermanents {
  name: string;
  ids: number[];
  count: number;
  representative: CardViewProps;
}

export function groupByName(objects: GameObject[]): GroupedPermanents[] {
  // Group same-name objects, return representative + count
}

// Usage in component via Zustand selector:
const playerCreatures = useGameStore((s) => {
  if (!s.gameState) return [];
  const objects = s.gameState.battlefield
    .map(id => s.gameState!.objects[id])
    .filter(obj => obj && obj.controller === 0 && obj.card_types.core_types.includes("Creature"));
  return groupByName(objects);
});
```

### Pattern 2: Preferences Store with Persist
**What:** Dedicated Zustand store with `persist` middleware for user preferences that survive sessions.
**When to use:** Any setting the user configures that should persist across page reloads.
**Example:**
```typescript
// stores/preferencesStore.ts
import { create } from "zustand";
import { persist } from "zustand/middleware";

type CardSizePreference = "small" | "medium" | "large";
type HudLayout = "inline" | "floating";
type LogDefaultState = "open" | "closed";
type BoardBackground = "auto-wubrg" | "white" | "blue" | "black" | "red" | "green" | "none";

interface PreferencesState {
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
}

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      cardSize: "medium",
      hudLayout: "inline",
      logDefaultState: "closed",
      boardBackground: "auto-wubrg",
      setCardSize: (cardSize) => set({ cardSize }),
      setHudLayout: (hudLayout) => set({ hudLayout }),
      setLogDefaultState: (logDefaultState) => set({ logDefaultState }),
      setBoardBackground: (boardBackground) => set({ boardBackground }),
    }),
    { name: "forge-preferences" },
  ),
);
```

### Pattern 3: Slide-Out Panel
**What:** Right-edge overlay panel for game log using Framer Motion's `animate` + absolute positioning.
**When to use:** Game log display that overlays the board when open.
**Example:**
```typescript
// Slide-out panel pattern with Framer Motion
<AnimatePresence>
  {isLogOpen && (
    <motion.div
      className="fixed right-0 top-0 bottom-0 z-30 w-80 bg-gray-900/95 border-l border-gray-700 shadow-2xl"
      initial={{ x: "100%" }}
      animate={{ x: 0 }}
      exit={{ x: "100%" }}
      transition={{ type: "spring", damping: 25, stiffness: 300 }}
    >
      {/* Log content */}
    </motion.div>
  )}
</AnimatePresence>
```

### Pattern 4: Drag-to-Play Hand Card
**What:** Framer Motion `drag` on hand cards with threshold check and snap-back.
**When to use:** Player hand interaction.
**Example:**
```typescript
// Hand card with drag-to-play
const DRAG_THRESHOLD = 50;

<motion.div
  drag="y"
  dragConstraints={{ top: -200, bottom: 0 }}
  dragElastic={0.3}
  dragSnapToOrigin
  onDragEnd={(_e, info) => {
    if (Math.abs(info.offset.y) > DRAG_THRESHOLD && isPlayable) {
      handlePlayCard(obj);
    }
  }}
  // Visual feedback during drag
  whileDrag={{ scale: 1.05, zIndex: 50 }}
>
  <CardImage cardName={obj.name} size="small" />
</motion.div>
```

### Anti-Patterns to Avoid
- **Reading raw `GameObject` in components:** Always go through view model functions. Components receive flat props, never navigate `obj.card_types.core_types.includes("Creature")` directly.
- **Storing preferences in `uiStore`:** `uiStore` is ephemeral (resets on page reload). Preferences must go in `preferencesStore` with persist middleware.
- **Inline event color logic in `GameLog`:** Extract color classification into a pure function in viewmodel layer for testability.
- **Fixed pixel card sizes:** Always use CSS custom properties (`--card-w`/`--card-h`). Never hardcode pixel dimensions for cards.
- **Blocking renders on image loading:** Card images load async via `useCardImage`. Always show a placeholder skeleton during load.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Drag gesture detection | Custom pointer event tracking | Framer Motion `drag` prop | Handles touch/mouse, momentum, snap-back, constraints automatically |
| State persistence | Manual `localStorage.getItem/setItem` + `useEffect` sync | Zustand `persist` middleware | Handles hydration, serialization, version migration, merge strategies |
| Slide panel animation | CSS transitions with state toggle | Framer Motion `AnimatePresence` + `motion.div` | Exit animations, spring physics, interruptible transitions |
| Responsive card sizing | JavaScript resize observer + state updates | CSS custom properties + media queries | Zero JS overhead, browser-native, already established in project |
| Card image loading | Manual fetch + blob URL management | Existing `useCardImage` hook + `imageCache.ts` | Already handles Scryfall rate limiting, IndexedDB caching, cleanup |

**Key insight:** The existing codebase already solves the hardest infrastructure problems (image loading pipeline, engine communication, store patterns). This phase is primarily layout restructuring and component enhancement.

## Common Pitfalls

### Pitfall 1: Tap Rotation Clipping
**What goes wrong:** 90-degree rotation causes the card to overflow its container and overlap neighbors.
**Why it happens:** A card rotated 90 degrees needs width-of-height space, but the container only allocates width-of-width.
**How to avoid:** Add extra horizontal margin/padding to tapped cards. Use `transform-origin: center center` and account for the rotated bounding box in the flex layout. Consider wrapping tapped cards in a container sized to the rotated dimensions.
**Warning signs:** Tapped cards overlapping adjacent cards or getting clipped by container overflow.

### Pitfall 2: Zustand Selector Equality
**What goes wrong:** View model selectors that return new arrays/objects on every call trigger unnecessary re-renders.
**Why it happens:** Zustand uses reference equality by default. `useGameStore(s => s.battlefield.map(...))` creates a new array every time.
**How to avoid:** Use `useShallow` from `zustand/react/shallow` for selectors that return derived arrays/objects, or memoize in the component with `useMemo`. For the view model pattern, compute derived state in selectors with stable references.
**Warning signs:** Components re-rendering on every store update even when their data hasn't changed.

### Pitfall 3: Drag-to-Play vs Click-to-Play Conflict
**What goes wrong:** Drag gesture triggers on what user intended as a click, or click fires after drag cancels.
**Why it happens:** Pointer events fire both drag start and click. Without threshold filtering, every interaction is ambiguous.
**How to avoid:** The 50px drag threshold is the right call. Additionally, use `dragSnapToOrigin` for cancelled drags and only dispatch the play action in `onDragEnd` when threshold is exceeded. Framer Motion's `onDragEnd` provides `info.offset` for exact distance measurement.
**Warning signs:** Cards playing when user only meant to inspect them, or previews showing when user meant to drag.

### Pitfall 4: Persist Middleware Hydration Flash
**What goes wrong:** On first render, preferences store has defaults, then hydrates from localStorage causing a visual flash.
**Why it happens:** Zustand `persist` with synchronous storage (localStorage) does hydrate synchronously at store creation time, but if there's any async rendering, the component may render with defaults first.
**How to avoid:** Zustand's localStorage persist is synchronous -- the store is hydrated before any component reads it. But if using async storage, wrap with `onRehydrateStorage` callback. For this project, localStorage is synchronous so this is not a concern.
**Warning signs:** Preferences reverting to defaults briefly on page load.

### Pitfall 5: Game Log Event Accumulation
**What goes wrong:** The current `gameStore.events` only holds the latest batch from the most recent dispatch. Historical events are lost.
**Why it happens:** `dispatch()` sets `events` to the new batch, replacing the previous. The game log needs a cumulative history.
**How to avoid:** Introduce an `eventLog` accumulator (array of all events) either in `gameStore` or a separate store/ref. Append new events after each dispatch. Consider capping at a reasonable size (e.g., 500 entries) to prevent memory growth.
**Warning signs:** Log showing only the last action's events, clearing on each new action.

### Pitfall 6: Token/Land Grouping with Attachments
**What goes wrong:** Grouping same-name permanents into stacks fails when some have unique state (different attachments, different counters, different tap state).
**Why it happens:** Grouping by name alone ignores state differences that make cards visually distinct.
**How to avoid:** Only group permanents that share the same name AND same tapped state AND have no attachments AND have no counters. If any permanent in a potential group has unique state, show it individually. The group count badge represents truly identical permanents only.
**Warning signs:** Grouped tokens where some are tapped and some aren't, or grouped lands where some have auras attached.

## Code Examples

### CSS Custom Properties (vw-based responsive sizing)
```css
/* index.css - Updated per CONTEXT decisions */
:root {
  --card-w: 18vw;    /* mobile */
  --card-h: calc(18vw * 1.4);
  --card-radius: 6px;
}

@media (min-width: 768px) {
  :root {
    --card-w: 12vw;    /* tablet */
    --card-h: calc(12vw * 1.4);
  }
}

@media (min-width: 1200px) {
  :root {
    --card-w: 7vw;     /* desktop */
    --card-h: calc(7vw * 1.4);
    --card-radius: 8px;
  }
}
```

### P/T Box with Color Coding
```typescript
// Derived from GameObject fields already available:
// obj.power (modified), obj.base_power, obj.toughness (modified), obj.base_toughness, obj.damage_marked

interface PTDisplay {
  power: number;
  toughness: number;          // effective: toughness - damage_marked
  powerColor: "green" | "red" | "white";
  toughnessColor: "green" | "red" | "white";
}

function computePTDisplay(obj: GameObject): PTDisplay | null {
  if (obj.power === null || obj.toughness === null) return null;
  if (obj.base_power === null || obj.base_toughness === null) return null;

  const effectiveToughness = obj.toughness - obj.damage_marked;

  const powerColor = obj.power > obj.base_power ? "green"
    : obj.power < obj.base_power ? "red"
    : "white";

  const toughnessColor = obj.damage_marked > 0 ? "red"
    : obj.toughness > obj.base_toughness ? "green"
    : obj.toughness < obj.base_toughness ? "red"
    : "white";

  return {
    power: obj.power,
    toughness: effectiveToughness,
    powerColor,
    toughnessColor,
  };
}
```

### WUBRG Dominant Color Detection
```typescript
// Compute from player's lands on battlefield
function getDominantManaColor(
  battlefieldIds: number[],
  objects: Record<string, GameObject>,
  playerId: number,
): ManaColor | null {
  const colorCounts: Record<string, number> = {};

  for (const id of battlefieldIds) {
    const obj = objects[id];
    if (!obj || obj.controller !== playerId) continue;
    if (!obj.card_types.core_types.includes("Land")) continue;

    for (const color of obj.color) {
      colorCounts[color] = (colorCounts[color] ?? 0) + 1;
    }
  }

  let maxColor: string | null = null;
  let maxCount = 0;
  for (const [color, count] of Object.entries(colorCounts)) {
    if (count > maxCount) {
      maxColor = color;
      maxCount = count;
    }
  }

  return maxColor as ManaColor | null;
}
```

### Mana Pool Summary Display
```typescript
// Player.mana_pool.mana is ManaUnit[] - summarize by color
function summarizeManaPool(pool: ManaPool): Record<ManaType, number> {
  const summary: Record<string, number> = {};
  for (const unit of pool.mana) {
    summary[unit.color] = (summary[unit.color] ?? 0) + 1;
  }
  return summary as Record<ManaType, number>;
}
```

### Full-Screen Layout Structure
```tsx
// GamePage restructured layout (conceptual)
<div className="relative h-screen w-screen overflow-hidden bg-gray-950">
  {/* Opponent area: HUD + hand */}
  <div className="flex items-center border-b border-gray-800">
    <OpponentHud />
    <OpponentHand />
  </div>

  {/* Opponent battlefield */}
  <Battlefield playerId={1} />

  {/* Center divider with phase indicator */}
  <div className="flex items-center justify-center py-1">
    <PhaseIndicator />
    <ZoneIndicators />
  </div>

  {/* Player battlefield */}
  <Battlefield playerId={0} />

  {/* Player area: HUD + hand */}
  <div className="flex items-center border-t border-gray-800">
    <PlayerHud />
    <PlayerHand />
  </div>

  {/* Overlay layers */}
  <GameLogPanel />      {/* slide-out from right */}
  <ZoneViewerModal />   {/* modal overlay */}
  <PreferencesModal />  {/* modal overlay */}
  {/* ... existing overlays (targeting, combat, mana, mulligan) ... */}
</div>
```

## State of the Art

| Old Approach (current) | Current Approach (target) | Impact |
|-------------------------|---------------------------|--------|
| Side panel layout with fixed 256px panel | Full-screen board, slide-out log | More board space, matches Arena |
| `CardImage tapped` uses 30deg rotation | 90deg rotation per Arena spec | Visual accuracy |
| White glow on all cards when has priority | Green glow only on playable cards | Better affordance |
| Events array replaced each dispatch | Cumulative event log | Full game history in log |
| No preferences persistence | Zustand persist to localStorage | Settings survive sessions |
| Components read raw `GameObject` | View model functions produce flat props | Better separation, testability |
| Static card sizes per breakpoint (px) | vw-based sizing with CSS custom properties | Smoother responsive behavior |

**Existing code to modify (not replace):**
- `GamePage.tsx` -- restructure layout, keep all overlay logic
- `GameBoard.tsx` -- add grouping logic, split into per-player battlefields
- `PermanentCard.tsx` -- add P/T box, fix tap rotation to 90deg
- `PlayerHand.tsx` -- add fan layout, drag-to-play, playable highlighting
- `CardImage.tsx` -- fix tapped rotation from 30deg to 90deg
- `GameLog.tsx` -- restructure as slide-out panel, add color coding + verbosity filter
- `LifeTotal.tsx` -- add red/green flash on damage/gain
- `index.css` -- update card sizing to vw-based values

## Open Questions

1. **Event log accumulation strategy**
   - What we know: Current `gameStore.events` only holds the latest batch. The game log needs cumulative history.
   - What's unclear: Should the accumulated log live in `gameStore` (simple but couples concerns) or a separate `logStore` (clean separation)?
   - Recommendation: Add an `eventHistory: GameEvent[]` field to `gameStore` that appends on each dispatch. Simpler and the store already owns events. Cap at 1000 entries.

2. **Legal actions for playable card highlighting**
   - What we know: `WaitingFor.Priority` tells us we have priority, but doesn't list which specific cards are playable.
   - What's unclear: Does the engine expose legal actions for the current player? The WASM adapter may not surface this.
   - Recommendation: For initial implementation, highlight all hand cards when player has priority (current behavior). Legal action filtering may require engine-side support that can be added in a later phase.

3. **Battlefield background images source**
   - What we know: CONTEXT says "Forge assets" for WUBRG backgrounds. The Forge Java project has background images.
   - What's unclear: Are these assets already available in the client, or do they need to be sourced/added?
   - Recommendation: Start with CSS gradient backgrounds per color (no external assets needed). Add actual Forge background images as a refinement if assets are available.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest 3.x + @testing-library/react 16.x |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && pnpm test -- --run` |
| Full suite command | `cd client && pnpm test -- --run --coverage` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| BOARD-01 | Card sizing uses CSS custom properties | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/cardSizing.test.ts` | No - Wave 0 |
| BOARD-02 | Battlefield groups permanents by type | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/battlefieldGrouping.test.ts` | No - Wave 0 |
| BOARD-03 | Same-name tokens group with count | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/permanentGrouping.test.ts` | No - Wave 0 |
| BOARD-04 | Same-name lands group with count | unit | Same as BOARD-03 | No - Wave 0 |
| BOARD-05 | Tapped permanents at 90deg | unit | `cd client && pnpm test -- --run src/components/card/__tests__/CardImage.test.tsx` | No - Wave 0 |
| BOARD-07 | Counter overlays with P/T box | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/ptDisplay.test.ts` | No - Wave 0 |
| BOARD-08 | Damage display on creatures | unit | Same as BOARD-07 | No - Wave 0 |
| BOARD-09 | WUBRG background selection | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/dominantColor.test.ts` | No - Wave 0 |
| HAND-03 | Playable card highlighting | unit | `cd client && pnpm test -- --run src/components/hand/__tests__/PlayerHand.test.tsx` | No - Wave 0 |
| HUD-03 | Life total flash animation | unit | `cd client && pnpm test -- --run src/components/hud/__tests__/LifeTotal.test.tsx` | No - Wave 0 |
| LOG-02 | Color-coded log entries | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/logFormatting.test.ts` | No - Wave 0 |
| LOG-03 | Log verbosity filtering | unit | Same as LOG-02 | No - Wave 0 |
| INTEG-02 | View model mapping | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/cardProps.test.ts` | No - Wave 0 |
| INTEG-03 | Preferences persistence | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | No - Wave 0 |
| BOARD-06 | Aura/equipment visual attachment | manual-only | Visual inspection | N/A |
| HAND-01 | Fan layout from bottom edge | manual-only | Visual inspection + interaction | N/A |
| HAND-02 | Drag-to-play with threshold | manual-only | Interaction testing | N/A |
| HAND-04 | Opponent card backs | manual-only | Visual inspection | N/A |
| HUD-01 | Player HUD displays life/mana/phase | smoke | Visual inspection | N/A |
| HUD-02 | Opponent HUD displays life/mana | smoke | Visual inspection | N/A |
| ZONE-01 | Graveyard viewer modal | smoke | Visual inspection + click | N/A |
| ZONE-02 | Exile viewer modal | smoke | Visual inspection + click | N/A |
| ZONE-03 | Zone count indicators | smoke | Visual inspection | N/A |
| LOG-01 | Scrollable log panel | smoke | Visual inspection | N/A |
| INTEG-01 | All UI through EngineAdapter | integration | Existing adapter tests cover | Yes |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cd client && pnpm test -- --run --coverage`
- **Phase gate:** Full suite green + `pnpm lint` + `pnpm run type-check` before verify

### Wave 0 Gaps
- [ ] `client/src/viewmodel/__tests__/cardProps.test.ts` -- covers INTEG-02
- [ ] `client/src/viewmodel/__tests__/battlefieldGrouping.test.ts` -- covers BOARD-02, BOARD-03, BOARD-04
- [ ] `client/src/viewmodel/__tests__/ptDisplay.test.ts` -- covers BOARD-07, BOARD-08
- [ ] `client/src/viewmodel/__tests__/dominantColor.test.ts` -- covers BOARD-09
- [ ] `client/src/viewmodel/__tests__/logFormatting.test.ts` -- covers LOG-02, LOG-03
- [ ] `client/src/stores/__tests__/preferencesStore.test.ts` -- covers INTEG-03

## Sources

### Primary (HIGH confidence)
- **Codebase analysis** -- All existing files read directly: `GamePage.tsx`, `GameBoard.tsx`, `PermanentCard.tsx`, `PlayerHand.tsx`, `OpponentHand.tsx`, `LifeTotal.tsx`, `GameLog.tsx`, `CardImage.tsx`, `CardPreview.tsx`, `gameStore.ts`, `uiStore.ts`, `animationStore.ts`, `adapter/types.ts`, `index.css`, `package.json`, `vitest.config.ts`
- **Zustand persist middleware** -- [Official docs](https://zustand.docs.pmnd.rs/reference/middlewares/persist) -- Synchronous localStorage hydration, `partialize`, `name` config
- **Framer Motion drag** -- [Official docs](https://motion.dev/docs/react-drag) -- `drag`, `dragConstraints`, `dragSnapToOrigin`, `onDragEnd` with `info.offset`
- **Framer Motion useDragControls** -- [Official docs](https://motion.dev/docs/react-use-drag-controls) -- Manual drag initiation, `snapToCursor`, touch support

### Secondary (MEDIUM confidence)
- MTGA visual reference for layout decisions -- Based on CONTEXT.md user direction and common knowledge of Arena UI

### Tertiary (LOW confidence)
- Battlefield background images from Forge assets -- Unclear if assets are available in this project; gradient fallback recommended

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- All libraries already installed and verified in package.json
- Architecture: HIGH -- Building on established patterns visible in codebase
- Pitfalls: HIGH -- Derived from actual code analysis (e.g., 30deg tap rotation, events replacement pattern)
- View model pattern: MEDIUM -- Hybrid approach is Claude's discretion; pure functions + selectors is well-established Zustand pattern
- WUBRG backgrounds: LOW -- Asset availability unclear

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable, no fast-moving dependencies)
